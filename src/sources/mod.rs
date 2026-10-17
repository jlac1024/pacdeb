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
use crate::version::DebVersion;

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
    match source {
        SourceConfig::Direct { .. } => {
            // "Beta,Stable" follows several channels: the newest version among them wins,
            // and channels the feed does not have are skipped.
            let list: Vec<&str> = channel.map(|c| c.split(',').map(str::trim).filter(|c| !c.is_empty()).collect()).unwrap_or_default();
            direct_channels(app, source, &list, &mut Vec::new())
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

/// A direct source following the channels in `list`: the newest version wins, and
/// channels that fail (usually ones the feed lacks) are skipped while one works.
fn direct_channels(app: &str, source: &SourceConfig, list: &[&str], feeds: &mut Vec<(String, String)>) -> Result<Latest> {
    if list.len() < 2 || !source.uses_channel() {
        return direct_latest(app, source, list.first().copied(), feeds);
    }
    let mut best: Option<Latest> = None;
    let mut errors = Vec::new();
    for c in list {
        match direct_latest(app, source, Some(c), feeds) {
            Ok(l) => {
                let newer = best.as_ref().is_none_or(|b| match (b.version.as_deref().map(DebVersion::parse), l.version.as_deref().map(DebVersion::parse)) {
                    (Some(Ok(old)), Some(Ok(new))) => new > old,
                    _ => false,
                });
                if newer {
                    best = Some(l);
                }
            }
            Err(e) => errors.push(format!("{c}: {e}")),
        }
    }
    match best {
        Some(l) => Ok(l),
        None => bail!("{app}: none of the channels {} worked\n  {}", list.join(", "), errors.join("\n  ")),
    }
}

/// A direct source for one channel. `feeds` keeps fetched feeds, so following several
/// channels of the same feed downloads it once.
fn direct_latest(app: &str, source: &SourceConfig, channel: Option<&str>, feeds: &mut Vec<(String, String)>) -> Result<Latest> {
    let SourceConfig::Direct { url, feed, version_json, version_pattern, version_regex, url_json, checksum_json, .. } = source else {
        bail!("{app} does not have a direct source");
    };
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
    let text = match feeds.iter().find(|(u, _)| *u == feed) {
        Some((_, t)) => t.clone(),
        None => {
            let t = net::get_text(&feed, &[])?;
            feeds.push((feed.clone(), t.clone()));
            t
        }
    };
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
    direct::from_feed(&text, &spec).map_err(|e| {
        // A channel the feed does not have is the usual mistake; say which ones it has.
        let offered = version_json.as_deref().filter(|_| channel.is_some()).and_then(|vj| direct::channels(&text, vj)).filter(|c| !c.is_empty());
        match offered {
            Some(c) => crate::error::Error::new(format!("{e}; the feed's channels are {}", c.join(", "))),
            None => e,
        }
    })
}

/// The channels a JSON feed offers, for a `version_json` path with `{channel}` in it.
pub fn feed_channels(text: &str, version_json: &str) -> Option<Vec<String>> {
    direct::channels(text, version_json)
}

pub use direct::{GuessedPaths, guess_paths};

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
    fn follows_several_channels() {
        let feed_url = "https://example.invalid/feed.json";
        let src = SourceConfig::Direct {
            url: None,
            feed: Some(feed_url.into()),
            version_json: Some("Releases[CategoryName={channel}].Version".into()),
            version_pattern: None,
            version_regex: None,
            url_json: Some("Releases[CategoryName={channel}].File[Identifier=.deb (Ubuntu/Debian)].Url".into()),
            checksum_json: None,
            default_channel: None,
        };
        let cases: &[(&[&str], std::result::Result<&str, &str>)] = &[
            (&["Stable"], Ok("1.14.0")),
            (&["Stable", "EarlyAccess"], Ok("1.15.0")),
            (&["EarlyAccess", "Stable"], Ok("1.15.0")),
            (&["Beta", "Stable"], Ok("1.14.0")),
            (&["Alpha", "Beta", "Stable"], Ok("1.15.1")),
            (&["Beta"], Err("the feed's channels are Alpha, EarlyAccess, Stable")),
            (&["Beta", "Nightly"], Err("none of the channels Beta, Nightly worked")),
        ];
        for (list, want) in cases {
            let mut feeds = vec![(feed_url.to_string(), crate::testdata::feed())];
            let got = direct_channels("demo", &src, list, &mut feeds);
            match (got, want) {
                (Ok(l), Ok(v)) => assert_eq!(l.version.as_deref(), Some(*v), "{list:?}"),
                (Err(e), Err(msg)) => assert!(e.to_string().contains(msg), "{list:?}: {e}"),
                (got, _) => panic!("{list:?}: unexpected {got:?}"),
            }
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
