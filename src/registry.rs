//! The apps pacdeb tracks (apps.toml in the config dir) and what it has built for them
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
    /// Saved apt repositories, by name. Apps with an apt source name one of these.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub apt: BTreeMap<String, AptRepoConfig>,
    #[serde(default)]
    pub apps: BTreeMap<String, App>,
}

/// A saved apt repository.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AptRepoConfig {
    pub url: String,
    /// A suite such as "stable", or a flat repository's folder ending in "/" (like "./").
    pub suite: String,
    /// Empty for a flat repository.
    #[serde(default)]
    pub components: Vec<String>,
    /// Debian architecture; defaults to the machine's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    /// Signing key, relative to the config dir.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Where the key is published, for fetching it again when it changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_url: Option<String>,
    /// The fingerprint the key must have, when pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_fingerprint: Option<String>,
}

impl AptRepoConfig {
    /// A flat repository keeps its index next to the packages instead of under dists/.
    pub fn is_flat(&self) -> bool {
        self.suite.ends_with('/')
    }

    /// The same place as `other`: URL, suite and architecture.
    pub fn same_place(&self, other: &AptRepoConfig) -> bool {
        self.url.trim_end_matches('/') == other.url.trim_end_matches('/') && self.suite == other.suite && self.arch == other.arch
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Channel for apps that do not pick their own, such as "Stable".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// The local pacman repository builds are published to, once set up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<RepoSettings>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoSettings {
    /// Folder holding the packages and database, outside the home folder so pacman's
    /// download user can read it.
    pub dir: String,
    /// The repository's name in pacman.conf.
    pub name: String,
    /// Fingerprint of pacdeb's signing key.
    pub key: String,
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
        /// A regex matched against a text feed; the first capture group (or the whole
        /// match) is a version, and the highest one wins.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version_regex: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url_json: Option<String>,
        /// A sha256 (64 hex) or sha512 (128 hex) checksum for the deb.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        checksum_json: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_channel: Option<String>,
    },
    /// A package from a saved apt repository (`[apt.<name>]`).
    Apt {
        repository: String,
        /// The Debian package name; defaults to the app name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        package: Option<String>,
    },
    Github {
        /// owner/name
        repo: String,
        /// Asset file name pattern, `*` and `?` wildcards.
        asset: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        prerelease: bool,
    },
    /// No remote: new versions come from `pacdeb update <app> --file <deb>`.
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
            SourceConfig::Direct { url, feed, version_json, version_pattern, version_regex, url_json, checksum_json, .. } => {
                [url, feed, version_json, version_pattern, version_regex, url_json, checksum_json]
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
            Ok(text) => Config::parse(&text).context(path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).context(path.display()),
        }
    }

    fn parse(text: &str) -> std::result::Result<Config, toml::de::Error> {
        let mut value: toml::Table = toml::from_str(text)?;
        move_inline_apt_sources(&mut value);
        toml::Value::Table(value).try_into()
    }

    /// The saved repository an app's apt source names.
    pub fn apt_repo(&self, app: &App) -> Option<(&str, &AptRepoConfig)> {
        match &app.source {
            SourceConfig::Apt { repository, .. } => self.apt.get_key_value(repository).map(|(k, v)| (k.as_str(), v)),
            _ => None,
        }
    }

    /// The apps that take packages from saved repository `name`.
    pub fn apps_using(&self, name: &str) -> Vec<&str> {
        self.apps
            .iter()
            .filter(|(_, a)| matches!(&a.source, SourceConfig::Apt { repository, .. } if repository == name))
            .map(|(n, _)| n.as_str())
            .collect()
    }

    pub fn save(&self, config_dir: &Path) -> Result<()> {
        let body = toml::to_string(self).context("writing apps.toml")?;
        let text = format!(
            "# Apps pacdeb tracks. Change it with 'pacdeb add', 'pacdeb set' and 'pacdeb remove';\n\
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

/// apt sources used to carry the repository inline (repo, suite, component, arch, key).
/// Each becomes a saved repository named after its app, or joins an existing saved one
/// for the same place, and the source then names it. Runs on every load, so files
/// written by older versions keep working until they are saved again.
fn move_inline_apt_sources(config: &mut toml::Table) {
    use toml::Value;
    let Some(Value::Table(apps)) = config.get("apps").cloned() else {
        return;
    };
    let mut saved = match config.get("apt") {
        Some(Value::Table(t)) => t.clone(),
        _ => toml::Table::new(),
    };
    let mut new_apps = apps.clone();
    for (name, app) in &apps {
        let Some(source) = app.get("source").and_then(Value::as_table) else {
            continue;
        };
        if source.get("type").and_then(Value::as_str) != Some("apt") || !source.contains_key("repo") {
            continue;
        }
        let text = |k: &str| source.get(k).and_then(Value::as_str).map(String::from);
        let mut repo = toml::Table::new();
        repo.insert("url".into(), Value::String(text("repo").unwrap_or_default()));
        repo.insert("suite".into(), Value::String(text("suite").unwrap_or_default()));
        let component = text("component").unwrap_or_else(|| "main".into());
        repo.insert("components".into(), Value::Array(vec![Value::String(component.clone())]));
        for k in ["arch", "key"] {
            if let Some(v) = text(k) {
                repo.insert(k.into(), Value::String(v));
            }
        }
        let place = |t: &toml::Table| {
            let s = |k: &str| t.get(k).and_then(Value::as_str).map(|v| v.trim_end_matches('/').to_string());
            (s("url"), s("suite"), s("arch"))
        };
        let existing = saved.iter().find(|(_, v)| v.as_table().is_some_and(|t| place(t) == place(&repo))).map(|(k, _)| k.clone());
        let repo_name = match existing {
            Some(k) => {
                // Same place: make sure its components include this one.
                if let Some(Value::Array(cs)) = saved.get_mut(&k).and_then(|v| v.as_table_mut()).and_then(|t| t.get_mut("components")) {
                    if !cs.iter().any(|c| c.as_str() == Some(component.as_str())) {
                        cs.push(Value::String(component));
                    }
                }
                k
            }
            None => {
                // A repository a preset knows gets the preset's key link and pinned fingerprint.
                let url = text("repo").unwrap_or_default();
                let known = presets().into_values().filter_map(|p| p.apt).find(|a| a.url.trim_end_matches('/') == url.trim_end_matches('/'));
                if let Some(a) = known {
                    for (k, v) in [("key_url", a.key_url), ("key_fingerprint", a.key_fingerprint)] {
                        if let Some(v) = v {
                            repo.insert(k.into(), Value::String(v));
                        }
                    }
                }
                let mut k = name.clone();
                while saved.contains_key(&k) {
                    k.push_str("-repo");
                }
                saved.insert(k.clone(), Value::Table(repo));
                k
            }
        };
        let mut new_source = toml::Table::new();
        new_source.insert("type".into(), Value::String("apt".into()));
        new_source.insert("repository".into(), Value::String(repo_name));
        if let Some(p) = text("package") {
            new_source.insert("package".into(), Value::String(p));
        }
        if let Some(Value::Table(a)) = new_apps.get_mut(name) {
            a.insert("source".into(), Value::Table(new_source));
        }
    }
    config.insert("apps".into(), Value::Table(new_apps));
    if !saved.is_empty() {
        config.insert("apt".into(), Value::Table(saved));
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
        write_atomic(&state_dir.join("state.toml"), &format!("# Written by pacdeb. Do not edit.\n\n{body}"))
    }
}

/// A preset: a ready made source for a known app.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub source: SourceConfig,
    /// For apt presets: the repository, saved under the name the source gives it.
    #[serde(default)]
    pub apt: Option<AptRepoConfig>,
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
        let apt = p["example-app"].apt.as_ref().unwrap();
        assert_eq!(apt.key_fingerprint.as_deref(), Some("A1B2C3D4E5F60718293A4B5C6D7E8F9001122334"));
        assert_eq!(apt.components, ["main"]);
    }

    #[test]
    fn moves_inline_apt_sources_into_saved_repositories() {
        let old = r#"
[apps.example-app.source]
type = "apt"
repo = "https://apt.example.com/app/stable"
suite = "stable"
component = "main"
package = "example-app"
key = "keys/example-app.asc"

[apps.other.source]
type = "apt"
repo = "https://apt.example.com/app/stable/"
suite = "stable"
component = "beta"

[apps.tool.source]
type = "manual"
"#;
        let c = Config::parse(old).unwrap();
        let repo = &c.apt["example-app"];
        assert_eq!(repo.url, "https://apt.example.com/app/stable");
        assert_eq!(repo.components, ["main", "beta"]);
        assert_eq!(repo.key.as_deref(), Some("keys/example-app.asc"));
        assert_eq!(repo.key_fingerprint.as_deref(), Some("A1B2C3D4E5F60718293A4B5C6D7E8F9001122334"), "filled in from the preset");
        assert_eq!(c.apt.len(), 1, "both apps share one repository");
        assert_eq!(c.apps["example-app"].source, SourceConfig::Apt { repository: "example-app".into(), package: Some("example-app".into()) });
        assert_eq!(c.apps["other"].source, SourceConfig::Apt { repository: "example-app".into(), package: None });
        assert_eq!(c.apps_using("example-app"), ["example-app", "other"]);

        // Saving writes the new form, which loads back the same.
        let text = toml::to_string(&c).unwrap();
        assert!(text.contains("[apt.example-app]"), "{text}");
        assert!(!text.contains("component = "), "{text}");
        assert_eq!(Config::parse(&text).unwrap(), c);
    }
}
