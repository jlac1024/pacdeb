//! Turns a .deb into the neutral package model, recording every change and warning.

mod deps;
mod desktop;
mod elf;
mod fs;
mod pathutil;
mod scripts;
mod soname;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Read, Seek};
use std::path::Path;

use crate::deb::{Deb, EntryKind, MAINTAINER_SCRIPTS};
use crate::error::{Result, bail};
use crate::model::{Node, NodeKind, Package};
use crate::version::{ArchVersion, DebVersion};

pub use deps::DepMap;
pub use desktop::DesktopCheck;
pub use scripts::{Action, Command as ScriptCommand, Outcome};
pub use soname::{Lookup, SonameReport, pacman_lookup};

/// Text files bigger than this are not read for checks; real desktop files and cron
/// jobs are a few KiB.
const TEXT_LIMIT: u64 = 1 << 20;

pub struct Translation {
    pub package: Package,
    /// Things Ferry changed on the way, for the record.
    pub changes: Vec<String>,
    /// Things worth knowing that need no action.
    pub notes: Vec<String>,
    /// Things a person should look at.
    pub warnings: Vec<String>,
    pub scripts: Vec<ScriptCommand>,
    pub desktop: Vec<DesktopCheck>,
    pub sonames: SonameReport,
    /// Unmapped dependency groups, also listed in warnings.
    pub unmapped: Vec<String>,
}

pub fn translate<R: Read + Seek>(
    deb: &mut Deb<R>,
    depmap: &DepMap,
    pkgrel: u32,
    lookup: impl FnOnce(&[String]) -> Lookup,
    on_system: impl Fn(&str) -> bool,
) -> Result<Translation> {
    let control = &deb.control;
    let name = control.require("Package")?.to_string();
    check_pkgname(&name)?;
    let deb_version = DebVersion::parse(control.require("Version")?)?;
    let deb_arch = control.require("Architecture")?.to_string();
    let arch = map_arch(&deb_arch)?;
    let description = control.description().map(|(s, _)| s.to_string()).unwrap_or_default();
    let url = control.get("Homepage").map(String::from);
    let deps = deps::translate(control, &deb_arch, depmap)?;
    let control_files = deb.control_files.clone();

    let mut entries = Vec::new();
    let mut contents = HashMap::new();
    let mut needed = BTreeSet::new();
    deb.scan_data(|e, r| {
        entries.push(e.clone());
        if e.kind != EntryKind::File {
            return Ok(());
        }
        let wanted_text = e.path.ends_with(".desktop") || e.path.starts_with("/etc/cron");
        if wanted_text && e.size <= TEXT_LIMIT {
            let mut buf = Vec::new();
            r.read_to_end(&mut buf)?;
            contents.insert(e.path.clone(), buf);
        } else if e.size >= 64 {
            let mut magic = [0u8; 4];
            r.read_exact(&mut magic)?;
            if &magic == elf::MAGIC {
                // Reading the whole file is simplest: the dynamic section can sit
                // anywhere and this only runs once per conversion.
                let mut buf = magic.to_vec();
                r.read_to_end(&mut buf)?;
                needed.extend(elf::needed(&buf).unwrap_or_default());
            }
        }
        Ok(())
    })?;

    let fs::FsResult { mut nodes, map, mut changes, mut warnings } = fs::apply(&entries, arch, &contents);

    let mut script_cmds = Vec::new();
    for script in MAINTAINER_SCRIPTS {
        if let Some(body) = control_files.get(script) {
            script_cmds.extend(scripts::analyze(script, &String::from_utf8_lossy(body)));
        }
    }
    apply_actions(&mut nodes, &map, &script_cmds, &mut changes, &mut warnings);
    for c in &script_cmds {
        if let Outcome::Unknown(why) = &c.outcome {
            warnings.push(format!("{} line {}: not translated ({why}): {}", c.script, c.line, c.text));
        }
    }

    changes.extend(fs::fix_modes(&mut nodes));
    ensure_parents(&mut nodes);
    nodes.sort_by(|a, b| a.path.cmp(&b.path));

    let mut backup = Vec::new();
    if let Some(conffiles) = control_files.get("conffiles") {
        for line in String::from_utf8_lossy(conffiles).lines().map(str::trim).filter(|l| !l.is_empty()) {
            // dpkg marks conffiles that should be removed with a flag after the path.
            let path = map.apply(line.split_whitespace().next().unwrap_or(line));
            if nodes.iter().any(|n| n.path == path && n.kind == NodeKind::File) {
                backup.push(path.trim_start_matches('/').to_string());
            } else {
                warnings.push(format!("conffile {path} is not in the package, left out of backup"));
            }
        }
    }

    let (desktop, desktop_warnings) = desktop::check(&nodes, &contents, &on_system);
    warnings.extend(desktop_warnings);

    let bundled: BTreeSet<String> = nodes
        .iter()
        .filter(|n| !n.is_dir())
        .map(|n| pathutil::basename(&n.path).to_string())
        .collect();
    let sonames = soname::report(&needed, &bundled, &deps.depends, lookup);
    for lib in &sonames.missing {
        warnings.push(format!("{lib} is needed by a binary but no Arch package provides it in /usr/lib"));
    }

    for u in &deps.unmapped {
        warnings.push(format!("no Arch name for {u}; left out (add it to depmap.toml if it is needed)"));
    }

    Ok(Translation {
        package: Package {
            version: ArchVersion::from_debian(&deb_version, pkgrel),
            deb_version,
            name,
            arch: arch.to_string(),
            description,
            url,
            license: "custom".to_string(),
            depends: deps.depends,
            optdepends: deps.optdepends,
            backup,
            nodes,
        },
        changes,
        notes: deps.notes,
        warnings,
        scripts: script_cmds,
        desktop,
        sonames,
        unmapped: deps.unmapped,
    })
}

