// SPDX-License-Identifier: AGPL-3.0-or-later
//! `pacdeb convert`: turn a .deb into a pacman package, or with --dry-run, print what
//! would be built and everything that needs attention.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::build;
use crate::clash;
use crate::deb::Deb;
use crate::error::{Context, Result};
use crate::human;
use crate::model::{NodeKind, Package};
use crate::paths::Paths;
use crate::registry::{App, AppState};
use crate::version::{ArchVersion, DebVersion};
use crate::style::Style;
use crate::translate::{self, Action, Outcome, Tables, Translation, Warning};

pub struct Options {
    pub deb: PathBuf,
    pub dry_run: bool,
    pub direct: bool,
    pub out: Option<PathBuf>,
}

pub fn run(opts: &Options) -> Result<()> {
    if opts.dry_run {
        let paths = Paths::from_env()?;
        let tables = Tables::load(&paths.config)?;
        let mut deb = Deb::open(&opts.deb)?;
        let mut t = translate::translate(&mut deb, &tables, 1, &translate::LiveSystem).context(opts.deb.display())?;
        avoid_clash(&mut t.package, None, &clash::LiveLookup::new(), Style::for_stdout());
        let backend = if opts.direct { "direct (.pkg.tar.zst written by pacdeb)" } else { "makepkg (PKGBUILD)" };
        let mut text = format!("Backend: {backend}\n");
        if let Some(out) = &opts.out {
            writeln!(text, "Output:  {}", out.display()).unwrap();
        }
        text.push_str(&report(&t, Style::for_stdout()));
        let _ = std::io::stdout().write_all(text.as_bytes());
        return Ok(());
    }
    let built = build_package(&opts.deb, opts.direct, opts.out.as_deref(), None, None)?;
    println!("{} {}", Style::for_stdout().good("Built"), built.path.display());
    Ok(())
}

/// A finished build and the versions that went into it.
pub struct Built {
    pub path: PathBuf,
    pub deb_version: String,
    pub pkgrel: u32,
    /// The package name chosen because the deb's name is taken, to remember for the app.
    pub renamed: Option<String>,
    /// Every warning of this build, accepted or not.
    pub warnings: Vec<crate::registry::RecordedWarning>,
}

/// The pkgrel for a new build: 1 for a new upstream version, one more than last time
/// when the same upstream version comes in a different deb, unchanged when the deb is
/// the same one again.
pub fn next_pkgrel(prev: Option<&AppState>, deb_version: &DebVersion, pkgver: &str) -> u32 {
    let Some(prev) = prev else {
        return 1;
    };
    let Some(prev_deb) = prev.deb_version.as_deref().and_then(|v| DebVersion::parse(v).ok()) else {
        return 1;
    };
    let prev_pkgver = ArchVersion::from_debian(&prev_deb, 1).pkgver;
    if prev_deb == *deb_version && prev_deb.to_string() == deb_version.to_string() {
        prev.pkgrel.max(1)
    } else if prev_pkgver == pkgver && prev_deb.epoch == deb_version.epoch {
        prev.pkgrel.max(1) + 1
    } else {
        1
    }
}

/// Per app settings that change the package: name, provides, conflicts, deps.
pub fn apply_overrides(p: &mut Package, app: &App) {
    if let Some(name) = &app.pkgname {
        p.name = name.clone();
    }
    p.provides.extend(app.provides.iter().cloned());
    p.conflicts.extend(app.conflicts.iter().cloned());
    for d in &app.extra_depends {
        if !p.depends.contains(d) {
            p.depends.push(d.clone());
        }
    }
    p.depends.retain(|d| !app.drop_depends.contains(d));
    p.optdepends.retain(|(d, _)| !app.drop_depends.contains(d));
}

