//! The apps Ferry tracks (apps.toml in the config dir) and what it has built for them
//! (state.toml in the state dir).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Context, Result};

const PRESETS: &str = include_str!("../data/presets.toml");

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub apps: BTreeMap<String, App>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Channel for apps that do not pick their own, such as "Stable".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    /// Overrides the global channel for this app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Package name to build instead of the deb's Package name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pkgname: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provides: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<String>,
    /// Added to depends.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_depends: Vec<String>,
    /// Removed from depends and optdepends.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drop_depends: Vec<String>,
    pub source: SourceConfig,
}

impl App {
    pub fn new(source: SourceConfig) -> App {
        App {
            channel: None,
            pkgname: None,
            provides: Vec::new(),
            conflicts: Vec::new(),
            extra_depends: Vec::new(),
            drop_depends: Vec::new(),
            source,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum SourceConfig {
    /// A .deb at a URL, optionally with a feed that says the current version. Any
    /// value may contain {channel}; `url` may also contain {version}.
    Direct {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feed: Option<String>,
        /// Path into a JSON feed, like `Releases[CategoryName={channel}].Version`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version_json: Option<String>,
        /// A pattern like `app_{version}_amd64.deb` matched against a text feed; the
        /// highest matching version wins.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version_pattern: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url_json: Option<String>,
        /// A sha256 (64 hex) or sha512 (128 hex) checksum for the deb.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checksum_json: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_channel: Option<String>,
    },
    Apt {
        repo: String,
        suite: String,
        component: String,
        /// The Debian package name; defaults to the app name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        package: Option<String>,
        /// Debian architecture; defaults to the machine's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arch: Option<String>,
        /// Signing key for the repo, relative to the config dir.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    Github {
        /// owner/name
        repo: String,
        /// Asset file name pattern, `*` and `?` wildcards.
        asset: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        prerelease: bool,
    },
    /// No remote: new versions come from `ferry update <app> --file <deb>`.
    Manual {},
}

impl SourceConfig {
    pub fn kind(&self) -> &'static str {
        match self {
            SourceConfig::Direct { .. } => "direct",
            SourceConfig::Apt { .. } => "apt",
            SourceConfig::Github { .. } => "github",
            SourceConfig::Manual {} => "manual",
        }
    }

    /// Whether any of the source's values use {channel}.
    pub fn uses_channel(&self) -> bool {
        match self {
            SourceConfig::Direct { url, feed, version_json, version_pattern, url_json, checksum_json, .. } => {
                [url, feed, version_json, version_pattern, url_json, checksum_json]
                    .iter()
                    .any(|v| v.as_deref().is_some_and(|s| s.contains("{channel}")))
            }
            _ => false,
        }
    }

    fn default_channel(&self) -> Option<&str> {
        match self {
            SourceConfig::Direct { default_channel, .. } => default_channel.as_deref(),
            _ => None,
        }
    }
}

impl Config {
    pub fn path(config_dir: &Path) -> PathBuf {
        config_dir.join("apps.toml")
    }

    pub fn load(config_dir: &Path) -> Result<Config> {
        let path = Config::path(config_dir);
        match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).context(path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).context(path.display()),
        }
    }

    pub fn save(&self, config_dir: &Path) -> Result<()> {
        let body = toml::to_string(self).context("writing apps.toml")?;
        let text = format!(
            "# Apps Ferry tracks. Change it with 'ferry add', 'ferry set' and 'ferry remove';\n\
             # hand edits work too, but comments are not kept.\n\n{body}"
        );
        write_atomic(&Config::path(config_dir), &text)
    }

