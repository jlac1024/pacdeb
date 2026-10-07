//! `pacdeb inspect`: a readable report on what is inside a .deb.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

use crate::control::Control;
use crate::deb::{DataEntry, Deb, EntryKind, MAINTAINER_SCRIPTS};
use crate::error::{Context, Result};
use crate::human;
use crate::relation::{RELATION_FIELDS, format_group, parse_relations};
use crate::version::{ArchVersion, DebVersion};

/// Long lists (icons, symlinks) are cut off after this many lines.
const LIST_LIMIT: usize = 20;
const PACMAN_VERSION: &str = "Pacman version";

pub fn run(path: &Path) -> Result<()> {
    let mut deb = Deb::open(path)?;
    let entries = deb.data_entries().context(path.display())?;
    // A closed pipe (pacdeb inspect x.deb | head) is not worth an error message.
    let _ = std::io::stdout().write_all(report(&deb, &entries).as_bytes());
    Ok(())
}

pub fn report<R>(deb: &Deb<R>, entries: &[DataEntry]) -> String {
    let mut out = String::new();
    control_section(&mut out, deb);
    relations_section(&mut out, &deb.control);
    files_section(&mut out, entries);
    scripts_section(&mut out, &deb.control_files);
    out
}

fn is_relation(name: &str) -> bool {
    RELATION_FIELDS.iter().any(|f| f.eq_ignore_ascii_case(name))
}

fn control_section<R>(out: &mut String, deb: &Deb<R>) {
    let fields: Vec<_> = deb
        .control
        .fields()
        .filter(|(n, _)| !is_relation(n) && !n.eq_ignore_ascii_case("Description"))
        .collect();
    let width = fields
        .iter()
        .map(|(n, _)| n.len())
        .chain([PACMAN_VERSION.len()])
        .max()
        .unwrap_or(0)
        + 2;
    for (name, value) in fields {
        push_field(out, name, value, width);
    }
    // Shown with pkgrel 1, which is what a first build of this deb would get.
    let pacman_version = match deb.control.get("Version").map(DebVersion::parse) {
        Some(Ok(v)) => ArchVersion::from_debian(&v, 1).to_string(),
        Some(Err(e)) => format!("cannot map: {e}"),
        None => "cannot map: no Version field".to_string(),
    };
    push_field(out, PACMAN_VERSION, &pacman_version, width);
    let archives = format!("control {}, data {}", deb.control_compression, deb.data_compression);
    push_field(out, "Archives", &archives, width);
    if let Some((synopsis, long)) = deb.control.description() {
        let text = if long.is_empty() {
            synopsis.to_string()
        } else {
            format!("{synopsis}\n{long}")
        };
        push_field(out, "Description", &text, width);
    }
}

fn push_field(out: &mut String, name: &str, value: &str, width: usize) {
    let mut lines = value.lines();
    let first = lines.next().unwrap_or("");
    let line = format!("{:<width$}{first}", format!("{name}:"));
    writeln!(out, "{}", line.trim_end()).unwrap();
    for l in lines {
        let line = format!("{:width$}{l}", "");
        writeln!(out, "{}", line.trim_end()).unwrap();
    }
}

fn relations_section(out: &mut String, control: &Control) {
    for field in RELATION_FIELDS {
        let Some(value) = control.get(field) else {
            continue;
        };
        writeln!(out, "\n{field}:").unwrap();
        match parse_relations(value) {
            Ok(groups) if groups.is_empty() => writeln!(out, "  (empty)").unwrap(),
            Ok(groups) => {
                for g in groups {
                    writeln!(out, "  {}", format_group(&g)).unwrap();
                }
            }
            Err(e) => {
                writeln!(out, "  cannot parse: {e}").unwrap();
                writeln!(out, "  raw: {}", value.replace('\n', " ")).unwrap();
            }
        }
    }
}