/// Gives a package whose name a repo or the AUR also uses a name of its own, unless the
/// app sets one. Returns the new name. When the lookup fails the name is kept, since
/// a build should not depend on the AUR being reachable.
pub fn avoid_clash(p: &mut Package, app: Option<&App>, lookup: &dyn clash::Lookup, st: Style) -> Option<String> {
    if app.is_some_and(|a| a.pkgname.is_some()) {
        return None;
    }
    match clash::choose(&p.name, lookup) {
        Ok(None) => None,
        Ok(Some(r)) => {
            let base = std::mem::replace(&mut p.name, r.name.clone());
            println!(
                "{} {base} is also a package in {}, so a system update would replace this build.\n\
                 Building it as {} instead, which provides and conflicts with {base}.",
                st.warn("note:"),
                r.because,
                r.name
            );
            for list in [&mut p.provides, &mut p.conflicts] {
                if !list.contains(&base) {
                    list.push(base.clone());
                }
            }
            Some(r.name)
        }
        Err(e) => {
            println!("{} could not check whether {} is taken in the repos or the AUR ({e}); keeping the name", st.warn("warning:"), p.name);
            None
        }
    }
}

/// Translates and builds a deb, printing the warnings first. `app` and `prev` are the
/// tracked app's settings and last build, when there is one.
pub fn build_package(deb_path: &Path, direct: bool, out: Option<&Path>, app: Option<&App>, prev: Option<&AppState>) -> Result<Built> {
    let direct = direct || {
        let missing = !build::makepkg_available();
        if missing {
            println!("makepkg was not found, so pacdeb writes the package itself (--direct)");
        }
        missing
    };
    let paths = Paths::from_env()?;
    let tables = Tables::load(&paths.config)?;
    let mut deb = Deb::open(deb_path)?;
    let file = deb_path.file_name().map_or_else(|| deb_path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let reading = crate::progress::Progress::status(format!("Reading {file}"));
    let mut t = translate::translate(&mut deb, &tables, 1, &translate::LiveSystem).context(deb_path.display())?;
    reading.finish();
    t.package.version.pkgrel = next_pkgrel(prev, &t.package.deb_version, &t.package.version.pkgver);
    if let Some(app) = app {
        apply_overrides(&mut t.package, app);
    }
    let style = Style::for_stdout();
    let renamed = avoid_clash(&mut t.package, app, &clash::LiveLookup::new(), style);

    let p = &t.package;
    println!("Building {} {} from {}", style.bold(&p.name), p.version, deb_path.display());
    let accepted = app.map(|a| a.accepted_warnings.as_slice()).unwrap_or_default();
    let (old, new): (Vec<Warning>, Vec<Warning>) = t.warnings.iter().cloned().partition(|w| accepted.contains(&w.key()));
    if !new.is_empty() {
        print!("\n{}", render_warnings(&new, style));
        println!("\n{}\n", style.dim(&format!("Full report: pacdeb convert --dry-run {}", deb_path.display())));
    }
    if !old.is_empty() {
        println!("{}", style.dim(&format!("{} you accepted earlier hidden", human::plural(old.len(), "warning", "warnings"))));
    }

    let out_dir = match out {
        Some(o) => std::path::absolute(o).context(o.display())?,
        None => paths.packages_dir(),
    };
    let path = if direct {
        build::direct(&mut deb, p, &paths.work_dir(), &out_dir)?
    } else {
        let origin = deb_path.file_name().map_or_else(|| deb_path.display().to_string(), |n| n.to_string_lossy().into_owned());
        build::with_makepkg(&mut deb, p, &origin, &paths.work_dir(), &out_dir)?
    };
    let warnings = t.warnings.iter().map(|w| crate::registry::RecordedWarning { key: w.key(), text: w.to_string() }).collect();
    Ok(Built { path, deb_version: p.deb_version.to_string(), pkgrel: p.version.pkgrel, renamed, warnings })
}

/// Warnings grouped by kind, with script lines left out for system reasons grouped
/// under the condition they wait on.
pub fn render_warnings(ws: &[Warning], st: Style) -> String {
    let mut out = String::new();
    if ws.is_empty() {
        return out;
    }
    writeln!(out, "{}", st.warn(&human::plural(ws.len(), "warning", "warnings"))).unwrap();

    let at = |script: &str, line: usize| format!("{script}:{line}");
    let width = ws
        .iter()
        .filter_map(|w| match w {
            Warning::SystemDependent { script, line, .. } | Warning::Untranslated { script, line, .. } => {
                Some(at(script, *line).len())
            }
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let pad = |s: String| format!("{s:<width$}");

    let mut groups: Vec<(&str, Vec<(String, &str)>)> = Vec::new();
    for w in ws {
        if let Warning::SystemDependent { script, line, text, condition } = w {
            let entry = (at(script, *line), text.as_str());
            match groups.iter_mut().find(|(c, _)| *c == condition.as_str()) {
                Some((_, items)) => items.push(entry),
                None => groups.push((condition, vec![entry])),
            }
        }
    }
    if !groups.is_empty() {
        writeln!(out, "\n  {}", st.bold("Left out because they depend on your system, not on the package:")).unwrap();
        for (condition, items) in groups {
            writeln!(out, "    if {condition}").unwrap();
            for (a, text) in items {
                writeln!(out, "      {}  {text}", st.dim(&pad(a))).unwrap();
            }
        }
    }

    let untranslated: Vec<_> = ws
        .iter()
        .filter_map(|w| match w {
            Warning::Untranslated { script, line, text, why } => Some((at(script, *line), text, why)),
            _ => None,
        })
        .collect();
    if !untranslated.is_empty() {
        writeln!(out, "\n  {}", st.bold("Script lines pacdeb could not translate (check them by hand):")).unwrap();
        for (a, text, why) in untranslated {
            writeln!(out, "    {}  {text}", st.dim(&pad(a))).unwrap();
            writeln!(out, "    {:width$}  {}", "", st.dim(why)).unwrap();
        }
    }

    let unmapped: Vec<_> = ws
        .iter()
        .filter_map(|w| match w {
            Warning::Unmapped { field, deps } => Some((deps, field)),
            _ => None,
        })
        .collect();
    if !unmapped.is_empty() {
        writeln!(out, "\n  {}", st.bold("Dependencies with no Arch name, left out (map them in depmap.toml if needed):")).unwrap();
        for (deps, field) in unmapped {
            writeln!(out, "    {deps}  {}", st.dim(&format!("({field})"))).unwrap();
        }
    }

    let other: Vec<&String> = ws
        .iter()
        .filter_map(|w| match w {
            Warning::Other(s) => Some(s),
            _ => None,
        })
        .collect();
    if !other.is_empty() {
        writeln!(out, "\n  {}", st.bold("Other:")).unwrap();
        for s in other {
            writeln!(out, "    {s}").unwrap();
        }
    }
    out
}

pub fn report(t: &Translation, st: Style) -> String {
    let mut out = String::new();
    let p = &t.package;
    writeln!(out, "Would build {} {} for {} (from deb version {})", p.name, p.version, p.arch, p.deb_version).unwrap();

    let count = |pred: fn(&NodeKind) -> bool| p.nodes.iter().filter(|n| pred(&n.kind)).count();
    let files = count(|k| matches!(k, NodeKind::File | NodeKind::Hardlink(_)));
    let dirs = count(|k| *k == NodeKind::Dir);
    let links = count(|k| matches!(k, NodeKind::Symlink(_)));
    let total: u64 = p.nodes.iter().filter(|n| n.kind == NodeKind::File).map(|n| n.size).sum();

    let mut fields = vec![
        ("Description", p.description.clone()),
        ("URL", p.url.clone().unwrap_or_else(|| "(none)".into())),
        ("License", p.license.clone()),
        ("Depends", list_or_none(&p.depends.join(" "))),
    ];
    if !p.optdepends.is_empty() {
        let opt: Vec<String> = p.optdepends.iter().map(|(n, why)| format!("{n}: {why}")).collect();
        fields.push(("Optdepends", opt.join("\n")));
    }
    if !p.backup.is_empty() {
        fields.push(("Backup", p.backup.join(" ")));
    }
    fields.push((
        "Contents",
        format!(
            "{}, {}, {}, {}",
            human::plural(files, "file", "files"),
            human::plural(dirs, "directory", "directories"),
            human::plural(links, "symlink", "symlinks"),
            human::size(total)
        ),
    ));
    for (name, value) in fields {
        let mut lines = value.lines();
        writeln!(out, "  {name:<13}{}", lines.next().unwrap_or("")).unwrap();
        for l in lines {
            writeln!(out, "  {:<13}{l}", "").unwrap();
        }
    }

    section(&mut out, "Changes", &t.changes);
    section(&mut out, "Notes", &t.notes);
    if !p.install_note.is_empty() {
        writeln!(out, "\nInstall note (pacman shows this after installing):").unwrap();
        for line in &p.install_note {
            writeln!(out, "  | {line}").unwrap();
        }
    }

    if !t.desktop.is_empty() {
        writeln!(out, "\nLauncher entries:").unwrap();
        for d in &t.desktop {
            let status = if d.problems.is_empty() { "ok" } else { "has problems, see warnings" };
            writeln!(
                out,
                "  {}: {status} (runs {}, icon {})",
                d.path,
                d.exec.as_deref().unwrap_or("?"),
                d.icon.as_deref().unwrap_or("?")
            )
            .unwrap();
        }
    }

    writeln!(out, "\nMaintainer scripts:").unwrap();
    if t.scripts.is_empty() {
        writeln!(out, "  nothing to translate").unwrap();
    }
    for c in &t.scripts {
        writeln!(out, "  {} line {}: {}", c.script, c.line, c.text).unwrap();
        writeln!(out, "    -> {}", describe(&c.outcome)).unwrap();
    }

    libraries(&mut out, t);

    if t.warnings.is_empty() {
        writeln!(out, "\n{}", st.good("No warnings.")).unwrap();
    } else {
        write!(out, "\n{}", render_warnings(&t.warnings, st)).unwrap();
    }
    out
}

fn libraries(out: &mut String, t: &Translation) {
    let s = &t.sonames;
    writeln!(out, "\nLibraries the binaries load:").unwrap();
    if let Some(problem) = &s.problem {
        writeln!(out, "  {problem}").unwrap();
        return;
    }
    if s.covered == 0 && s.suggestions.is_empty() && s.missing.is_empty() {
        writeln!(out, "  no dynamically linked binaries found").unwrap();
        return;
    }
    writeln!(out, "  {} covered by depends or the base system", s.covered).unwrap();
    if !s.suggestions.is_empty() {
        writeln!(out, "  not pulled in by depends; consider adding (in depmap.toml or per app):").unwrap();
        let width = s.suggestions.iter().map(|(p, _)| p.len()).max().unwrap_or(0) + 2;
        for (packages, libs) in &s.suggestions {
            writeln!(out, "    {packages:<width$}for {}", libs.join(", ")).unwrap();
        }
        if !t.unmapped.is_empty() {
            writeln!(out, "  the unmapped deps in the warnings below may be among these").unwrap();
        }
    }
    if !s.missing.is_empty() {
        writeln!(out, "  not provided by any Arch package: {}", s.missing.join(", ")).unwrap();
    }
}

fn describe(o: &Outcome) -> String {
    match o {
        Outcome::Actions(actions) => actions
            .iter()
            .map(|a| match a {
                Action::Remove { path } => format!("nothing, pacman removes {path} with the package"),
                a => format!("{} in the package", translate::describe_action(a)),
            })
            .collect::<Vec<_>>()
            .join("; "),
        Outcome::Hook(what) => format!("nothing, a pacman hook updates the {what}"),
        Outcome::Service(steps) => {
            let units: Vec<String> = steps.iter().map(|s| format!("{} {}", s.verb, s.unit)).collect();
            format!("left to you ({}); the install note says how", units.join(", "))
        }
        Outcome::Handled(why) => format!("nothing, {why}"),
        Outcome::AptRepo => "dropped, apt repository setup".to_string(),
        Outcome::Skipped(why) => format!("nothing, it {why}"),
        Outcome::Conditional { condition, would } => format!("left out, runs only if {condition}; it would {would}"),
        Outcome::Unknown(why) => format!("NOT TRANSLATED: {why}"),
    }
}

fn section(out: &mut String, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    writeln!(out, "\n{title}:").unwrap();
    for item in items {
        writeln!(out, "  - {item}").unwrap();
    }
}

fn list_or_none(s: &str) -> String {
    if s.is_empty() { "(none)".to_string() } else { s.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deb::testutil::{DebBuilder, TestEntry};
    use crate::translate::tests::BareSystem;
    use std::io::Cursor;

    #[test]
    fn counts_pkgrel() {
        let prev = |v: &str, rel: u32| AppState { deb_version: Some(v.into()), pkgrel: rel, ..Default::default() };
        let rel = |p: Option<&AppState>, v: &str| {
            let d = DebVersion::parse(v).unwrap();
            next_pkgrel(p, &d, &ArchVersion::from_debian(&d, 1).pkgver)
        };
        let cases: &[(Option<AppState>, &str, u32)] = &[
            (None, "1.0-1", 1),
            (Some(prev("1.0-1", 1)), "1.0-1", 1),
            (Some(prev("1.0-1", 3)), "1.0-1", 3),
            (Some(prev("1.0-1", 1)), "1.0-2", 2),
            (Some(prev("1.0-2", 2)), "1.0-3", 3),
            (Some(prev("1.0-3", 3)), "1.1-1", 1),
            (Some(prev("1:1.0-1", 1)), "1.0-1", 1),
            (Some(AppState::default()), "1.0", 1),
        ];
        for (p, v, want) in cases {
            assert_eq!(rel(p.as_ref(), v), *want, "{p:?} -> {v}");
        }
    }

    #[test]
    fn applies_app_overrides() {
        let bytes = DebBuilder::new("Package: demo\nVersion: 1.0\nArchitecture: amd64\nDepends: libnss3, libnotify4\nRecommends: pulseaudio\n").build();
        let mut deb = Deb::from_reader(Cursor::new(bytes)).unwrap();
        let mut p = translate::translate(&mut deb, &translate::Tables::builtin(), 1, &BareSystem).unwrap().package;
        let mut app = App::new(crate::registry::SourceConfig::Manual {});
        app.pkgname = Some("demo-deb".into());
        app.provides = vec!["demo".into()];
        app.conflicts = vec!["demo-bin".into()];
        app.extra_depends = vec!["gtk3".into()];
        app.drop_depends = vec!["libnotify".into(), "libpulse".into()];
        apply_overrides(&mut p, &app);
        assert_eq!(p.name, "demo-deb");
        assert_eq!(p.provides, ["demo"]);
        assert_eq!(p.conflicts, ["demo-bin"]);
        assert_eq!(p.depends, ["nss", "gtk3"]);
        assert!(p.optdepends.is_empty());
    }

    struct TakenInAur(&'static [&'static str]);

    impl crate::clash::Lookup for TakenInAur {
        fn repo_of(&self, _: &str) -> Option<String> {
            None
        }
        fn in_aur(&self, names: &[String]) -> Result<Vec<String>> {
            Ok(names.iter().filter(|n| self.0.contains(&n.as_str())).cloned().collect())
        }
    }

    #[test]
    fn renames_packages_whose_name_is_taken() {
        let bytes = DebBuilder::new("Package: demo\nVersion: 1.0\nArchitecture: amd64\n").build();
        let mut deb = Deb::from_reader(Cursor::new(bytes)).unwrap();
        let base = translate::translate(&mut deb, &translate::Tables::builtin(), 1, &BareSystem).unwrap().package;

        let mut p = base.clone();
        assert_eq!(avoid_clash(&mut p, None, &TakenInAur(&["demo"]), Style::plain()).as_deref(), Some("demo-deb"));
        assert_eq!((p.name.as_str(), &p.provides[..], &p.conflicts[..]), ("demo-deb", &["demo".to_string()][..], &["demo".to_string()][..]));

        let mut p = base.clone();
        assert_eq!(avoid_clash(&mut p, None, &TakenInAur(&[]), Style::plain()), None);
        assert_eq!(p.name, "demo");

        // A name the app sets is kept as is, even when it is taken.
        let mut app = App::new(crate::registry::SourceConfig::Manual {});
        app.pkgname = Some("demo".into());
        let mut p = base.clone();
        assert_eq!(avoid_clash(&mut p, Some(&app), &TakenInAur(&["demo"]), Style::plain()), None);
        assert_eq!(p.name, "demo");
    }

    #[test]
    fn reports_a_dry_run() {
        let bytes = DebBuilder::new(
            "Package: demo\nVersion: 1.0-1\nArchitecture: amd64\nDepends: libnss3, libodd1\nRecommends: pulseaudio\nDescription: Demo\n",
        )
        .control_file("postinst", "#!/bin/sh\nln -sf /opt/Demo/demo /usr/bin/demo\nupdate-mime-database /usr/share/mime\nfrobnicate\n")
        .entry(TestEntry::file("./opt/Demo/demo", 0o755, b"x"))
        .build();
        let mut deb = Deb::from_reader(Cursor::new(bytes)).unwrap();
        let t = translate::translate(&mut deb, &translate::Tables::builtin(), 1, &BareSystem).unwrap();
        let text = report(&t, Style::plain());
        for want in [
            "Would build demo 1.0-1 for x86_64 (from deb version 1.0-1)",
            "  Depends      nss\n",
            "  Optdepends   libpulse: recommended by the deb\n",
            "  Contents     1 file, 4 directories, 1 symlink, 1 B\n",
            "  postinst line 2: ln -sf /opt/Demo/demo /usr/bin/demo\n    -> symlink /usr/bin/demo -> /opt/Demo/demo in the package\n",
            "    -> nothing, a pacman hook updates the MIME database\n",
            "  postinst line 4: frobnicate\n    -> NOT TRANSLATED: no translation for this command\n",
            "Libraries the binaries load:\n  no dynamically linked binaries found\n",
            "\n2 warnings\n",
            "  Script lines pacdeb could not translate (check them by hand):\n    postinst:4  frobnicate\n                no translation for this command\n",
            "  Dependencies with no Arch name, left out (map them in depmap.toml if needed):\n    libodd1  (Depends)\n",
        ] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
    }

    #[test]
    fn groups_system_dependent_lines_by_condition() {
        let sd = |script: &str, line, text: &str, condition: &str| Warning::SystemDependent {
            script: script.into(),
            line,
            text: text.into(),
            condition: condition.into(),
        };
        let ws = [
            sd("postinst", 255, "rm -f /etc/apparmor.d/app", "/etc/apparmor.d/abi/4.0 exists"),
            sd("postinst", 64, "rm -f /usr/bin/ccd", "/usr/bin/ccd is a symlink"),
            sd("postinst", 256, "cat > /etc/apparmor.d/app", "/etc/apparmor.d/abi/4.0 exists"),
            sd("postrm", 28, "rm -f /usr/bin/ccd", "/usr/bin/ccd is a symlink"),
        ];
        let text = render_warnings(&ws, Style::plain());
        let want = "\
4 warnings

  Left out because they depend on your system, not on the package:
    if /etc/apparmor.d/abi/4.0 exists
      postinst:255  rm -f /etc/apparmor.d/app
      postinst:256  cat > /etc/apparmor.d/app
    if /usr/bin/ccd is a symlink
      postinst:64   rm -f /usr/bin/ccd
      postrm:28     rm -f /usr/bin/ccd
";
        assert_eq!(text, want);
    }
}
