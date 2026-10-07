//! `ferry convert`: turn a .deb into a pacman package. So far only --dry-run, which
//! prints what would be built and everything that needs attention.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

use crate::deb::Deb;
use crate::error::{Context, Result, bail};
use crate::human;
use crate::model::NodeKind;
use crate::paths::Paths;
use crate::translate::{self, Action, DepMap, Outcome, Translation};

pub struct Options {
    pub deb: PathBuf,
    pub dry_run: bool,
    pub direct: bool,
    pub out: Option<PathBuf>,
}

pub fn run(opts: &Options) -> Result<()> {
    if !opts.dry_run {
        bail!("building packages is not implemented yet; use --dry-run to see what would be built");
    }
    let paths = Paths::from_env()?;
    let depmap = DepMap::load(&paths.config)?;
    let mut deb = Deb::open(&opts.deb)?;
    let t = translate::translate(&mut deb, &depmap, 1, translate::pacman_lookup, translate::on_system)
        .context(opts.deb.display())?;
    let backend = if opts.direct { "direct (.pkg.tar.zst written by Ferry)" } else { "makepkg (PKGBUILD)" };
    let mut text = format!("Backend: {backend}\n");
    if let Some(out) = &opts.out {
        writeln!(text, "Output:  {}", out.display()).unwrap();
    }
    text.push_str(&report(&t));
    let _ = std::io::stdout().write_all(text.as_bytes());
    Ok(())
}

pub fn report(t: &Translation) -> String {
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
        writeln!(out, "\nNo warnings.").unwrap();
    } else {
        section(&mut out, &format!("Warnings ({})", t.warnings.len()), &t.warnings);
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
        let lead = if t.unmapped.is_empty() {
            "  not in depends, probably pulled in by them:".to_string()
        } else {
            format!("  not in depends; may stand in for unmapped {}:", t.unmapped.join(", "))
        };
        writeln!(out, "{lead}").unwrap();
        let width = s.suggestions.iter().map(|(l, _)| l.len()).max().unwrap_or(0) + 2;
        for (lib, owners) in &s.suggestions {
            writeln!(out, "    {lib:<width$}{}", owners.join(" or ")).unwrap();
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
                Action::Symlink { link, target } => format!("symlink {link} -> {target} in the package"),
                Action::Chmod { path, mode } => format!("mode {mode:04o} on {path} in the package"),
                Action::RemoveOwned { path } => format!("nothing, pacman removes {path} with the package"),
            })
            .collect::<Vec<_>>()
            .join("; "),
        Outcome::Hook(what) => format!("nothing, a pacman hook updates the {what}"),
        Outcome::Handled(why) => format!("nothing, {why}"),
        Outcome::AptRepo => "dropped, apt repository setup".to_string(),
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
    use crate::translate::Lookup;
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
        let t = translate::translate(&mut deb, &DepMap::builtin(), 1, |_| Lookup::NoDatabase, |_| false).unwrap();
        let text = report(&t);
        for want in [
            "Would build demo 1.0-1 for x86_64 (from deb version 1.0-1)",
            "  Depends      nss\n",
            "  Optdepends   libpulse: recommended by the deb\n",
            "  Contents     1 file, 4 directories, 1 symlink, 1 B\n",
            "  postinst line 2: ln -sf /opt/Demo/demo /usr/bin/demo\n    -> symlink /usr/bin/demo -> /opt/Demo/demo in the package\n",
            "    -> nothing, a pacman hook updates the MIME database\n",
            "  postinst line 4: frobnicate\n    -> NOT TRANSLATED: no translation for this command\n",
            "Libraries the binaries load:\n  no dynamically linked binaries found\n",
            "Warnings (3):",
            "no Arch name for Depends: libodd1",
            "no .desktop file in /usr/share/applications",
        ] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
    }
}