pub fn map_arch(deb_arch: &str) -> Result<&'static str> {
    Ok(match deb_arch {
        "amd64" => "x86_64",
        "arm64" => "aarch64",
        "all" => "any",
        other => bail!("architecture '{other}' is not supported, only amd64, arm64 and all"),
    })
}

fn check_pkgname(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && !name.starts_with(['-', '.'])
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "@._+-".contains(c));
    if !valid {
        bail!("package name '{name}' is not a valid pacman package name");
    }
    Ok(())
}

/// Whether a program or path exists outside the package. Bare names are looked up
/// on PATH the way `which` does.
pub fn on_system(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).exists();
    }
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| dir.join(program).is_file())
    })
}

fn apply_actions(
    nodes: &mut Vec<Node>,
    map: &fs::PathMap,
    cmds: &[ScriptCommand],
    changes: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    for c in cmds {
        let Outcome::Actions(actions) = &c.outcome else {
            continue;
        };
        let at = format!("{} line {}", c.script, c.line);
        for action in actions {
            match action {
                Action::Symlink { link, target } => {
                    let link = map.apply(link);
                    let target = if target.starts_with('/') { map.apply(target) } else { target.clone() };
                    match nodes.iter().find(|n| n.path == link) {
                        Some(n) if n.kind == NodeKind::Symlink(target.clone()) => {}
                        Some(_) => warnings.push(format!("{at}: {link} is already in the package, symlink to {target} skipped")),
                        None => {
                            changes.push(format!("added symlink {link} -> {target} ({at})"));
                            nodes.push(Node { path: link, kind: NodeKind::Symlink(target), mode: 0o777, size: 0, source: None });
                        }
                    }
                }
                Action::Chmod { path, mode } => {
                    let path = map.apply(path);
                    match nodes.iter_mut().find(|n| n.path == path && matches!(n.kind, NodeKind::File | NodeKind::Dir)) {
                        Some(n) if n.mode == *mode => {}
                        Some(n) => {
                            changes.push(format!("{path}: mode {:04o} -> {mode:04o} ({at})", n.mode));
                            n.mode = *mode;
                        }
                        None => warnings.push(format!("{at}: chmod on {path}, which is not in the package")),
                    }
                }
                Action::RemoveOwned { path } => {
                    let path = map.apply(path);
                    if !nodes.iter().any(|n| n.path == path) {
                        warnings.push(format!("{at}: removes {path}, which is not in the package; check by hand"));
                    }
                }
            }
        }
    }
}

