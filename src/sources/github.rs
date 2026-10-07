//! GitHub releases: the newest release with an asset matching a pattern.

use serde_json::Value;

use super::{Latest, glob_match};
use crate::error::{Context, Result, bail};
use crate::net::Checksum;

pub fn pick(json: &str, asset_pattern: &str, prerelease: bool) -> Result<Latest> {
    let releases: Value = serde_json::from_str(json).context("GitHub's answer is not JSON")?;
    let Some(list) = releases.as_array() else {
        let msg = releases.get("message").and_then(Value::as_str).unwrap_or("unexpected answer");
        bail!("GitHub says: {msg}");
    };
    for r in list {
        let flag = |k: &str| r.get(k).and_then(Value::as_bool).unwrap_or(false);
        if flag("draft") || (flag("prerelease") && !prerelease) {
            continue;
        }
        let assets = r.get("assets").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
        let Some(asset) = assets
            .iter()
            .find(|a| a.get("name").and_then(Value::as_str).is_some_and(|n| glob_match(asset_pattern, n)))
        else {
            continue;
        };
        let tag = r.get("tag_name").and_then(Value::as_str).unwrap_or_default();
        let version = tag.strip_prefix('v').unwrap_or(tag).to_string();
        let url = asset.get("browser_download_url").and_then(Value::as_str).map(String::from);
        let checksum = asset
            .get("digest")
            .and_then(Value::as_str)
            .and_then(|d| d.strip_prefix("sha256:"))
            .and_then(Checksum::from_hex);
        return Ok(Latest { version: Some(version), url, checksum, ..Latest::default() });
    }
    bail!("no release has an asset matching '{asset_pattern}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> String {
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sources/github-releases.json")).unwrap()
    }

    #[test]
    fn picks_the_newest_matching_asset() {
        let l = pick(&fixture(), "obsidian_*_amd64.deb", false).unwrap();
        assert_eq!(l.version.as_deref(), Some("1.14.4"));
        assert_eq!(l.url.as_deref(), Some("https://github.com/obsidianmd/obsidian-releases/releases/download/v1.14.4/obsidian_1.14.4_amd64.deb"));
        assert_eq!(l.checksum, Checksum::from_hex("85b10dcba6edfc1c0460a6d18260cf31c30447a444bd858a6440b9c9c8806d25"));
    }

    #[test]
    fn skips_drafts_and_prereleases() {
        let json = r#"[
            {"tag_name": "v3.0", "draft": true, "assets": [{"name": "app_3.0_amd64.deb", "browser_download_url": "u3"}]},
            {"tag_name": "v2.0-rc1", "prerelease": true, "assets": [{"name": "app_2.0-rc1_amd64.deb", "browser_download_url": "u2"}]},
            {"tag_name": "1.0", "assets": [{"name": "app_1.0_amd64.deb", "browser_download_url": "u1"}]}
        ]"#;
        assert_eq!(pick(json, "app_*_amd64.deb", false).unwrap().version.as_deref(), Some("1.0"));
        assert_eq!(pick(json, "app_*_amd64.deb", true).unwrap().version.as_deref(), Some("2.0-rc1"));
        assert!(pick(json, "*.rpm", true).unwrap_err().to_string().contains("no release has an asset"));
        assert!(pick(r#"{"message": "API rate limit exceeded"}"#, "*", false).unwrap_err().to_string().contains("rate limit"));
    }
}
