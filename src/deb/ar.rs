// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reader for the `ar` container that wraps every .deb.

use std::io::{ErrorKind, Read, Seek, SeekFrom};

use crate::error::{Error, Result, bail};

const MAGIC: &[u8; 8] = b"!<arch>\n";
const HEADER_LEN: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    /// Byte offset of the member's data in the archive.
    pub offset: u64,
    pub size: u64,
}

pub fn read_members<R: Read + Seek>(r: &mut R) -> Result<Vec<Member>> {
    let len = r.seek(SeekFrom::End(0))?;
    r.seek(SeekFrom::Start(0))?;

    let mut magic = [0u8; 8];
    if read_full(r, &mut magic)? != magic.len() || &magic != MAGIC {
        bail!("not an ar archive, so not a .deb");
    }

    let mut members = Vec::new();
    let mut pos = MAGIC.len() as u64;
    loop {
        // Members start on even offsets. Some tools leave out the pad byte after the
        // last member, so running out of data here is a normal end.
        if pos % 2 == 1 {
            pos += 1;
        }
        if pos >= len {
            break;
        }
        r.seek(SeekFrom::Start(pos))?;
        let mut header = [0u8; HEADER_LEN];
        if read_full(r, &mut header)? < HEADER_LEN {
            bail!("ar archive is truncated (partial header at offset {pos})");
        }
        let (name, size) = parse_header(&header, pos)?;
        let offset = pos + HEADER_LEN as u64;
        if offset + size > len {
            bail!("ar member '{name}' runs past the end of the file, the download may be incomplete");
        }
        members.push(Member { name, offset, size });
        pos = offset + size;
    }
    Ok(members)
}

fn parse_header(h: &[u8; HEADER_LEN], pos: u64) -> Result<(String, u64)> {
    if &h[58..60] != b"`\n" {
        bail!("bad ar header at offset {pos}");
    }
    let name = std::str::from_utf8(&h[0..16])
        .map_err(|_| Error::new(format!("bad ar member name at offset {pos}")))?
        .trim_end()
        // GNU ar ends names with a slash.
        .trim_end_matches('/');
    if name.is_empty() {
        bail!("empty ar member name at offset {pos}");
    }
    let size_field = std::str::from_utf8(&h[48..58]).unwrap_or("").trim();
    let size = size_field
        .parse()
        .map_err(|_| Error::new(format!("bad ar member size '{size_field}' at offset {pos}")))?;
    Ok((name.to_string(), size))
}

fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deb::testutil::ar_bytes;
    use std::io::Cursor;

    fn names(members: &[Member]) -> Vec<(&str, u64)> {
        members.iter().map(|m| (m.name.as_str(), m.size)).collect()
    }

    #[test]
    fn reads_members_with_odd_sizes() {
        let bytes = ar_bytes(&[
            ("debian-binary", &b"2.0\n"[..]),
            ("odd", &b"abc"[..]),
            ("last", &b"xy"[..]),
        ]);
        let members = read_members(&mut Cursor::new(&bytes)).unwrap();
        assert_eq!(names(&members), [("debian-binary", 4), ("odd", 3), ("last", 2)]);
        let odd = &members[1];
        assert_eq!(&bytes[odd.offset as usize..][..3], b"abc");
    }

    #[test]
    fn accepts_gnu_names_and_missing_final_pad() {
        let mut bytes = ar_bytes(&[("data.tar.xz/", &b"abc"[..])]);
        assert_eq!(bytes.pop(), Some(b'\n'));
        let members = read_members(&mut Cursor::new(&bytes)).unwrap();
        assert_eq!(names(&members), [("data.tar.xz", 3)]);
    }

    #[test]
    fn rejects_broken_archives() {
        let good = ar_bytes(&[("a", &b"hello"[..])]);
        let mut truncated = good.clone();
        truncated.truncate(good.len() - 3);
        let mut bad_fmag = good.clone();
        bad_fmag[8 + 58] = b'x';
        let mut bad_size = good.clone();
        bad_size[8 + 48] = b'z';

        let cases: &[(&[u8], &str)] = &[
            (b"", "not an ar archive"),
            (b"!<arch>", "not an ar archive"),
            (b"PK\x03\x04 not ar", "not an ar archive"),
            (&good[..20], "truncated"),
            (&truncated, "runs past the end"),
            (&bad_fmag, "bad ar header"),
            (&bad_size, "bad ar member size"),
        ];
        for (input, want) in cases {
            let err = read_members(&mut Cursor::new(input)).unwrap_err().to_string();
            assert!(err.contains(want), "expected '{want}', got '{err}'");
        }
    }
}