/// Adds any missing parent directory so the package lists the full tree.
fn ensure_parents(nodes: &mut Vec<Node>) {
    let mut have: HashSet<String> = nodes.iter().map(|n| n.path.clone()).collect();
    let mut missing = Vec::new();
    for n in nodes.iter() {
        let mut dir = pathutil::parent(&n.path);
        while dir != "/" && have.insert(dir.to_string()) {
            missing.push(dir.to_string());
            dir = pathutil::parent(dir);
        }
    }
    nodes.extend(missing.into_iter().map(|path| Node { path, kind: NodeKind::Dir, mode: 0o755, size: 0, source: None }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deb::testutil::{DebBuilder, TestEntry};
    use std::collections::BTreeMap;
    use std::io::Cursor;

    const POSTINST: &str = "#!/bin/sh
set -e
update-alternatives --install /usr/bin/demo demo /opt/Demo/demo 100
chmod 4755 /opt/Demo/chrome-sandbox
update-desktop-database -q || true
weird-tool --setup
";

    fn demo_deb() -> Deb<Cursor<Vec<u8>>> {
        let elf = elf::tests::fake_elf(&["libnss3.so", "libgtk-3.so.0", "libffmpeg.so", "libc.so.6", "libmystery.so.1"]);
        let bytes = DebBuilder::new(
            "Package: demo\nVersion: 2:1.0~rc1-3\nArchitecture: amd64\nHomepage: https://example.com\n\
             Depends: libnss3 (>= 3.26), libgtk-3-0, libfoo-unmapped\nRecommends: pulseaudio\n\
             Description: Demo app\n Long text.\n",
        )
        .control_file("postinst", POSTINST)
        .control_file("prerm", "#!/bin/sh\nupdate-alternatives --remove demo /opt/Demo/demo\nrm -f /etc/demo/stale\n")
        .control_file("conffiles", "/etc/default/demo\n/etc/gone remove-on-upgrade\n")
        .entry(TestEntry::dir("./", 0o755))
        .entry(TestEntry::file("./opt/Demo/demo", 0o755, &elf))
        .entry(TestEntry::file("./opt/Demo/libffmpeg.so", 0o755, b"not elf, but long enough to be checked for a magic number ......"))
        .entry(TestEntry::file("./opt/Demo/chrome-sandbox", 0o755, b"sandbox"))
        .entry(TestEntry::file("./opt/Demo/resources/app.asar", 0o666, b"asar"))
        .entry(TestEntry::file("./bin/demo-helper", 0o755, b"helper"))
        .entry(TestEntry::file("./usr/lib/x86_64-linux-gnu/libdemo.so.1", 0o644, b"lib"))
        .entry(TestEntry::file("./etc/apt/sources.list.d/demo.list", 0o644, b"deb https://example.com stable main"))
        .entry(TestEntry::file("./etc/default/demo", 0o644, b"X=1"))
        .entry(TestEntry::file("./usr/share/applications/demo.desktop", 0o644, b"[Desktop Entry]\nExec=/opt/Demo/demo %U\nIcon=demo\n"))
        .entry(TestEntry::file("./usr/share/icons/hicolor/256x256/apps/demo.png", 0o644, b"png"))
        .build();
        Deb::from_reader(Cursor::new(bytes)).unwrap()
    }

    fn depmap() -> DepMap {
        DepMap::builtin()
    }

    fn lookup(wanted: &[String]) -> Lookup {
        let mut wanted = wanted.to_vec();
        wanted.sort();
        assert_eq!(wanted, ["libc.so.6", "libgtk-3.so.0", "libmystery.so.1", "libnss3.so"]);
        Lookup::Found(BTreeMap::from([
            ("libnss3.so".into(), vec!["extra/nss".into()]),
            ("libgtk-3.so.0".into(), vec!["extra/gtk3".into()]),
            ("libc.so.6".into(), vec!["core/glibc".into()]),
        ]))
    }

    #[test]
    fn translates_a_full_deb() {
        let mut deb = demo_deb();
        let t = translate(&mut deb, &depmap(), 1, lookup, |_| false).unwrap();
        let p = &t.package;

        assert_eq!((p.name.as_str(), p.version.to_string().as_str(), p.arch.as_str()), ("demo", "2:1.0~rc1-1", "x86_64"));
        assert_eq!(p.description, "Demo app");
        assert_eq!(p.url.as_deref(), Some("https://example.com"));
        assert_eq!(p.license, "custom");
        assert_eq!(p.depends, ["nss", "gtk3"]);
        assert_eq!(p.optdepends, [("libpulse".to_string(), "recommended by the deb".to_string())]);
        assert_eq!(p.backup, ["etc/default/demo"]);

        let listing: Vec<String> = p
            .nodes
            .iter()
            .map(|n| match &n.kind {
                NodeKind::Dir => format!("{} dir {:o}", n.path, n.mode),
                NodeKind::File => format!("{} {:o}", n.path, n.mode),
                NodeKind::Symlink(t) => format!("{} -> {t}", n.path),
                NodeKind::Hardlink(t) => format!("{} => {t}", n.path),
            })
            .collect();
        assert_eq!(
            listing,
            [
                "/etc dir 755",
                "/etc/default dir 755",
                "/etc/default/demo 644",
                "/opt dir 755",
                "/opt/Demo dir 755",
                "/opt/Demo/chrome-sandbox 4755",
                "/opt/Demo/demo 755",
                "/opt/Demo/libffmpeg.so 755",
                "/opt/Demo/resources dir 755",
                "/opt/Demo/resources/app.asar 644",
                "/usr dir 755",
                "/usr/bin dir 755",
                "/usr/bin/demo -> /opt/Demo/demo",
                "/usr/bin/demo-helper 755",
                "/usr/lib dir 755",
                "/usr/lib/libdemo.so.1 644",
                "/usr/share dir 755",
                "/usr/share/applications dir 755",
                "/usr/share/applications/demo.desktop 644",
                "/usr/share/icons dir 755",
                "/usr/share/icons/hicolor dir 755",
                "/usr/share/icons/hicolor/256x256 dir 755",
                "/usr/share/icons/hicolor/256x256/apps dir 755",
                "/usr/share/icons/hicolor/256x256/apps/demo.png 644",
            ]
        );

        let has = |list: &[String], want: &str| list.iter().any(|s| s.contains(want));
        for want in [
            "removed apt file /etc/apt/sources.list.d/demo.list",
            "moved 1 entry from /bin into /usr/bin",
            "moved 1 entry from /usr/lib/x86_64-linux-gnu into /usr/lib",
            "added symlink /usr/bin/demo -> /opt/Demo/demo (postinst line 3)",
            "/opt/Demo/chrome-sandbox: mode 0755 -> 4755 (postinst line 4)",
            "removed group/other write permission from /opt/Demo/resources/app.asar",
        ] {
            assert!(has(&t.changes, want), "missing change {want:?} in {:#?}", t.changes);
        }
        for want in [
            "postinst line 6: not translated (no translation for this command): weird-tool --setup",
            "prerm line 3: removes /etc/demo/stale, which is not in the package",
            "conffile /etc/gone is not in the package",
            "no Arch name for Depends: libfoo-unmapped",
            "libmystery.so.1 is needed by a binary but no Arch package provides it",
        ] {
            assert!(has(&t.warnings, want), "missing warning {want:?} in {:#?}", t.warnings);
        }
        assert_eq!(t.warnings.len(), 5, "{:#?}", t.warnings);
        assert!(has(&t.notes, "version constraints dropped"), "{:?}", t.notes);

        assert_eq!(t.desktop.len(), 1);
        assert!(t.desktop[0].problems.is_empty(), "{:?}", t.desktop);
        assert_eq!(t.sonames.covered, 3);
        assert_eq!(t.sonames.suggestions, []);
        assert_eq!(t.unmapped, ["Depends: libfoo-unmapped"]);

        let outcomes: Vec<(&str, usize)> = t.scripts.iter().map(|c| (c.script.as_str(), c.line)).collect();
        assert_eq!(outcomes, [("postinst", 3), ("postinst", 4), ("postinst", 5), ("postinst", 6), ("prerm", 2), ("prerm", 3)]);
    }

    #[test]
    fn rejects_bad_arch_and_names() {
        let cases = [
            ("Package: demo\nVersion: 1\nArchitecture: i386\n", "architecture 'i386' is not supported"),
            ("Package: Demo\nVersion: 1\nArchitecture: amd64\n", "not a valid pacman package name"),
            ("Package: demo\nVersion: v1\nArchitecture: amd64\n", "must start with a digit"),
        ];
        for (control, want) in cases {
            let mut deb = Deb::from_reader(Cursor::new(DebBuilder::new(control).build())).unwrap();
            let err = translate(&mut deb, &depmap(), 1, |_| Lookup::NoDatabase, |_| false).err().unwrap().to_string();
            assert!(err.contains(want), "expected '{want}', got '{err}'");
        }
        let cases = [("amd64", "x86_64"), ("arm64", "aarch64"), ("all", "any")];
        for (deb, arch) in cases {
            assert_eq!(map_arch(deb).unwrap(), arch);
        }
    }
}
