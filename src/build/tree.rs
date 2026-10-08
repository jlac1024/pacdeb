//! Writes the package's file tree to disk from the deb, as the model describes it.

use std::collections::HashMap;
use std::fs::{self, File, Permissions};
use std::io::{self, Read, Seek};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use crate::deb::{Deb, EntryKind};
use crate::error::{Context, Result, bail};
use crate::model::{NodeKind, Package, Source};
use crate::progress::{Counting, Progress};

/// Builds the tree under `root`, which must not exist yet. Files keep their modes,
/// setuid included; ownership is left to the backend, which makes everything root.
pub fn write<R: Read + Seek>(deb: &mut Deb<R>, pkg: &Package, root: &Path) -> Result<()> {
    fs::create_dir_all(root).context(root.display())?;
    let at = |p: &str| -> PathBuf { root.join(p.trim_start_matches('/')) };

    // Directories first, writable for now; their real modes go on at the end in case
    // one of them is read only.
    for n in pkg.nodes.iter().filter(|n| n.is_dir()) {
        fs::create_dir_all(at(&n.path)).context(&n.path)?;
    }

    // Every node whose content comes from a given deb entry. One entry can feed several
    // nodes when a script copied it.
    let mut wanted: HashMap<&str, Vec<&str>> = HashMap::new();
    for n in pkg.nodes.iter().filter(|n| n.kind == NodeKind::File) {
        match &n.source {
            Source::Deb(src) => wanted.entry(src.as_str()).or_default().push(&n.path),
            Source::Inline(bytes) => fs::write(at(&n.path), bytes).context(&n.path)?,
            Source::None => bail!("{}: a file with no content source", n.path),
        }
    }

    let total: u64 = pkg.nodes.iter().filter(|n| n.kind == NodeKind::File && matches!(n.source, Source::Deb(_))).map(|n| n.size).sum();
    let mut progress = Progress::new(format!("Unpacking {}", pkg.name), Some(total));
    let mut written = 0;
    deb.scan_data(|e, r| {
        if e.kind != EntryKind::File {
            return Ok(());
        }
        let Some(targets) = wanted.get(e.path.as_str()) else {
            return Ok(());
        };
        let first = at(targets[0]);
        let mut out = File::create(&first).context(first.display())?;
        io::copy(&mut Counting { inner: r, progress: &mut progress }, &mut out).context(first.display())?;
        for t in &targets[1..] {
            let copied = fs::copy(&first, at(t)).context(t)?;
            progress.add(copied);
        }
        written += targets.len();
        Ok(())
    })?;
    let expected: usize = wanted.values().map(Vec::len).sum();
    if written != expected {
        bail!("the deb is missing {} of the files the package needs", expected - written);
    }
    progress.finish();

    for n in &pkg.nodes {
        match &n.kind {
            NodeKind::Symlink(target) => symlink(target, at(&n.path)).context(&n.path)?,
            NodeKind::Hardlink(target) => fs::hard_link(at(target), at(&n.path)).context(&n.path)?,
            NodeKind::File => fs::set_permissions(at(&n.path), Permissions::from_mode(n.mode)).context(&n.path)?,
            NodeKind::Dir => {}
        }
    }
    // Deepest first, so a read only parent does not block its children.
    for n in pkg.nodes.iter().rev().filter(|n| n.is_dir()) {
        fs::set_permissions(at(&n.path), Permissions::from_mode(n.mode)).context(&n.path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deb::testutil::{DebBuilder, TestEntry};
    use crate::translate;
    use std::io::Cursor;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn writes_the_translated_tree() {
        let bytes = DebBuilder::new("Package: demo\nVersion: 1.0\nArchitecture: amd64\n")
            .control_file(
                "postinst",
                "#!/bin/sh\nln -s /opt/Demo/demo /usr/bin/demo\ncp /opt/Demo/mime.xml /usr/share/mime/packages/demo.xml\n\
                 cat > /usr/share/demo/note.txt <<'EOF'\nhello\nEOF\n",
            )
            .entry(TestEntry::dir("./opt/", 0o755))
            .entry(TestEntry::dir("./opt/Demo/", 0o755))
            .entry(TestEntry::file("./opt/Demo/demo", 0o755, b"#!/bin/sh\necho demo\n"))
            .entry(TestEntry::file("./opt/Demo/chrome-sandbox", 0o755, b"sandbox"))
            .entry(TestEntry::file("./opt/Demo/mime.xml", 0o644, b"<mime/>"))
            .entry(TestEntry::file("./bin/helper", 0o755, b"helper"))
            .entry(TestEntry::hardlink("./opt/Demo/demo2", "./opt/Demo/demo"))
            .build();
        let mut deb = Deb::from_reader(Cursor::new(bytes)).unwrap();
        let t = translate::translate(&mut deb, &translate::Tables::builtin(), 1, &translate::tests::BareSystem).unwrap();

        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("build/sandbox/test-tree");
        let _ = fs::remove_dir_all(&root);
        write(&mut deb, &t.package, &root).unwrap();

        let read = |p: &str| fs::read_to_string(root.join(p)).unwrap();
        let mode = |p: &str| fs::symlink_metadata(root.join(p)).unwrap().mode() & 0o7777;
        assert_eq!(read("opt/Demo/demo"), "#!/bin/sh\necho demo\n");
        assert_eq!(read("usr/share/mime/packages/demo.xml"), "<mime/>");
        assert_eq!(read("usr/share/demo/note.txt"), "hello\n");
        assert_eq!(read("usr/bin/helper"), "helper");
        assert_eq!(fs::read_link(root.join("usr/bin/demo")).unwrap(), Path::new("/opt/Demo/demo"));
        assert_eq!(mode("opt/Demo/chrome-sandbox"), 0o4755);
        assert_eq!(mode("opt/Demo/demo"), 0o755);
        assert_eq!(mode("usr/share/demo/note.txt"), 0o644);
        assert_eq!(
            fs::metadata(root.join("opt/Demo/demo2")).unwrap().ino(),
            fs::metadata(root.join("opt/Demo/demo")).unwrap().ino()
        );
        assert!(!root.join("bin").exists());
        fs::remove_dir_all(&root).unwrap();
    }
}
