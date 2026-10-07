//! Reading .deb files: the ar container, the control archive and the data archive.

mod ar;
mod data;
mod decompress;
#[cfg(test)]
pub mod testutil;

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use crate::control::Control;
use crate::error::{Context, Result, bail};

pub use data::{DataEntry, EntryKind};
pub use decompress::Compression;

pub const MAINTAINER_SCRIPTS: [&str; 4] = ["preinst", "postinst", "prerm", "postrm"];

// Control archives are a few KiB. The cap only stops a corrupt size field from
// making us allocate gigabytes.
const CONTROL_LIMIT: u64 = 64 << 20;

pub struct Deb<R> {
    reader: R,
    pub control: Control,
    /// Every regular file in the control archive by name ("control", "postinst", "md5sums", ...).
    pub control_files: BTreeMap<String, Vec<u8>>,
    pub control_compression: Compression,
    pub data_compression: Compression,
    data_member: ar::Member,
}

impl Deb<BufReader<File>> {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).context(format!("cannot open {}", path.display()))?;
        Deb::from_reader(BufReader::new(file)).context(path.display())
    }
}

impl<R: Read + Seek> Deb<R> {
    pub fn from_reader(mut reader: R) -> Result<Self> {
        let members = ar::read_members(&mut reader)?;
        let Some(first) = members.first() else {
            bail!("empty archive, not a .deb");
        };
        if first.name != "debian-binary" {
            bail!("first member is '{}', expected debian-binary", first.name);
        }
        check_format(&read_member(&mut reader, first, 64)?)?;

        let mut control = None;
        let mut data = None;
        for m in &members[1..] {
            // Members starting with "_" are reserved for local additions and skipped by dpkg too.
            if m.name.starts_with('_') {
                continue;
            }
            if let Some(suffix) = m.name.strip_prefix("control.tar") {
                if control.is_some() || data.is_some() {
                    bail!("unexpected '{}' member", m.name);
                }
                control = Some((m, Compression::from_suffix(&m.name, suffix)?));
            } else if let Some(suffix) = m.name.strip_prefix("data.tar") {
                if control.is_none() || data.is_some() {
                    bail!("unexpected '{}' member", m.name);
                }
                data = Some((m, Compression::from_suffix(&m.name, suffix)?));
            } else {
                bail!("unknown member '{}', not a valid .deb", m.name);
            }
        }
        let Some((control_member, control_compression)) = control else {
            bail!("no control.tar member");
        };
        let Some((data_member, data_compression)) = data else {
            bail!("no data.tar member");
        };

        let raw = read_member(&mut reader, control_member, CONTROL_LIMIT)?;
        let control_files = read_control_archive(&raw, control_compression)?;
        let Some(control_text) = control_files.get("control") else {
            bail!("control archive has no control file");
        };
        let control =
            Control::parse(&String::from_utf8_lossy(control_text)).context("control file")?;
        for field in ["Package", "Version", "Architecture"] {
            control.require(field)?;
        }

        Ok(Deb {
            data_member: data_member.clone(),
            reader,
            control,
            control_files,
            control_compression,
            data_compression,
        })
    }

    /// A decompressed stream of the data tar.
    pub fn data_reader(&mut self) -> Result<Box<dyn Read + '_>> {
        self.reader.seek(SeekFrom::Start(self.data_member.offset))?;
        let limited = (&mut self.reader).take(self.data_member.size);
        self.data_compression.decoder(limited)
    }

    pub fn data_entries(&mut self) -> Result<Vec<DataEntry>> {
        let reader = self.data_reader()?;
        data::list(reader)
    }
}

fn check_format(raw: &[u8]) -> Result<()> {
    let text = String::from_utf8_lossy(raw);
    let version = text.trim();
    if version.split('.').next() != Some("2") {
        bail!("unsupported deb format '{version}', only 2.x is supported");
    }
    Ok(())
}

fn read_member<R: Read + Seek>(r: &mut R, m: &ar::Member, limit: u64) -> Result<Vec<u8>> {
    if m.size > limit {
        bail!("member '{}' is {} bytes, larger than expected", m.name, m.size);
    }
    r.seek(SeekFrom::Start(m.offset))?;
    let mut buf = Vec::with_capacity(m.size as usize);
    r.take(m.size).read_to_end(&mut buf)?;
    Ok(buf)
}

