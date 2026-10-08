// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where new versions of an app come from: direct URLs and feeds, apt repositories,
//! GitHub releases, or nowhere (manual).

pub mod apt;
pub mod apt_fetch;
mod direct;
mod github;
pub mod gpg;
mod jsonpath;
pub mod release;

use std::path::Path;

use crate::error::{Result, bail};
use crate::net::{self, Checksum, Head};
use crate::registry::{Config, SourceConfig};

pub use apt::host_arch;

/// The newest version a source offers.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Latest {
    /// Debian version, when the source says it without a download.
    pub version: Option<String>,
    pub url: Option<String>,
    pub checksum: Option<Checksum>,
    /// For direct sources without a feed: the server's ETag and Last-Modified.
    pub head: Option<Head>,
}

impl Latest {
    pub fn to_available(&self, checked: i64) -> crate::registry::Available {
        crate::registry::Available {
            version: self.version.clone(),
            url: self.url.clone(),
            checksum: self.checksum.as_ref().map(|c| c.hex().to_string()),
            etag: self.head.as_ref().and_then(|h| h.etag.clone()),
            last_modified: self.head.as_ref().and_then(|h| h.last_modified.clone()),
            checked,
        }
    }

    pub fn from_available(a: &crate::registry::Available) -> Latest {
        let head = (a.etag.is_some() || a.last_modified.is_some()).then(|| Head { etag: a.etag.clone(), last_modified: a.last_modified.clone() });
        Latest { version: a.version.clone(), url: a.url.clone(), checksum: a.checksum.as_deref().and_then(Checksum::from_hex), head }
    }
}

/// The newest `package` in already fetched apt indexes, as a download.
pub fn apt_latest_in(indexes: &[(String, String)], repository: &str, repo: &crate::registry::AptRepoConfig, package: &str) -> Result<Latest> {
    let arch = apt_fetch::arch(repo);
    let mut best: Option<apt::Candidate> = None;
    for (_, index) in indexes {
        if let Some(c) = apt::pick(index, package, &arch)? {
            let newer = best.as_ref().is_none_or(|b| {
                matches!((crate::version::DebVersion::parse(&c.version), crate::version::DebVersion::parse(&b.version)), (Ok(n), Ok(o)) if n > o)
            });
            if newer {
                best = Some(c);
            }
        }
    }
    let Some(c) = best else {
        bail!("apt repository {repository} has no {package} for {arch}");
    };
    Ok(Latest {
        version: Some(c.version),
        url: Some(format!("{}/{}", repo.url.trim_end_matches('/'), c.filename.trim_start_matches("./"))),
        checksum: c.sha256.as_deref().and_then(Checksum::from_hex),
        head: None,
    })
}

