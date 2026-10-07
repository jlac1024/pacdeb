//! Where new versions of an app come from: direct URLs and feeds, apt repositories,
//! GitHub releases, or nowhere (manual).

mod apt;
mod direct;
mod github;
pub mod gpg;
mod jsonpath;

use std::path::Path;

use crate::error::{Error, Result, bail};
use crate::net::{self, Checksum, Head};
use crate::registry::SourceConfig;

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

/// Asks the source what its newest version is. Only feeds and indexes are fetched,
/// never the deb itself. `config_dir` resolves apt key paths; `cache_dir` holds gpg's
/// scratch home.
pub fn latest(app: &str, source: &SourceConfig, channel: Option<&str>, config_dir: &Path, cache_dir: &Path) -> Result<Latest> {
    let fill = |s: &str| -> Result<String> {
        if !s.contains("{channel}") {
            return Ok(s.to_string());
        }
        match channel {
            Some(c) => Ok(s.replace("{channel}", c)),
            None => bail!("{app} needs a channel; set one with 'ferry set {app} --channel <name>'"),
        }
    };
    let fill_opt = |s: &Option<String>| -> Result<Option<String>> { s.as_deref().map(fill).transpose() };

    match source {
        SourceConfig::Direct { url, feed, version_json, version_pattern, url_json, checksum_json, .. } => {
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
            let (vj, vp, uj, cj) = (fill_opt(version_json)?, fill_opt(version_pattern)?, fill_opt(url_json)?, fill_opt(checksum_json)?);
            let spec = direct::FeedSpec {
                url: url.as_deref(),
                version_json: vj.as_deref(),
                version_pattern: vp.as_deref(),
                url_json: uj.as_deref(),
                checksum_json: cj.as_deref(),
            };
            direct::from_feed(&text, &spec).map_err(|e| Error::new(format!("{app}: {e}")))
        }
        SourceConfig::Apt { repo, suite, component, package, arch, key } => {
            let Some(key) = key else {
                bail!("{app}'s apt source has no signing key; add one with 'ferry set {app} --key <file>'");
            };
            let key = config_dir.join(key);
            let home = cache_dir.join("gnupg").join(app);
            let repo = repo.trim_end_matches('/');
            let dists = format!("{repo}/dists/{suite}");
            let release = match net::get_bytes(&format!("{dists}/InRelease"), &[]) {
                Ok(signed) => gpg::verify_clearsigned(&signed, &key, &home)?,
                // Older repositories sign Release separately.
                Err(_) => {
                    let data = net::get_bytes(&format!("{dists}/Release"), &[])?;
                    let sig = net::get_bytes(&format!("{dists}/Release.gpg"), &[])?;
                    gpg::verify_detached(&data, &sig, &key, &home)?;
                    data
                }
            };
            let release = String::from_utf8_lossy(&release);
            let arch = arch.as_deref().unwrap_or(host_arch());
            let file = apt::index_file(&release, component, arch)?;
            let raw = net::get_bytes(&format!("{dists}/{}", file.path), &[])?;
            let index = apt::read_index(&file, &raw)?;
            let package = package.as_deref().unwrap_or(app);
            let Some(c) = apt::pick(&index, package, arch)? else {
                bail!("the repository has no {package} for {arch}");
            };
            Ok(Latest {
                version: Some(c.version),
                url: Some(format!("{repo}/{}", c.filename)),
                checksum: c.sha256.as_deref().and_then(Checksum::from_hex),
                head: None,
            })
        }
        SourceConfig::Github { repo, asset, prerelease } => {
            let token = std::env::var("FERRY_GITHUB_TOKEN").ok().filter(|t| !t.is_empty());
            let auth = token.map(|t| format!("Bearer {t}"));
            let mut headers = vec![("Accept", "application/vnd.github+json")];
            if let Some(a) = &auth {
                headers.push(("Authorization", a));
            }
            let json = net::get_text(&format!("https://api.github.com/repos/{repo}/releases?per_page=20"), &headers)?;
            github::pick(&json, asset, *prerelease).map_err(|e| Error::new(format!("{app}: {e}")))
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
            url_json: None,
            checksum_json: None,
            default_channel: None,
        };
        let tmp = Path::new(env!("CARGO_MANIFEST_DIR")).join("build/sandbox");
        let err = latest("demo", &src, None, &tmp, &tmp).unwrap_err().to_string();
        assert!(err.contains("ferry set demo --channel"), "{err}");
        assert_eq!(latest("demo", &SourceConfig::Manual {}, None, &tmp, &tmp).unwrap(), Latest::default());
        let apt = SourceConfig::Apt { repo: "r".into(), suite: "s".into(), component: "c".into(), package: None, arch: None, key: None };
        assert!(latest("demo", &apt, None, &tmp, &tmp).unwrap_err().to_string().contains("no signing key"));
    }
}
