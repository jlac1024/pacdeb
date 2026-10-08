// SPDX-License-Identifier: AGPL-3.0-or-later
//! Direct sources: a .deb at a URL, with an optional feed that names the version.

use serde_json::Value;

use super::{Latest, jsonpath};
use crate::error::{Context, Result, bail};
use crate::net::Checksum;
use crate::version::DebVersion;

/// What a direct source with a feed is configured with, with {channel} filled in.
pub struct FeedSpec<'a> {
    pub url: Option<&'a str>,
    pub version_json: Option<&'a str>,
    pub version_pattern: Option<&'a str>,
    pub version_regex: Option<&'a str>,
    pub url_json: Option<&'a str>,
    pub checksum_json: Option<&'a str>,
}

/// Reads the version, download URL and checksum out of a feed's text.
pub fn from_feed(text: &str, spec: &FeedSpec) -> Result<Latest> {
    let json: Option<Value> = if spec.version_json.is_some() || spec.url_json.is_some() || spec.checksum_json.is_some() {
        Some(serde_json::from_str(text).context("the feed is not JSON")?)
    } else {
        None
    };
    let version = match (spec.version_json, spec.version_regex, spec.version_pattern, &json) {
        (Some(path), _, _, Some(j)) => jsonpath::get_string(j, path)?,
        (None, Some(re), _, _) => match highest_regex_match(text, re)? {
            Some(v) => v,
            None => bail!("nothing in the feed matches the regex '{re}'"),
        },
        (None, None, Some(pattern), _) => match highest_match(text, pattern) {
            Some(v) => v,
            None => bail!("nothing in the feed matches the pattern '{pattern}'"),
        },
        _ => bail!("the source has a feed but no version_json, version_regex or version_pattern to read it with"),
    };
    let url = match (spec.url_json, spec.url, &json) {
        (Some(path), _, Some(j)) => jsonpath::get_string(j, path)?,
        (None, Some(template), _) => template.replace("{version}", &version),
        _ => bail!("the source has no url or url_json to download from"),
    };
    let checksum = match (spec.checksum_json, &json) {
        (Some(path), Some(j)) => {
            let hex = jsonpath::get_string(j, path)?;
            match Checksum::from_hex(&hex) {
                Some(c) => Some(c),
                None => bail!("the feed's checksum '{hex}' is not a sha256 or sha512 hex digest"),
            }
        }
        _ => None,
    };
    Ok(Latest { version: Some(version), url: Some(url), checksum, ..Latest::default() })
}

/// Every match of `re` in `text` gives a version (its first capture group, or the whole
/// match); the highest by Debian ordering wins.
pub fn highest_regex_match(text: &str, re: &str) -> Result<Option<String>> {
    let re = regex::Regex::new(re).context(format!("bad version_regex '{re}'"))?;
    let mut best: Option<(String, Option<DebVersion>)> = None;
    for caps in re.captures_iter(text) {
        let Some(m) = caps.get(1).or_else(|| caps.get(0)) else {
            continue;
        };
        let v = m.as_str().to_string();
        let parsed = DebVersion::parse(&v).ok();
        let better = match (&best, &parsed) {
            (None, _) => true,
            (Some((_, Some(b))), Some(p)) => p > b,
            (Some((_, None)), Some(_)) => true,
            _ => false,
        };
        if better {
            best = Some((v, parsed));
        }
    }
    Ok(best.map(|(v, _)| v))
}