/// Asks the source what its newest version is. Only feeds and indexes are fetched,
/// never the deb itself. `config_dir` resolves apt key paths; `cache_dir` holds gpg's
/// scratch home.
pub fn latest(app: &str, source: &SourceConfig, channel: Option<&str>, config: &Config, config_dir: &Path, cache_dir: &Path) -> Result<Latest> {
    let fill = |s: &str| -> Result<String> {
        if !s.contains("{channel}") {
            return Ok(s.to_string());
        }
        match channel {
            Some(c) => Ok(s.replace("{channel}", c)),
            None => bail!("{app} needs a channel; set one with 'pacdeb set {app} --channel <name>'"),
        }
    };
    let fill_opt = |s: &Option<String>| -> Result<Option<String>> { s.as_deref().map(fill).transpose() };

    match source {
        SourceConfig::Direct { url, feed, version_json, version_pattern, version_regex, url_json, checksum_json, .. } => {
            let url = fill_opt(url)?;
            let Some(feed) = fill_opt(feed)? else {
                let Some(url) = url else {
                    bail!("{app} has a direct source with neither url nor feed");
                };
                if url.contains("{version}") {
                    bail!("{app}'s url has {{version}} but there is no feed to say which version");
                }
                let head = net::head(&url)?;
                return Ok(Latest { url: Some(url), head: Some(head), ..Latest::default() });
            };
            let text = net::get_text(&feed, &[])?;
            let (vj, vp, vr) = (fill_opt(version_json)?, fill_opt(version_pattern)?, fill_opt(version_regex)?);
            let (uj, cj) = (fill_opt(url_json)?, fill_opt(checksum_json)?);
            let spec = direct::FeedSpec {
                url: url.as_deref(),
                version_json: vj.as_deref(),
                version_pattern: vp.as_deref(),
                version_regex: vr.as_deref(),
                url_json: uj.as_deref(),
                checksum_json: cj.as_deref(),
            };
            direct::from_feed(&text, &spec)
        }
        SourceConfig::Apt { repository, package } => {
            let Some(repo) = config.apt.get(repository) else {
                bail!("the apt repository {repository} is not saved; 'pacdeb apt list' shows the saved ones");
            };
            let Some(key) = &repo.key else {
                bail!("apt repository {repository} has no signing key; add one with 'pacdeb apt key {repository} --key-url <url>'");
            };
            let fetched = apt_fetch::fetch(repository, repo, &config_dir.join(key), cache_dir)?;
            apt_latest_in(&fetched.indexes, repository, repo, package.as_deref().unwrap_or(app))
        }
        SourceConfig::Github { repo, asset, prerelease } => {
            let token = std::env::var("PACDEB_GITHUB_TOKEN").ok().filter(|t| !t.is_empty());
            let auth = token.map(|t| format!("Bearer {t}"));
            let mut headers = vec![("Accept", "application/vnd.github+json")];
            if let Some(a) = &auth {
                headers.push(("Authorization", a));
            }
            let json = match net::get_text(&format!("https://api.github.com/repos/{repo}/releases?per_page=20"), &headers) {
                Err(e) if net::not_found(&e) => bail!(
                    "GitHub has no repository {repo}; check --repo (owner/name), or set PACDEB_GITHUB_TOKEN if it is private"
                ),
                other => other?,
            };
            github::pick(&json, asset, *prerelease)
        }
        SourceConfig::Manual {} => Ok(Latest::default()),
    }
}

/// Shell style matching with `*` and `?`.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    fn go(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some('*') => (0..=n.len()).any(|i| go(&p[1..], &n[i..])),
            Some('?') => !n.is_empty() && go(&p[1..], &n[1..]),
            Some(c) => n.first() == Some(c) && go(&p[1..], &n[1..]),
        }
    }
    go(&p, &n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        let cases = [
            ("*.deb", "app_1.0_amd64.deb", true),
            ("app_*_amd64.deb", "app_1.0_amd64.deb", true),
            ("app_*_amd64.deb", "app_1.0_arm64.deb", false),
            ("app-?.deb", "app-1.deb", true),
            ("app-?.deb", "app-10.deb", false),
            ("*", "", true),
        ];
        for (p, n, want) in cases {
            assert_eq!(glob_match(p, n), want, "{p} vs {n}");
        }
    }

    #[test]
    fn needs_a_channel_when_the_source_uses_one() {
        let src = SourceConfig::Direct {
            url: None,
            feed: Some("https://example.invalid/{channel}.json".into()),
            version_json: None,
            version_pattern: None,
            version_regex: None,
            url_json: None,
            checksum_json: None,
            default_channel: None,
        };
        let tmp = Path::new(env!("CARGO_MANIFEST_DIR")).join("build/sandbox");
        let mut config = Config::default();
        let err = latest("demo", &src, None, &config, &tmp, &tmp).unwrap_err().to_string();
        assert!(err.contains("pacdeb set demo --channel"), "{err}");
        assert_eq!(latest("demo", &SourceConfig::Manual {}, None, &config, &tmp, &tmp).unwrap(), Latest::default());
        let apt = SourceConfig::Apt { repository: "r".into(), package: None };
        assert!(latest("demo", &apt, None, &config, &tmp, &tmp).unwrap_err().to_string().contains("not saved"));
        config.apt.insert(
            "r".into(),
            crate::registry::AptRepoConfig { url: "u".into(), suite: "s".into(), components: vec![], arch: None, key: None, key_url: None, key_fingerprint: None },
        );
        assert!(latest("demo", &apt, None, &config, &tmp, &tmp).unwrap_err().to_string().contains("no signing key"));
    }
}
