//! Signature checks for apt repositories, with gpg and gpgv run against a private home
//! directory so the user's own keyring is never read or changed.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use crate::error::{Context, Error, Result, bail};

/// The fingerprints of the keys in a key file.
pub fn fingerprints(key: &Path, home: &Path) -> Result<Vec<String>> {
    prepare_home(home)?;
    let out = Command::new("gpg")
        .arg("--homedir")
        .arg(home)
        .args(["--batch", "--show-keys", "--with-colons"])
        .arg(key)
        .env("LC_ALL", "C")
        .output()
        .context("cannot run gpg")?;
    if !out.status.success() {
        bail!("gpg could not read {}: {}", key.display(), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("fpr:"))
        .filter_map(|rest| rest.split(':').nth(8))
        .map(String::from)
        .collect())
}

/// Checks a clearsigned file like apt's InRelease and returns the signed text.
pub fn verify_clearsigned(signed: &[u8], key: &Path, home: &Path) -> Result<Vec<u8>> {
    let keyring = write_keyring(key, home)?;
    let file = home.join("InRelease");
    fs::write(&file, signed).context(file.display())?;
    let out = Command::new("gpgv")
        .arg("--homedir")
        .arg(home)
        .arg("--keyring")
        .arg(&keyring)
        .args(["--output", "-"])
        .arg(&file)
        .env("LC_ALL", "C")
        .output()
        .context("cannot run gpgv")?;
    if !out.status.success() {
        bail!("the repository signature does not check out: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

/// Checks a detached signature, like apt's Release plus Release.gpg.
pub fn verify_detached(data: &[u8], signature: &[u8], key: &Path, home: &Path) -> Result<()> {
    let keyring = write_keyring(key, home)?;
    let (data_file, sig_file) = (home.join("Release"), home.join("Release.gpg"));
    fs::write(&data_file, data).context(data_file.display())?;
    fs::write(&sig_file, signature).context(sig_file.display())?;
    let out = Command::new("gpgv")
        .arg("--homedir")
        .arg(home)
        .arg("--keyring")
        .arg(&keyring)
        .arg(&sig_file)
        .arg(&data_file)
        .env("LC_ALL", "C")
        .output()
        .context("cannot run gpgv")?;
    if !out.status.success() {
        bail!("the repository signature does not check out: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

fn prepare_home(home: &Path) -> Result<()> {
    fs::create_dir_all(home).context(home.display())?;
    fs::set_permissions(home, fs::Permissions::from_mode(0o700)).context(home.display())
}

/// gpgv wants a binary keyring, while keys are usually published ASCII armored.
fn write_keyring(key: &Path, home: &Path) -> Result<std::path::PathBuf> {
    prepare_home(home)?;
    let raw = fs::read(key).context(key.display())?;
    let binary = dearmor(&raw).context(key.display())?;
    let keyring = home.join("keyring.gpg");
    fs::write(&keyring, binary).context(keyring.display())?;
    Ok(keyring)
}

fn dearmor(raw: &[u8]) -> Result<Vec<u8>> {
    let text = String::from_utf8_lossy(raw);
    if !text.trim_start().starts_with("-----BEGIN PGP") {
        return Ok(raw.to_vec());
    }
    let mut body = String::new();
    let mut in_block = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("-----BEGIN PGP") {
            in_block = true;
        } else if line.starts_with("-----END PGP") {
            break;
        } else if in_block && !line.is_empty() && !line.contains(": ") && !line.starts_with('=') {
            body.push_str(line);
        }
    }
    base64_decode(&body)
}

fn base64_decode(s: &str) -> Result<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes().filter(|c| *c != b'=') {
        let v = value(c).ok_or_else(|| Error::new(format!("bad base64 character '{}'", c as char)))?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sources").join(name)
    }

    fn home(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("build/sandbox/test-gpg").join(name)
    }

    #[test]
    fn decodes_base64() {
        let cases = [("", ""), ("Zg==", "f"), ("Zm8=", "fo"), ("Zm9v", "foo"), ("Zm9vYmFy", "foobar")];
        for (input, want) in cases {
            assert_eq!(base64_decode(input).unwrap(), want.as_bytes(), "{input}");
        }
        assert!(base64_decode("Zm9v!").is_err());
    }

    #[test]
    fn reads_the_example_key_fingerprint() {
        let fprs = fingerprints(&fixture("example-key.asc"), &home("fpr")).unwrap();
        assert_eq!(fprs, ["A1B2C3D4E5F60718293A4B5C6D7E8F9001122334"]);
    }

    #[test]
    fn verifies_the_example_inrelease() {
        let signed = fs::read(fixture("example-InRelease")).unwrap();
        let text = verify_clearsigned(&signed, &fixture("example-key.asc"), &home("clear")).unwrap();
        let text = String::from_utf8(text).unwrap();
        assert!(text.contains("main/binary-amd64/Packages.gz"), "{text}");
        assert!(!text.contains("BEGIN PGP"), "{text}");

        // One changed byte in the signed part must fail.
        let mut tampered = signed.clone();
        let at = String::from_utf8_lossy(&signed).find("binary-amd64").unwrap();
        tampered[at] = b'B';
        assert!(verify_clearsigned(&tampered, &fixture("example-key.asc"), &home("tamper")).is_err());
    }
}