fn files_section(out: &mut String, entries: &[DataEntry]) {
    let count = |k: EntryKind| entries.iter().filter(|e| e.kind == k).count();
    let others = entries.iter().filter(|e| matches!(e.kind, EntryKind::Other(_))).count();
    let total: u64 = entries.iter().filter(|e| e.kind == EntryKind::File).map(|e| e.size).sum();

    let mut summary = vec![
        human::plural(count(EntryKind::File), "file", "files"),
        human::plural(count(EntryKind::Dir), "directory", "directories"),
        human::plural(count(EntryKind::Symlink), "symlink", "symlinks"),
    ];
    if count(EntryKind::Hardlink) > 0 {
        summary.push(human::plural(count(EntryKind::Hardlink), "hardlink", "hardlinks"));
    }
    if others > 0 {
        summary.push(human::plural(others, "special file", "special files"));
    }
    writeln!(out, "\nFiles: {}, {}", summary.join(", "), human::size(total)).unwrap();

    let mut groups: BTreeMap<String, (usize, u64)> = BTreeMap::new();
    for e in entries.iter().filter(|e| e.kind != EntryKind::Dir) {
        let g = groups.entry(group_key(&e.path)).or_default();
        g.0 += 1;
        g.1 += e.size;
    }
    let width = groups.keys().map(String::len).max().unwrap_or(0) + 2;
    for (key, (n, size)) in &groups {
        let label = if *n == 1 { "entry" } else { "entries" };
        writeln!(out, "  {key:<width$}{n:>6} {label:<7}  {:>10}", human::size(*size)).unwrap();
    }

    let not_dir = |e: &&DataEntry| e.kind != EntryKind::Dir;
    push_list(
        out,
        "Desktop entries",
        entries.iter().filter(not_dir).filter(|e| e.path.ends_with(".desktop")).map(|e| e.path.clone()),
    );
    push_list(
        out,
        "Icons",
        entries
            .iter()
            .filter(not_dir)
            .filter(|e| e.path.starts_with("/usr/share/icons/") || e.path.starts_with("/usr/share/pixmaps/"))
            .map(|e| e.path.clone()),
    );
    push_list(
        out,
        "Setuid/setgid",
        entries.iter().filter(|e| e.mode & 0o6000 != 0).map(|e| format!("{:04o} {}", e.mode, e.path)),
    );
    for (kind, title) in [(EntryKind::Symlink, "Symlinks"), (EntryKind::Hardlink, "Hardlinks")] {
        push_list(
            out,
            title,
            entries
                .iter()
                .filter(|e| e.kind == kind)
                .map(|e| format!("{} -> {}", e.path, e.link.as_deref().unwrap_or(""))),
        );
    }
    push_list(
        out,
        "Special files",
        entries.iter().filter(|e| matches!(e.kind, EntryKind::Other(_))).map(|e| e.path.clone()),
    );
}

fn scripts_section(out: &mut String, files: &BTreeMap<String, Vec<u8>>) {
    writeln!(out, "\nMaintainer scripts:").unwrap();
    let mut any = false;
    for name in MAINTAINER_SCRIPTS {
        let Some(body) = files.get(name) else {
            continue;
        };
        any = true;
        let text = String::from_utf8_lossy(body);
        writeln!(out, "  {name} ({})", human::plural(text.lines().count(), "line", "lines")).unwrap();
        for l in text.lines() {
            writeln!(out, "{}", format!("    | {l}").trim_end()).unwrap();
        }
    }
    if !any {
        writeln!(out, "  none").unwrap();
    }

    if let Some(conffiles) = files.get("conffiles") {
        push_list(out, "Conffiles", String::from_utf8_lossy(conffiles).lines().map(str::to_string));
    }
    let others: Vec<&str> = files
        .keys()
        .map(String::as_str)
        .filter(|k| *k != "control" && !MAINTAINER_SCRIPTS.contains(k))
        .collect();
    if !others.is_empty() {
        writeln!(out, "\nOther control files: {}", others.join(", ")).unwrap();
    }
}

fn push_list(out: &mut String, title: &str, items: impl Iterator<Item = String>) {
    let items: Vec<String> = items.collect();
    if items.is_empty() {
        return;
    }
    writeln!(out, "\n{title}:").unwrap();
    for item in items.iter().take(LIST_LIMIT) {
        writeln!(out, "  {item}").unwrap();
    }
    if items.len() > LIST_LIMIT {
        writeln!(out, "  ... and {} more", items.len() - LIST_LIMIT).unwrap();
    }
}

