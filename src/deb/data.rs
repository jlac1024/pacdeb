// SPDX-License-Identifier: AGPL-3.0-or-later
//! Listing the contents of a deb's data archive.

use std::io::Read;

use crate::error::{Context, Error, Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Hardlink,
    /// Device nodes, fifos and the like, keyed by the tar type byte.
    Other(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataEntry {
    /// Absolute path inside the package, like "/usr/bin/foo". The root itself is never listed.
    pub path: String,
    pub kind: EntryKind,
    pub mode: u32,
    pub size: u64,
    pub uid: u64,
    pub gid: u64,
    /// Symlink target as written in the archive, or the normalized path a hardlink points to.
    pub link: Option<String>,
}

pub fn list<R: Read>(r: R) -> Result<Vec<DataEntry>> {
    let mut out = Vec::new();
    scan(r, |e, _| {
        out.push(e.clone());
        Ok(())
    })?;
    Ok(out)
}

/// Walks the data tar once, calling `visit` with each entry and a reader for its
/// contents. Contents the visitor does not read are skipped.
pub fn scan<R: Read>(
    r: R,
    mut visit: impl FnMut(&DataEntry, &mut dyn Read) -> Result<()>,
) -> Result<()> {
    let mut archive = tar::Archive::new(r);
    for entry in archive.entries().context("reading data archive")? {
        let mut entry = entry.context("reading data archive")?;
        let Some(path) = normalize_path(&entry.path_bytes())? else {
            continue;
        };
        let header = entry.header();
        let kind = match header.entry_type() {
            tar::EntryType::Regular | tar::EntryType::Continuous => EntryKind::File,
            tar::EntryType::Directory => EntryKind::Dir,
            tar::EntryType::Symlink => EntryKind::Symlink,
            tar::EntryType::Link => EntryKind::Hardlink,
            other => EntryKind::Other(other.as_byte()),
        };
        let link = match (kind, entry.link_name_bytes()) {
            (EntryKind::Symlink, Some(target)) => Some(String::from_utf8_lossy(&target).into_owned()),
            (EntryKind::Hardlink, Some(target)) => normalize_path(&target)?,
            (EntryKind::Symlink | EntryKind::Hardlink, None) => bail!("{path}: link has no target"),
            _ => None,
        };
        let data_entry = DataEntry {
            kind,
            mode: header.mode().context(&path)? & 0o7777,
            size: entry.size(),
            uid: header.uid().context(&path)?,
            gid: header.gid().context(&path)?,
            link,
            path,
        };
        visit(&data_entry, &mut entry).context(&data_entry.path)?;
    }
    Ok(())
}

/// Turns an archive path like "./usr/bin/" into "/usr/bin". Returns None for the root.
/// Paths that climb out with ".." are refused, since later phases write these to disk.
pub fn normalize_path(raw: &[u8]) -> Result<Option<String>> {
    let s = std::str::from_utf8(raw).map_err(|_| {
        Error::new(format!("path is not valid UTF-8: {}", String::from_utf8_lossy(raw)))
    })?;
    let mut parts = Vec::new();
    for part in s.split('/') {
        match part {
            "" | "." => {}
            ".." => bail!("path escapes the package root: {s}"),
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        return Ok(None);
    }
    Ok(Some(format!("/{}", parts.join("/"))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deb::testutil::{TestEntry, tar_bytes};

    #[test]
    fn normalizes_paths() {
        let cases: &[(&str, Option<&str>)] = &[
            ("./", None),
            (".", None),
            ("/", None),
            ("./usr/", Some("/usr")),
            ("./usr/bin/foo", Some("/usr/bin/foo")),
            ("usr//lib/./x", Some("/usr/lib/x")),
            ("/opt/App Name/run", Some("/opt/App Name/run")),
        ];
        for (input, want) in cases {
            let got = normalize_path(input.as_bytes()).unwrap();
            assert_eq!(got.as_deref(), *want, "input '{input}'");
        }
    }

    #[test]
    fn refuses_escaping_paths() {
        for input in ["../etc/passwd", "./usr/../../x"] {
            assert!(normalize_path(input.as_bytes()).is_err(), "input '{input}'");
        }
    }

    #[test]
    fn lists_entries() {
        let tar = tar_bytes(&[
            TestEntry::dir("./", 0o755),
            TestEntry::dir("./usr/", 0o755),
            TestEntry::file("./usr/lib/app/app", 0o755, b"binary"),
            TestEntry::file("./usr/lib/app/chrome-sandbox", 0o4755, b"sandbox"),
            TestEntry::symlink("./usr/bin/app", "../lib/app/app"),
            TestEntry::hardlink("./usr/lib/app/app2", "./usr/lib/app/app"),
        ]);
        let entries = list(&tar[..]).unwrap();
        let got: Vec<_> = entries
            .iter()
            .map(|e| (e.path.as_str(), e.kind, e.mode, e.size, e.link.as_deref()))
            .collect();
        assert_eq!(
            got,
            [
                ("/usr", EntryKind::Dir, 0o755, 0, None),
                ("/usr/lib/app/app", EntryKind::File, 0o755, 6, None),
                ("/usr/lib/app/chrome-sandbox", EntryKind::File, 0o4755, 7, None),
                ("/usr/bin/app", EntryKind::Symlink, 0o777, 0, Some("../lib/app/app")),
                ("/usr/lib/app/app2", EntryKind::Hardlink, 0o755, 0, Some("/usr/lib/app/app")),
            ]
        );
    }
}