/// Finds every `prefix{version}suffix` in `text` and returns the highest version.
/// Versions are runs of characters Debian allows in a version.
pub fn highest_match(text: &str, pattern: &str) -> Option<String> {
    let (prefix, suffix) = pattern.split_once("{version}")?;
    let allowed = |c: char| c.is_ascii_alphanumeric() || ".+~-:_".contains(c);
    let mut best: Option<(String, Option<DebVersion>)> = None;
    let mut rest = text;
    while let Some(i) = rest.find(prefix) {
        let after = &rest[i + prefix.len()..];
        // Take the longest version run that still leaves the suffix right after it.
        let run_len = after.find(|c: char| !allowed(c)).unwrap_or(after.len());
        let found = (1..=run_len)
            .rev()
            .map(|n| &after[..n])
            .find(|v| after[v.len()..].starts_with(suffix) && v.starts_with(|c: char| c.is_ascii_digit()));
        if let Some(v) = found {
            let parsed = DebVersion::parse(v).ok();
            let better = match (&best, &parsed) {
                (None, _) => true,
                (Some((_, Some(b))), Some(p)) => p > b,
                _ => false,
            };
            if better {
                best = Some((v.to_string(), parsed));
            }
        }
        rest = &rest[i + prefix.len().max(1)..];
    }
    best.map(|(v, _)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proton() -> String {
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sources/proton-version.json")).unwrap()
    }

    fn spec(channel: &str) -> (String, String, String) {
        (
            format!("Releases[CategoryName={channel}].Version"),
            format!("Releases[CategoryName={channel}].File[Identifier=.deb (Ubuntu/Debian)].Url"),
            format!("Releases[CategoryName={channel}].File[Identifier=.deb (Ubuntu/Debian)].Sha512CheckSum"),
        )
    }

    #[test]
    fn reads_proton_channels() {
        let cases = [("Stable", "1.14.0"), ("EarlyAccess", "1.15.0"), ("Alpha", "1.15.1")];
        for (channel, want) in cases {
            let (v, u, c) = spec(channel);
            let s = FeedSpec { url: None, version_json: Some(&v), version_pattern: None, version_regex: None, url_json: Some(&u), checksum_json: Some(&c) };
            let l = from_feed(&proton(), &s).unwrap();
            assert_eq!(l.version.as_deref(), Some(want), "{channel}");
            assert_eq!(l.url.unwrap(), format!("https://proton.me/download/mail/linux/{want}/ProtonMail-desktop-beta.deb"));
            assert!(matches!(l.checksum, Some(Checksum::Sha512(_))), "{channel}");
        }
    }

    #[test]
    fn reads_versions_from_a_page() {
        let page = r#"<a href="/dl/app_1.9.0_amd64.deb">old</a> <a href="/dl/app_1.10.2_amd64.deb">new</a>
            <a href="/dl/app_1.10.2_arm64.deb">arm</a> app_beta_amd64.deb"#;
        assert_eq!(highest_match(page, "app_{version}_amd64.deb").as_deref(), Some("1.10.2"));
        assert_eq!(highest_match(page, "nothing_{version}.deb"), None);
        let s = FeedSpec { url: Some("https://x/dl/app_{version}_amd64.deb"), version_json: None, version_pattern: Some("app_{version}_amd64.deb"), version_regex: None, url_json: None, checksum_json: None };
        let l = from_feed(page, &s).unwrap();
        assert_eq!(l.url.as_deref(), Some("https://x/dl/app_1.10.2_amd64.deb"));
        assert_eq!(l.checksum, None);
    }

    #[test]
    fn reads_versions_with_a_regex() {
        let page = "Download v2.4.1 (stable) or v2.5.0-beta2. Older: v2.3.9";
        assert_eq!(highest_regex_match(page, r"v(\d+\.\d+\.\d+)\b").unwrap().as_deref(), Some("2.5.0"));
        assert_eq!(highest_regex_match(page, r"v(\d+\.\d+\.\d+) \(stable\)").unwrap().as_deref(), Some("2.4.1"));
        assert_eq!(highest_regex_match(page, r"\d+\.\d+\.\d+-beta\d").unwrap().as_deref(), Some("2.5.0-beta2"));
        assert_eq!(highest_regex_match(page, r"nomatch(\d)").unwrap(), None);
        assert!(highest_regex_match(page, r"(unclosed").unwrap_err().to_string().contains("bad version_regex"));

        let s = FeedSpec { url: Some("https://x/app_{version}.deb"), version_json: None, version_pattern: None, version_regex: Some(r"app_([0-9.]+)\.deb"), url_json: None, checksum_json: None };
        let l = from_feed("app_1.2.deb app_1.10.deb", &s).unwrap();
        assert_eq!(l.url.as_deref(), Some("https://x/app_1.10.deb"));
    }

    #[test]
    fn explains_bad_specs() {
        let s = FeedSpec { url: None, version_json: None, version_pattern: None, version_regex: None, url_json: None, checksum_json: None };
        assert!(from_feed("x", &s).unwrap_err().to_string().contains("no version_json, version_regex or version_pattern"));
        let s = FeedSpec { url: None, version_json: Some("Version"), version_pattern: None, version_regex: None, url_json: None, checksum_json: None };
        assert!(from_feed("not json", &s).unwrap_err().to_string().contains("not JSON"));
        assert!(from_feed("{\"Version\": \"1.0\"}", &s).unwrap_err().to_string().contains("no url or url_json"));
    }
}