fn read_control_archive(raw: &[u8], c: Compression) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut archive = tar::Archive::new(c.decoder(raw)?);
    let mut files = BTreeMap::new();
    for entry in archive.entries().context("reading control archive")? {
        let mut entry = entry.context("reading control archive")?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let Some(path) = data::normalize_path(&entry.path_bytes())? else {
            continue;
        };
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).context("reading control archive")?;
        files.insert(path.trim_start_matches('/').to_string(), buf);
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::testutil::{DebBuilder, TestEntry, ar_bytes};
    use super::*;
    use std::io::Cursor;

    const CONTROL: &str = "Package: demo\nVersion: 1.0-1\nArchitecture: amd64\n";

    fn open(bytes: Vec<u8>) -> Result<Deb<Cursor<Vec<u8>>>> {
        Deb::from_reader(Cursor::new(bytes))
    }

    #[test]
    fn reads_every_compression_pair() {
        use Compression::*;
        for control in [None, Gzip, Xz, Zstd] {
            for data in [None, Gzip, Xz, Zstd] {
                let bytes = DebBuilder::new(CONTROL)
                    .compression(control, data)
                    .control_file("postinst", "#!/bin/sh\nexit 0\n")
                    .entry(TestEntry::dir("./", 0o755))
                    .entry(TestEntry::file("./usr/bin/demo", 0o755, b"hi"))
                    .build();
                let mut deb = open(bytes).unwrap();
                assert_eq!(deb.control.get("Package"), Some("demo"));
                assert_eq!(deb.control_compression, control);
                assert_eq!(deb.data_compression, data);
                assert!(deb.control_files.contains_key("postinst"));
                let paths: Vec<_> = deb.data_entries().unwrap().into_iter().map(|e| e.path).collect();
                assert_eq!(paths, ["/usr/bin/demo"], "{control}/{data}");
            }
        }
    }

    #[test]
    fn rejects_bad_layouts() {
        let control = DebBuilder::new(CONTROL).build();
        let members = crate::deb::ar::read_members(&mut Cursor::new(&control)).unwrap();
        let part = |i: usize| -> Vec<u8> {
            let m = &members[i];
            control[m.offset as usize..][..m.size as usize].to_vec()
        };
        let (ctl, data) = (part(1), part(2));

        let cases: Vec<(Vec<u8>, &str)> = vec![
            (ar_bytes(&[]), "empty archive"),
            (ar_bytes(&[("control.tar.xz", &ctl)]), "expected debian-binary"),
            (ar_bytes(&[("debian-binary", b"3.0\n")]), "unsupported deb format"),
            (ar_bytes(&[("debian-binary", b"2.0\n"), ("data.tar.xz", &data)]), "unexpected 'data.tar.xz'"),
            (ar_bytes(&[("debian-binary", b"2.0\n"), ("control.tar.xz", &ctl)]), "no data.tar"),
            (ar_bytes(&[("debian-binary", b"2.0\n"), ("control.tar.xz", &ctl), ("data.tar.bz2", &data)]), "unsupported compression"),
            (ar_bytes(&[("debian-binary", b"2.0\n"), ("control.tar.xz", &ctl), ("extra", b"x"), ("data.tar.xz", &data)]), "unknown member 'extra'"),
        ];
        for (bytes, want) in cases {
            let err = open(bytes).err().expect(want).to_string();
            assert!(err.contains(want), "expected '{want}', got '{err}'");
        }
    }

    #[test]
    fn skips_underscore_members() {
        let good = DebBuilder::new(CONTROL).build();
        let members = crate::deb::ar::read_members(&mut Cursor::new(&good)).unwrap();
        let part = |i: usize| good[members[i].offset as usize..][..members[i].size as usize].to_vec();
        let bytes = ar_bytes(&[
            ("debian-binary", b"2.0\n"),
            ("_gpgorigin", b"sig"),
            ("control.tar.xz", &part(1)),
            ("data.tar.xz", &part(2)),
        ]);
        assert!(open(bytes).is_ok());
    }

    #[test]
    fn requires_control_file_and_core_fields() {
        let cases = [
            ("", "control file is empty"),
            ("Version: 1\nArchitecture: amd64\n", "no Package field"),
            ("Package: a\nArchitecture: amd64\n", "no Version field"),
            ("Package: a\nVersion: 1\nArchitecture:\n", "no Architecture field"),
        ];
        for (control, want) in cases {
            let err = open(DebBuilder::new(control).build()).err().unwrap().to_string();
            assert!(err.contains(want), "expected '{want}', got '{err}'");
        }
    }

    /// Opens every deb Jeff put in references/debs. Ignored by default because those
    /// files are large; run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "reads the large sample debs in references/debs"]
    fn opens_reference_debs() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("references/debs");
        let mut seen = 0;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "deb") {
                continue;
            }
            let mut deb = Deb::open(&path).unwrap();
            for field in ["Package", "Version", "Architecture"] {
                assert!(deb.control.get(field).is_some(), "{}: no {field}", path.display());
            }
            assert!(!deb.data_entries().unwrap().is_empty(), "{}", path.display());
            seen += 1;
        }
        assert!(seen > 0, "no .deb files in {}", dir.display());
    }
}
