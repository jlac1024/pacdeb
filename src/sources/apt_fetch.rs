// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reading a saved apt repository over the network: its signed Release file, then the
//! package index of each component, each checked against Release.

use std::fs;
use std::path::{Path, PathBuf};

use super::release::{self, ReleaseInfo};
use super::{apt, gpg};
use crate::error::{Context, Result, bail};
use crate::net;
use crate::registry::AptRepoConfig;

pub struct Fetched {
    pub release: String,
    pub info: ReleaseInfo,
    /// Each component's package index (one entry with an empty name for a flat repository).
    pub indexes: Vec<(String, String)>,
}

/// Where the repository's Release and indexes live: dists/<suite> for a normal
/// repository, the folder itself for a flat one.
fn base(repo: &AptRepoConfig) -> String {
    let url = repo.url.trim_end_matches('/');
    if repo.is_flat() {
        let dir = repo.suite.trim_start_matches("./").trim_end_matches('/');
        if dir.is_empty() { url.to_string() } else { format!("{url}/{dir}") }
    } else {
        format!("{url}/dists/{}", repo.suite)
    }
}

pub fn arch(repo: &AptRepoConfig) -> String {
    repo.arch.clone().unwrap_or_else(|| apt::host_arch().to_string())
}

/// The verified Release file of saved repository `name`, also kept in the cache so
/// its details and the time of the last check can be shown without the network.
pub fn fetch_release(name: &str, repo: &AptRepoConfig, key: &Path, cache_dir: &Path) -> Result<(String, ReleaseInfo)> {
    let base = base(repo);
    let home = cache_dir.join("gnupg").join(format!("apt-{name}"));
    let raw = match net::get_bytes(&format!("{base}/InRelease"), &[]) {
        Ok(signed) => gpg::verify_clearsigned(&signed, key, &home)?,
        // Older repositories sign Release separately.
        Err(e) if net::not_found(&e) => {
            let data = net::get_bytes(&format!("{base}/Release"), &[])?;
            let sig = net::get_bytes(&format!("{base}/Release.gpg"), &[])?;
            gpg::verify_detached(&data, &sig, key, &home)?;
            data
        }
        Err(e) => return Err(e),
    };
    let text = String::from_utf8_lossy(&raw).into_owned();
    let info = release::info(&text);
    release::check_fresh(&info, release::now())?;
    let cached = cached_release_path(name, cache_dir);
    if let Some(dir) = cached.parent() {
        fs::create_dir_all(dir).context(dir.display())?;
    }
    fs::write(&cached, &text).context(cached.display())?;
    Ok((text, info))
}

/// Release plus every component's package index.
pub fn fetch(name: &str, repo: &AptRepoConfig, key: &Path, cache_dir: &Path) -> Result<Fetched> {
    let (release, info) = fetch_release(name, repo, key, cache_dir)?;
    let base = base(repo);
    let arch = arch(repo);
    let components: Vec<String> = if repo.is_flat() { vec![String::new()] } else { repo.components.clone() };
    if components.is_empty() {
        bail!("apt repository {name} lists no components; add some with 'pacdeb apt edit {name} --components main'");
    }
    let mut indexes = Vec::new();
    for component in components {
        let file = apt::index_file(&release, &component, &arch).context(format!("component {component}"))?;
        let raw = net::get_bytes(&format!("{base}/{}", file.path), &[])?;
        indexes.push((component, apt::read_index(&file, &raw)?));
    }
    save_indexes(name, &indexes, cache_dir)?;
    Ok(Fetched { release, info, indexes })
}

fn indexes_dir(name: &str, cache_dir: &Path) -> PathBuf {
    cache_dir.join("apt").join(name)
}

/// Keeps the verified indexes, replacing the previous ones, for searching and
/// installing without the network.
fn save_indexes(name: &str, indexes: &[(String, String)], cache_dir: &Path) -> Result<()> {
    let dir = indexes_dir(name, cache_dir);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).context(dir.display())?;
    for (component, text) in indexes {
        let file = dir.join(format!("{}.Packages", if component.is_empty() { "flat" } else { component }));
        fs::write(&file, text).context(file.display())?;
    }
    Ok(())
}

/// The indexes from the last successful fetch; empty when there was none.
pub fn cached_indexes(name: &str, cache_dir: &Path) -> Vec<(String, String)> {
    let Ok(entries) = fs::read_dir(indexes_dir(name, cache_dir)) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = entries
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            let component = file.strip_suffix(".Packages")?.to_string();
            Some((if component == "flat" { String::new() } else { component }, fs::read_to_string(e.path()).ok()?))
        })
        .collect();
    out.sort();
    out
}

fn cached_release_path(name: &str, cache_dir: &Path) -> PathBuf {
    cache_dir.join("apt").join(format!("{name}.Release"))
}

/// The Release file from the last successful check, and when that was.
pub fn last_checked(name: &str, cache_dir: &Path) -> Option<(i64, ReleaseInfo)> {
    let path = cached_release_path(name, cache_dir);
    let when = fs::metadata(&path).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((when, release::info(&fs::read_to_string(&path).ok()?)))
}

/// Forgets the cached Release of a repository that was removed or renamed.
pub fn forget(name: &str, cache_dir: &Path) {
    let _ = fs::remove_file(cached_release_path(name, cache_dir));
    let _ = fs::remove_dir_all(indexes_dir(name, cache_dir));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(url: &str, suite: &str) -> AptRepoConfig {
        AptRepoConfig { url: url.into(), suite: suite.into(), components: vec!["main".into()], arch: None, key: None, key_url: None, key_fingerprint: None }
    }

    #[test]
    fn finds_the_release_folder() {
        let cases = [
            ("https://x.example/apt", "stable", "https://x.example/apt/dists/stable"),
            ("https://x.example/apt/", "noble", "https://x.example/apt/dists/noble"),
            ("https://x.example/flat", "./", "https://x.example/flat"),
            ("https://x.example/flat/", "/", "https://x.example/flat"),
            ("https://x.example/flat", "amd64/", "https://x.example/flat/amd64"),
        ];
        for (url, suite, want) in cases {
            assert_eq!(base(&repo(url, suite)), want, "{url} {suite}");
        }
    }
}
