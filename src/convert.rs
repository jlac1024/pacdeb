//! `ferry convert`: turn a .deb into a pacman package, or with --dry-run, print what
//! would be built and everything that needs attention.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::build;
use crate::deb::Deb;
use crate::error::{Context, Result};
use crate::human;
use crate::model::NodeKind;
use crate::paths::Paths;
use crate::style::Style;
use crate::translate::{self, Action, DepMap, Outcome, Translation, Warning};

pub struct Options {
    pub deb: PathBuf,
    pub dry_run: bool,
    pub direct: bool,
    pub out: Option<PathBuf>,
}

pub fn run(opts: &Options) -> Result<()> {
    if opts.dry_run {
        let paths = Paths::from_env()?;
        let depmap = DepMap::load(&paths.config)?;
        let mut deb = Deb::open(&opts.deb)?;
        let t = translate::translate(&mut deb, &depmap, 1, &translate::LiveSystem).context(opts.deb.display())?;
        let backend = if opts.direct { "direct (.pkg.tar.zst written by Ferry)" } else { "makepkg (PKGBUILD)" };
        let mut text = format!("Backend: {backend}\n");
        if let Some(out) = &opts.out {
            writeln!(text, "Output:  {}", out.display()).unwrap();
        }
        text.push_str(&report(&t, Style::for_stdout()));
        let _ = std::io::stdout().write_all(text.as_bytes());
        return Ok(());
    }
    let built = build_package(&opts.deb, opts.direct, opts.out.as_deref())?;
    println!("{} {}", Style::for_stdout().good("Built"), built.display());
    Ok(())
}

/// Translates and builds a deb, printing the warnings first. Returns the package path.
pub fn build_package(deb_path: &Path, direct: bool, out: Option<&Path>) -> Result<PathBuf> {
    let direct = direct || {
        let missing = !build::makepkg_available();
        if missing {
            println!("makepkg was not found, so Ferry writes the package itself (--direct)");
        }
        missing
    };
    let paths = Paths::from_env()?;
    let depmap = DepMap::load(&paths.config)?;
    let mut deb = Deb::open(deb_path)?;
    let t = translate::translate(&mut deb, &depmap, 1, &translate::LiveSystem).context(deb_path.display())?;

    let p = &t.package;
    let style = Style::for_stdout();
    println!("Building {} {} from {}", style.bold(&p.name), p.version, deb_path.display());
    if !t.warnings.is_empty() {
        print!("\n{}", render_warnings(&t.warnings, style));
        println!("\n{}\n", style.dim(&format!("Full report: ferry convert --dry-run {}", deb_path.display())));
    }

    let out_dir = match out {
        Some(o) => std::path::absolute(o).context(o.display())?,
        None => paths.packages_dir(),
    };
    if direct {
        return build::direct(&mut deb, p, &paths.work_dir(), &out_dir);
    }
    let origin = deb_path.file_name().map_or_else(|| deb_path.display().to_string(), |n| n.to_string_lossy().into_owned());
    build::with_makepkg(&mut deb, p, &origin, &paths.work_dir(), &out_dir)
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
        writeln!(out, "\n  {}", st.bold("Script lines Ferry could not translate (check them by hand):")).unwrap();
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
    fn reports_a_dry_run() {
        let bytes = DebBuilder::new(
            "Package: demo\nVersion: 1.0-1\nArchitecture: amd64\nDepends: libnss3, libodd1\nRecommends: pulseaudio\nDescription: Demo\n",
        )
        .control_file("postinst", "#!/bin/sh\nln -sf /opt/Demo/demo /usr/bin/demo\nupdate-mime-database /usr/share/mime\nfrobnicate\n")
        .entry(TestEntry::file("./opt/Demo/demo", 0o755, b"x"))
        .build();
        let mut deb = Deb::from_reader(Cursor::new(bytes)).unwrap();
        let t = translate::translate(&mut deb, &DepMap::builtin(), 1, &BareSystem).unwrap();
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
            "  Script lines Ferry could not translate (check them by hand):\n    postinst:4  frobnicate\n                no translation for this command\n",
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
