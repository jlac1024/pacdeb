// SPDX-License-Identifier: AGPL-3.0-or-later
//! Builds small synthetic .deb files in memory for tests.

use std::io::Write;

use super::Compression;

pub enum TestEntry {
    Dir(String, u32),
    File(String, u32, Vec<u8>),
    Symlink(String, String),
    Hardlink(String, String),
}

impl TestEntry {
    pub fn dir(path: &str, mode: u32) -> Self {
        TestEntry::Dir(path.into(), mode)
    }
    pub fn file(path: &str, mode: u32, data: &[u8]) -> Self {
        TestEntry::File(path.into(), mode, data.to_vec())
    }
    pub fn symlink(path: &str, target: &str) -> Self {
        TestEntry::Symlink(path.into(), target.into())
    }
    pub fn hardlink(path: &str, target: &str) -> Self {
        TestEntry::Hardlink(path.into(), target.into())
    }
}

pub fn tar_bytes(entries: &[TestEntry]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for e in entries {
        let mut h = tar::Header::new_gnu();
        h.set_uid(0);
        h.set_gid(0);
        h.set_mtime(0);
        match e {
            TestEntry::Dir(path, mode) => {
                h.set_entry_type(tar::EntryType::Directory);
                h.set_mode(*mode);
                h.set_size(0);
                b.append_data(&mut h, path, std::io::empty()).unwrap();
            }
            TestEntry::File(path, mode, data) => {
                h.set_entry_type(tar::EntryType::Regular);
                h.set_mode(*mode);
                h.set_size(data.len() as u64);
                b.append_data(&mut h, path, &data[..]).unwrap();
            }
            TestEntry::Symlink(path, target) => {
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_mode(0o777);
                h.set_size(0);
                b.append_link(&mut h, path, target).unwrap();
            }
            TestEntry::Hardlink(path, target) => {
                h.set_entry_type(tar::EntryType::Link);
                h.set_mode(0o755);
                h.set_size(0);
                b.append_link(&mut h, path, target).unwrap();
            }
        }
    }
    b.into_inner().unwrap()
}

pub fn compress(data: &[u8], c: Compression) -> Vec<u8> {
    match c {
        Compression::None => data.to_vec(),
        Compression::Gzip => {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            e.write_all(data).unwrap();
            e.finish().unwrap()
        }
        Compression::Xz => {
            let mut e = xz2::write::XzEncoder::new(Vec::new(), 1);
            e.write_all(data).unwrap();
            e.finish().unwrap()
        }
        Compression::Zstd => zstd::encode_all(data, 1).unwrap(),
    }
}

pub fn ar_bytes(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (name, data) in members {
        let header = format!("{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", 0, 0, 0, "100644", data.len());
        assert_eq!(header.len(), 60, "member name too long: {name}");
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

fn suffix(c: Compression) -> &'static str {
    match c {
        Compression::None => "",
        Compression::Gzip => ".gz",
        Compression::Xz => ".xz",
        Compression::Zstd => ".zst",
    }
}

pub struct DebBuilder {
    pub control: String,
    /// Extra control archive files such as maintainer scripts: (name, contents).
    pub control_files: Vec<(String, String)>,
    pub data: Vec<TestEntry>,
    pub control_compression: Compression,
    pub data_compression: Compression,
}

impl DebBuilder {
    pub fn new(control: &str) -> Self {
        DebBuilder {
            control: control.into(),
            control_files: Vec::new(),
            data: Vec::new(),
            control_compression: Compression::Xz,
            data_compression: Compression::Xz,
        }
    }

    pub fn control_file(mut self, name: &str, contents: &str) -> Self {
        self.control_files.push((name.into(), contents.into()));
        self
    }

    pub fn entry(mut self, e: TestEntry) -> Self {
        self.data.push(e);
        self
    }

    pub fn compression(mut self, control: Compression, data: Compression) -> Self {
        self.control_compression = control;
        self.data_compression = data;
        self
    }

    pub fn build(&self) -> Vec<u8> {
        let mut control = vec![
            TestEntry::dir("./", 0o755),
            TestEntry::file("./control", 0o644, self.control.as_bytes()),
        ];
        for (name, contents) in &self.control_files {
            control.push(TestEntry::file(&format!("./{name}"), 0o755, contents.as_bytes()));
        }
        let control = compress(&tar_bytes(&control), self.control_compression);
        let data = compress(&tar_bytes(&self.data), self.data_compression);
        let control_name = format!("control.tar{}", suffix(self.control_compression));
        let data_name = format!("data.tar{}", suffix(self.data_compression));
        ar_bytes(&[
            ("debian-binary", b"2.0\n"),
            (&control_name, &control),
            (&data_name, &data),
        ])
    }
}
