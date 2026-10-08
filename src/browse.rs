//! Looking at sources rather than apps: which apt repositories the tracked apps come
//! from, and every package such a repository offers. Used by `pacdeb packages` and by
//! pacdeb-gui's Sources page.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Context, Result, bail};
use crate::net;
use crate::paths::Paths;
use crate::registry::{Config, SourceConfig};
use crate::sources::apt::{self, Listed};
use crate::sources::{apt_index, gpg};

/// An apt repository: where it is and which part of it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AptRepo {
    pub repo: String,
    pub suite: String,
    pub component: String,
    pub arch: String,
}

/// An apt repository the tracked apps use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsedRepo {
    pub repo: AptRepo,
    /// The signing key file, from the first app that has one.
    pub key: Option<PathBuf>,
    pub apps: Vec<String>,
    /// Package name each app follows in this repository.
    pub packages: Vec<String>,
}

/// The apt repositories tracked apps come from, one entry per repository.
pub fn used_repos(config: &Config, paths: &Paths) -> Vec<UsedRepo> {
    let mut out: Vec<UsedRepo> = Vec::new();
    for (name, app) in &config.apps {
        let SourceConfig::Apt { repo, suite, component, package, arch, key } = &app.source else {
            continue;
        };
        let r = AptRepo {
            repo: repo.trim_end_matches('/').to_string(),
            suite: suite.clone(),
            component: component.clone(),
            arch: arch.clone().unwrap_or_else(|| apt::host_arch().to_string()),
        };
        let key = key.as_ref().map(|k| paths.config.join(k));
        let package = package.clone().unwrap_or_else(|| name.clone());
        match out.iter_mut().find(|u| u.repo == r) {
            Some(u) => {
                u.apps.push(name.clone());
                u.packages.push(package);
                if u.key.is_none() {
                    u.key = key;
                }
            }
            None => out.push(UsedRepo { repo: r, key, apps: vec![name.clone()], packages: vec![package] }),
        }
    }
    out.sort_by(|a, b| a.repo.cmp(&b.repo));
    out
}

/// Every package `repo` offers, after checking its signature with `key`.
pub fn packages(repo: &AptRepo, key: &Path, paths: &Paths) -> Result<Vec<Listed>> {
    let home = paths.cache.join("gnupg").join("browse");
    let index = apt_index(&repo.repo, &repo.suite, &repo.component, &repo.arch, key, &home)?;
    apt::list(&index, &repo.arch)
}

/// Downloads a repository's signing key for browsing before anything is tracked, and
/// checks it against `fingerprint` when one is given. Returns the file and the key's
/// fingerprints.
pub fn fetch_key(url: &str, fingerprint: Option<&str>, paths: &Paths) -> Result<(PathBuf, Vec<String>)> {
    let dir = paths.cache.join("keys").join("browse");
    fs::create_dir_all(&dir).context(dir.display())?;
    let file = dir.join(format!("{:016x}.asc", simple_hash(url)));
    fs::write(&file, net::get_bytes(url, &[])?).context(file.display())?;
    let fprs = gpg::fingerprints(&file, &paths.cache.join("gnupg").join("browse"))?;
    if fprs.is_empty() {
        bail!("{url} holds no public key");
    }
    if let Some(want) = fingerprint.map(|f| f.replace(' ', "").to_ascii_uppercase()).filter(|f| !f.is_empty()) {
        if !fprs.iter().any(|f| f.eq_ignore_ascii_case(&want)) {
            bail!("the key from {url} has fingerprint {}, not {want}; not using it", fprs.join(", "));
        }
    }
    Ok((file, fprs))
}

/// A stable file name for a URL (FNV-1a), so browsing the same repository reuses one file.
fn simple_hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

/// `pacdeb packages <app>`: every package in a tracked app's apt repository.
pub fn run(app: &str) -> Result<()> {
    let paths = Paths::from_env()?;
    let config = Config::load(&paths.config)?;
    let Some(a) = config.apps.get(app) else {
        bail!("{app} is not tracked; 'pacdeb list' shows the tracked apps");
    };
    if !matches!(a.source, SourceConfig::Apt { .. }) {
        bail!("{app} does not come from an apt repository");
    }
    let Some(used) = used_repos(&config, &paths).into_iter().find(|u| u.apps.iter().any(|n| n == app)) else {
        bail!("{app}'s repository was not found");
    };
    let Some(key) = &used.key else {
        bail!("{app}'s apt source has no signing key; add one with 'pacdeb set {app} --key-url <url>'");
    };
    let r = &used.repo;
    let list = packages(r, key, &paths)?;
    println!("{} {} {} ({}): {}", r.repo, r.suite, r.component, r.arch, crate::human::plural(list.len(), "package", "packages"));
    let width = list.iter().map(|l| l.name.len()).max().unwrap_or(0);
    let vwidth = list.iter().map(|l| l.version.len()).max().unwrap_or(0);
    for l in &list {
        let mark = if used.packages.contains(&l.name) { "*" } else { " " };
        println!("{mark} {:<width$}  {:<vwidth$}  {}", l.name, l.version, l.summary);
    }
    println!("* tracked. Track another with: pacdeb add <name> --source apt --repo {} --suite {} --component {} --key {}", r.repo, r.suite, r.component, key.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::App;

    fn apt_app(repo: &str, package: Option<&str>) -> App {
        App::new(SourceConfig::Apt {
            repo: repo.into(),
            suite: "stable".into(),
            component: "main".into(),
            package: package.map(String::from),
            arch: None,
            key: Some(format!("keys/{repo}.asc").replace("https://", "")),
        })
    }

    #[test]
    fn groups_apps_by_repository() {
        let mut config = Config::default();
        config.apps.insert("one".into(), apt_app("https://a.example/apt", None));
        config.apps.insert("two".into(), apt_app("https://a.example/apt/", Some("two-bin")));
        config.apps.insert("three".into(), apt_app("https://b.example/apt", None));
        config.apps.insert("web".into(), App::new(SourceConfig::Manual {}));
        let paths = Paths { config: "/c".into(), state: "/s".into(), cache: "/k".into() };
        let used = used_repos(&config, &paths);
        assert_eq!(used.len(), 2);
        assert_eq!(used[0].repo.repo, "https://a.example/apt");
        assert_eq!(used[0].apps, ["one", "two"]);
        assert_eq!(used[0].packages, ["one", "two-bin"]);
        assert_eq!(used[0].key.as_deref(), Some(Path::new("/c/keys/a.example/apt.asc")));
        assert_eq!(used[1].apps, ["three"]);
    }
}