/// The first two directories of an entry's parent, which is enough to see where an
/// app puts things ("/usr/lib", "/opt/SomeApp").
fn group_key(path: &str) -> String {
    let parent = path.rsplit_once('/').map_or("", |(p, _)| p);
    let parts: Vec<_> = parent.split('/').filter(|s| !s.is_empty()).take(2).collect();
    format!("/{}", parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deb::testutil::{DebBuilder, TestEntry};
    use std::io::Cursor;

    #[test]
    fn group_keys() {
        let cases = [
            ("/usr/bin/foo", "/usr/bin"),
            ("/usr/lib/app/sub/x.so", "/usr/lib"),
            ("/opt/App Name/app", "/opt/App Name"),
            ("/etc/foo.conf", "/etc"),
            ("/toplevel", "/"),
        ];
        for (path, want) in cases {
            assert_eq!(group_key(path), want, "{path}");
        }
    }

    #[test]
    fn reports_a_synthetic_deb() {
        let control = "\
Package: demo
Version: 1:2.0-1
Architecture: amd64
Depends: libc6 (>= 2.34), libasound2 | libasound2t64
Recommends: qemu-system-x86
Description: Demo app
 Longer text.
 .
 Second paragraph.
";
        let bytes = DebBuilder::new(control)
            .control_file("postinst", "#!/bin/sh\nupdate-alternatives --install /usr/bin/demo demo /opt/Demo/demo 100\n")
            .control_file("md5sums", "abc  opt/Demo/demo\n")
            .entry(TestEntry::dir("./", 0o755))
            .entry(TestEntry::dir("./opt/", 0o755))
            .entry(TestEntry::file("./opt/Demo/demo", 0o755, &[0; 2048]))
            .entry(TestEntry::file("./opt/Demo/chrome-sandbox", 0o4755, b"x"))
            .entry(TestEntry::file("./usr/share/applications/demo.desktop", 0o644, b"[Desktop Entry]\n"))
            .entry(TestEntry::file("./usr/share/icons/hicolor/256x256/apps/demo.png", 0o644, b"png"))
            .entry(TestEntry::symlink("./usr/bin/demo", "/opt/Demo/demo"))
            .build();
        let mut deb = Deb::from_reader(Cursor::new(bytes)).unwrap();
        let entries = deb.data_entries().unwrap();
        let text = report(&deb, &entries);

        let expected = [
            "Package:        demo",
            "Version:        1:2.0-1",
            "Pacman version: 1:2.0-1",
            "Archives:       control xz, data xz",
            "Description:    Demo app\n                Longer text.\n\n                Second paragraph.\n",
            "\nDepends:\n  libc6 (>= 2.34)\n  libasound2 | libasound2t64\n",
            "\nRecommends:\n  qemu-system-x86\n",
            "Files: 4 files, 1 directory, 1 symlink, 2.0 KiB",
            "  /opt/Demo",
            "\nDesktop entries:\n  /usr/share/applications/demo.desktop\n",
            "\nIcons:\n  /usr/share/icons/hicolor/256x256/apps/demo.png\n",
            "\nSetuid/setgid:\n  4755 /opt/Demo/chrome-sandbox\n",
            "\nSymlinks:\n  /usr/bin/demo -> /opt/Demo/demo\n",
            "  postinst (2 lines)\n    | #!/bin/sh\n    | update-alternatives --install",
            "\nOther control files: md5sums\n",
        ];
        for want in expected {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
        assert!(!text.contains("Depends: libc6"), "relations should not repeat in the field list");
    }

    #[test]
    fn shows_unparseable_relations_instead_of_failing() {
        let bytes = DebBuilder::new("Package: a\nVersion: v1\nArchitecture: all\nDepends: foo (>= 1\n").build();
        let mut deb = Deb::from_reader(Cursor::new(bytes)).unwrap();
        let entries = deb.data_entries().unwrap();
        let text = report(&deb, &entries);
        assert!(text.contains("cannot parse: unclosed '('"), "{text}");
        assert!(text.contains("Pacman version: cannot map: version 'v1'"), "{text}");
        assert!(text.contains("Maintainer scripts:\n  none\n"), "{text}");
    }
}
