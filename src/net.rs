//! HTTP for feeds, repository indexes and downloads.

use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256, Sha512};

use crate::error::{Context, Error, Result, bail};

/// Feeds and indexes are small; anything past this is not what we asked for.
const TEXT_LIMIT: u64 = 64 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checksum {
    Sha256(String),
    Sha512(String),
}

impl Checksum {
    /// Picks the algorithm from the length of a hex digest.
    pub fn from_hex(hex: &str) -> Option<Checksum> {
        let hex = hex.trim().to_ascii_lowercase();
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        match hex.len() {
            64 => Some(Checksum::Sha256(hex)),
            128 => Some(Checksum::Sha512(hex)),
            _ => None,
        }
    }

    pub fn matches(&self, data: &[u8]) -> bool {
        match self {
            Checksum::Sha256(want) => hex(&Sha256::digest(data)) == *want,
            Checksum::Sha512(want) => hex(&Sha512::digest(data)) == *want,
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        .user_agent(concat!("pacdeb/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn get(url: &str, headers: &[(&str, &str)]) -> Result<ureq::http::Response<ureq::Body>> {
    let mut req = agent().get(url);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    req.call().map_err(|e| Error::new(format!("cannot fetch {url}: {}", describe(&e))))
}

/// ureq's messages name its internals; these say what went wrong and what to try.
fn describe(e: &ureq::Error) -> String {
    use ureq::Error as E;
    match e {
        E::StatusCode(404 | 410) => "the server has no such file (404); check the URL".into(),
        E::StatusCode(c @ (401 | 403)) => format!("the server refused access ({c})"),
        E::StatusCode(429) => "the server is rate limiting requests (429); try again later".into(),
        E::StatusCode(c) if *c >= 500 => format!("the server had an error ({c}); try again later"),
        E::StatusCode(c) => format!("the server answered with status {c}"),
        E::HostNotFound => "host not found; check the URL and your internet connection".into(),
        E::Io(io) if io.to_string().contains("lookup address") => {
            "host not found; check the URL and your internet connection".into()
        }
        E::ConnectionFailed => "could not connect; check your internet connection".into(),
        E::Timeout(_) => "the server took too long to answer; try again later".into(),
        E::BadUri(_) => "this is not a valid URL".into(),
        other => crate::error::plain(other),
    }
}

/// Whether a fetch failed because the server has no such file.
pub fn not_found(e: &Error) -> bool {
    e.to_string().contains("no such file (404)")
}

pub fn get_bytes(url: &str, headers: &[(&str, &str)]) -> Result<Vec<u8>> {
    let mut resp = get(url, headers)?;
    resp.body_mut().with_config().limit(TEXT_LIMIT).read_to_vec().context(format!("reading {url}"))
}

pub fn get_text(url: &str, headers: &[(&str, &str)]) -> Result<String> {
    String::from_utf8(get_bytes(url, headers)?).context(format!("{url} is not text"))
}

/// What a server says about a file without sending it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Head {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

pub fn head(url: &str) -> Result<Head> {
    let resp = agent().head(url).call().map_err(|e| Error::new(format!("cannot reach {url}: {}", describe(&e))))?;
    let header = |name: &str| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(String::from);
    Ok(Head { etag: header("etag"), last_modified: header("last-modified") })
}

/// Downloads `url` to `dest`, checking it against `checksum` when given. The file only
/// appears at `dest` once it is complete and checked.
pub fn download(url: &str, dest: &Path, checksum: Option<&Checksum>) -> Result<()> {
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).context(dir.display())?;
    }
    let part = dest.with_extension("part");
    let resp = get(url, &[])?;
    let mut reader = resp.into_body().into_reader();
    let mut out = BufWriter::new(File::create(&part).context(part.display())?);
    let (mut s256, mut s512) = (Sha256::new(), Sha512::new());
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = reader.read(&mut buf).context(format!("downloading {url}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).context(part.display())?;
        match checksum {
            Some(Checksum::Sha256(_)) => s256.update(&buf[..n]),
            Some(Checksum::Sha512(_)) => s512.update(&buf[..n]),
            None => {}
        }
    }
    out.flush().context(part.display())?;
    drop(out);
    let ok = match checksum {
        Some(Checksum::Sha256(want)) => hex(&s256.finalize()) == *want,
        Some(Checksum::Sha512(want)) => hex(&s512.finalize()) == *want,
        None => true,
    };
    if !ok {
        let _ = fs::remove_file(&part);
        bail!("{url} does not match its published checksum; the download was deleted");
    }
    fs::rename(&part, dest).context(dest.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums() {
        let cases = [
            ("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", true),
            ("BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD", true),
            ("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ae", false),
        ];
        for (hex, ok) in cases {
            assert_eq!(Checksum::from_hex(hex).unwrap().matches(b"abc"), ok, "{hex}");
        }
        let sha512_abc = "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f";
        assert!(Checksum::from_hex(sha512_abc).unwrap().matches(b"abc"));
        assert_eq!(Checksum::from_hex("abc"), None);
        assert_eq!(Checksum::from_hex(&"z".repeat(64)), None);
    }
}