    /// The channel an app follows: its own, then the global one, then the source's
    /// default. None when the source has no channels.
    pub fn channel(&self, app: &App) -> Option<String> {
        app.channel
            .clone()
            .or_else(|| self.settings.channel.clone())
            .or_else(|| app.source.default_channel().map(String::from))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    #[serde(default)]
    pub apps: BTreeMap<String, AppState>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppState {
    /// Debian version of the deb last built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deb_version: Option<String>,
    #[serde(default)]
    pub pkgrel: u32,
    /// Built packages, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<String>,
    /// For direct sources without a feed: what the server said last time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
}

impl State {
    pub fn load(state_dir: &Path) -> Result<State> {
        let path = state_dir.join("state.toml");
        match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).context(path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e).context(path.display()),
        }
    }

    pub fn save(&self, state_dir: &Path) -> Result<()> {
        let body = toml::to_string(self).context("writing state.toml")?;
        write_atomic(&state_dir.join("state.toml"), &format!("# Written by Ferry. Do not edit.\n\n{body}"))
    }
}

/// A preset: a ready made source for a known app.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub source: SourceConfig,
    /// For apt presets: where the signing key is published, and its fingerprint.
    #[serde(default)]
    pub key_url: Option<String>,
    #[serde(default)]
    pub key_fingerprint: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

pub fn presets() -> BTreeMap<String, Preset> {
    toml::from_str(PRESETS).expect("data/presets.toml is checked by tests")
}

fn write_atomic(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).context(dir.display())?;
    }
    let tmp = path.with_extension("toml.part");
    fs::write(&tmp, text).context(tmp.display())?;
    fs::rename(&tmp, path).context(path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("build/sandbox/test-registry").join(name);
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn round_trips_config_and_state() {
        let d = dir("roundtrip");
        let mut c = Config::default();
        c.settings.channel = Some("Stable".into());
        let mut app = App::new(presets()["proton-mail"].source.clone());
        app.channel = Some("EarlyAccess".into());
        app.provides = vec!["proton-mail-bin".into()];
        c.apps.insert("proton-mail".into(), app);
        c.apps.insert("tool".into(), App::new(SourceConfig::Manual {}));
        c.save(&d).unwrap();
        assert_eq!(Config::load(&d).unwrap(), c);

        let text = fs::read_to_string(Config::path(&d)).unwrap();
        assert!(text.contains("[apps.proton-mail.source]\ntype = \"direct\""), "{text}");
        assert!(text.contains("[apps.tool.source]\ntype = \"manual\""), "{text}");

        let mut s = State::default();
        s.apps.insert("tool".into(), AppState { deb_version: Some("1.0-1".into()), pkgrel: 2, ..Default::default() });
        s.save(&d).unwrap();
        assert_eq!(State::load(&d).unwrap(), s);
        assert_eq!(Config::load(&dir("missing")).unwrap(), Config::default());
    }

    #[test]
    fn picks_the_channel() {
        let mut c = Config::default();
        let mut app = App::new(presets()["proton-mail"].source.clone());
        assert_eq!(c.channel(&app).as_deref(), Some("Stable"));
        c.settings.channel = Some("EarlyAccess".into());
        assert_eq!(c.channel(&app).as_deref(), Some("EarlyAccess"));
        app.channel = Some("Alpha".into());
        assert_eq!(c.channel(&app).as_deref(), Some("Alpha"));
        assert!(app.source.uses_channel());
        assert!(!SourceConfig::Manual {}.uses_channel());
    }

    #[test]
    fn rejects_unknown_fields() {
        let bad = "[apps.x.source]\ntype = \"manual\"\nurl = \"nope\"\n";
        assert!(toml::from_str::<Config>(bad).is_err());
        let bad = "[apps.x]\nchanel = \"Stable\"\n[apps.x.source]\ntype = \"manual\"\n";
        assert!(toml::from_str::<Config>(bad).is_err());
    }

    #[test]
    fn presets_parse() {
        let p = presets();
        assert!(p.contains_key("proton-mail"));
        assert!(matches!(p["example-app"].source, SourceConfig::Apt { .. }));
        assert_eq!(p["example-app"].key_fingerprint.as_deref(), Some("A1B2C3D4E5F60718293A4B5C6D7E8F9001122334"));
    }
}
