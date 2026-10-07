//! Apt repositories: a signed Release file listing package indexes, and Packages
//! indexes listing the debs.

use std::io::Read;

use crate::control::Control;
use crate::error::{Context, Result, bail};
use crate::net::Checksum;
use crate::version::DebVersion;

/// One package index file named in Release.
#[derive(Debug, PartialEq, Eq)]
pub struct IndexFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// Picks the Packages index for a component and architecture from Release, preferring
/// the smallest compressed form pacdeb can read.
pub fn index_file(release: &str, component: &str, arch: &str) -> Result<IndexFile> {
    let control = Control::parse(release).context("the repository's Release file")?;
    let Some(list) = control.get("SHA256") else {
        bail!("the repository's Release file has no SHA256 list");
    };
    let base = format!("{component}/binary-{arch}/Packages");
    let files: Vec<IndexFile> = list
        .lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let (hash, size, path) = (parts.next()?, parts.next()?, parts.next()?);
            Some(IndexFile { path: path.to_string(), size: size.parse().ok()?, sha256: hash.to_ascii_lowercase() })
        })
        .collect();
    for ext in [".xz", ".gz", ""] {
        if let Some(f) = files.iter().find(|f| f.path == format!("{base}{ext}")) {
            return Ok(IndexFile { path: f.path.clone(), size: f.size, sha256: f.sha256.clone() });
        }
    }
    bail!("the repository has no {base} index for this architecture");
}

/// Checks an index against Release and unpacks it.
pub fn read_index(file: &IndexFile, raw: &[u8]) -> Result<String> {
    if raw.len() as u64 != file.size || !Checksum::Sha256(file.sha256.clone()).matches(raw) {
        bail!("{} does not match the signed Release file", file.path);
    }
    let mut text = String::new();
    if file.path.ends_with(".xz") {
        xz2::read::XzDecoder::new(raw).read_to_string(&mut text)
    } else if file.path.ends_with(".gz") {
        flate2::read::GzDecoder::new(raw).read_to_string(&mut text)
    } else {
        return String::from_utf8(raw.to_vec()).context(&file.path);
    }
    .context(&file.path)?;
    Ok(text)
}

#[derive(Debug, PartialEq, Eq)]
pub struct Candidate {
    pub version: String,
    pub filename: String,
    pub sha256: Option<String>,
}

/// The highest version of `package` for `arch` (or arch independent) in an index.
pub fn pick(index: &str, package: &str, arch: &str) -> Result<Option<Candidate>> {
    let mut best: Option<(DebVersion, Candidate)> = None;
    for para in paragraphs(index) {
        let c = Control::parse(&para).context("the repository's package index")?;
        if c.get("Package") != Some(package) || !matches!(c.get("Architecture"), Some(a) if a == arch || a == "all") {
            continue;
        }
        let (Some(version), Some(filename)) = (c.get("Version"), c.get("Filename")) else {
            continue;
        };
        let Ok(parsed) = DebVersion::parse(version) else {
            continue;
        };
        if best.as_ref().is_none_or(|(b, _)| parsed > *b) {
            let sha256 = c.get("SHA256").map(|s| s.to_ascii_lowercase());
            best = Some((parsed, Candidate { version: version.to_string(), filename: filename.to_string(), sha256 }));
        }
    }
    Ok(best.map(|(_, c)| c))
}

fn paragraphs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push_str(line);
            cur.push('\n');
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The machine's Debian architecture name.
pub fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        _ => "amd64",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sources").join(name)).unwrap()
    }

    /// The signed part of the fixture InRelease, without gpg.
    fn release_text() -> String {
        let text = String::from_utf8(fixture("example-InRelease")).unwrap();
        let body = text.split_once("\n\n").unwrap().1;
        body.split("-----BEGIN PGP SIGNATURE-----").next().unwrap().to_string()
    }

    #[test]
    fn finds_and_reads_the_example_index() {
        let file = index_file(&release_text(), "main", "amd64").unwrap();
        assert_eq!(file.path, "main/binary-amd64/Packages.gz");
        assert_eq!(file.size, 6306);
        let index = read_index(&file, &fixture("example-Packages.gz")).unwrap();
        let c = pick(&index, "example-app", "amd64").unwrap().unwrap();
        assert!(c.filename.starts_with("pool/main/e/example-app/example-app_"), "{c:?}");
        assert!(c.filename.ends_with("_amd64.deb"), "{c:?}");
        assert_eq!(c.sha256.as_ref().map(String::len), Some(64));
        // Every other listed version is lower.
        let newest = DebVersion::parse(&c.version).unwrap();
        for para in paragraphs(&index) {
            let v = Control::parse(&para).unwrap().get("Version").map(|v| DebVersion::parse(v).unwrap());
            assert!(v.is_none_or(|v| v <= newest));
        }
        assert_eq!(pick(&index, "not-there", "amd64").unwrap(), None);
    }

    #[test]
    fn refuses_a_tampered_index() {
        let file = index_file(&release_text(), "main", "amd64").unwrap();
        let mut raw = fixture("example-Packages.gz");
        raw[100] ^= 1;
        assert!(read_index(&file, &raw).unwrap_err().to_string().contains("does not match the signed Release"));
        assert!(index_file(&release_text(), "main", "s390x").is_err());
    }

    #[test]
    fn picks_the_highest_version_for_the_arch() {
        let index = "\
Package: app\nVersion: 1.2-1\nArchitecture: amd64\nFilename: pool/app_1.2-1_amd64.deb\n\n\
Package: app\nVersion: 1.10-1\nArchitecture: amd64\nFilename: pool/app_1.10-1_amd64.deb\n\n\
Package: app\nVersion: 2.0-1\nArchitecture: arm64\nFilename: pool/app_2.0-1_arm64.deb\n\n\
Package: other\nVersion: 9\nArchitecture: all\nFilename: pool/other_9_all.deb\n";
        let c = pick(index, "app", "amd64").unwrap().unwrap();
        assert_eq!((c.version.as_str(), c.filename.as_str()), ("1.10-1", "pool/app_1.10-1_amd64.deb"));
        assert_eq!(pick(index, "other", "amd64").unwrap().unwrap().version, "9");
    }
}
