//! The registry commands: add, set, list, remove and check.

use std::process::Command;

use crate::error::{Result, bail};
use crate::paths::Paths;
use crate::aptrepos::{self, KeySource};
use crate::registry::{App, AppState, AptRepoConfig, Config, SourceConfig, State, presets};
use crate::sources::{self, Latest};
use crate::style::Style;
use crate::version::DebVersion;

/// Options shared by add and set. Unset fields leave things as they are.
#[derive(Debug, Default)]
pub struct Flags {
    pub preset: Option<String>,
    pub source: Option<String>,
    pub channel: Option<String>,
    pub key: Option<String>,
    pub key_url: Option<String>,
    pub key_fingerprint: Option<String>,
    pub pkgname: Option<String>,
    pub provides: Option<Vec<String>>,
    pub conflicts: Option<Vec<String>>,
    pub depends: Option<Vec<String>>,
    pub no_depends: Option<Vec<String>>,
    pub url: Option<String>,
    pub feed: Option<String>,
    pub version_json: Option<String>,
    pub version_pattern: Option<String>,
    pub version_regex: Option<String>,
    pub url_json: Option<String>,
    pub checksum_json: Option<String>,
    pub repo: Option<String>,
    /// A saved apt repository, for apt sources.
    pub apt: Option<String>,
    pub suite: Option<String>,
    pub component: Option<String>,
    pub package: Option<String>,
    pub arch: Option<String>,
    pub asset: Option<String>,
    pub prerelease: Option<bool>,
}

/// Splits add/set arguments into positional words and flags.
pub fn parse_flags(args: &[String]) -> Result<(Vec<String>, Flags)> {
    let mut f = Flags::default();
    let mut positional = Vec::new();
    let mut it = args.iter();
    let list = |v: &str| -> Vec<String> { v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect() };
    while let Some(a) = it.next() {
        if !a.starts_with("--") {
            positional.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--prerelease" => f.prerelease = Some(true),
            "--no-prerelease" => f.prerelease = Some(false),
            flag => {
                let Some(v) = it.next() else {
                    bail!("{flag} needs a value");
                };
                let v = v.clone();
                match flag {
                    "--preset" => f.preset = Some(v),
                    "--source" => f.source = Some(v),
                    "--channel" => f.channel = Some(v),
                    "--key" => f.key = Some(v),
                    "--key-url" => f.key_url = Some(v),
                    "--key-fingerprint" => f.key_fingerprint = Some(v),
                    "--pkgname" => f.pkgname = Some(v),
                    "--provides" => f.provides = Some(list(&v)),
                    "--conflicts" => f.conflicts = Some(list(&v)),
                    "--depends" => f.depends = Some(list(&v)),
                    "--no-depends" => f.no_depends = Some(list(&v)),
                    "--url" => f.url = Some(v),
                    "--feed" => f.feed = Some(v),
                    "--version-json" => f.version_json = Some(v),
                    "--version-pattern" => f.version_pattern = Some(v),
                    "--version-regex" => f.version_regex = Some(v),
                    "--url-json" => f.url_json = Some(v),
                    "--checksum-json" => f.checksum_json = Some(v),
                    "--repo" => f.repo = Some(v),
                    "--apt" => f.apt = Some(v),
                    "--suite" => f.suite = Some(v),
                    "--component" => f.component = Some(v),
                    "--package" => f.package = Some(v),
                    "--arch" => f.arch = Some(v),
                    "--asset" => f.asset = Some(v),
                    other => bail!("unknown option '{other}'"),
                }
            }
        }
    }
    Ok((positional, f))
}

fn check_app_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && !name.starts_with(['-', '.'])
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "@._+-".contains(c));
    if !ok {
        bail!("'{name}' is not a usable app name; use lowercase letters, digits and @._+-");
    }
    Ok(())
}

/// A new source of the given type from flags alone.
fn source_from_flags(kind: &str, f: &Flags) -> Result<SourceConfig> {
    let need = |v: &Option<String>, flag: &str| -> Result<String> {
        v.clone().ok_or_else(|| crate::error::Error::new(format!("a {kind} source needs {flag}")))
    };
    Ok(match kind {
        "direct" => {
            if f.url.is_none() && f.feed.is_none() {
                bail!("a direct source needs --url, --feed, or both");
            }
            SourceConfig::Direct {
                url: f.url.clone(),
                feed: f.feed.clone(),
                version_json: f.version_json.clone(),
                version_pattern: f.version_pattern.clone(),
                version_regex: f.version_regex.clone(),
                url_json: f.url_json.clone(),
                checksum_json: f.checksum_json.clone(),
                default_channel: None,
            }
        }
        "apt" => unreachable!("apt sources come from apt_source"),
        "github" => SourceConfig::Github { repo: need(&f.repo, "--repo")?, asset: need(&f.asset, "--asset")?, prerelease: f.prerelease.unwrap_or(false) },
        "manual" => SourceConfig::Manual {},
        other => bail!("unknown source type '{other}'; use direct, apt, github or manual"),
    })
}

/// Applies source flags to an existing source. Flags that do not fit its type are an error.
fn update_source(src: &mut SourceConfig, f: &Flags) -> Result<()> {
    let set = |field: &mut Option<String>, v: &Option<String>| {
        if let Some(v) = v {
            *field = if v.is_empty() { None } else { Some(v.clone()) };
        }
    };
    let kind = src.kind();
    let stray = |names: &[(&str, bool)]| -> Result<()> {
        match names.iter().find(|(_, given)| *given) {
            Some((name, _)) => bail!("{name} does not apply to a {kind} source"),
            None => Ok(()),
        }
    };
    match src {
        SourceConfig::Direct { url, feed, version_json, version_pattern, version_regex, url_json, checksum_json, .. } => {
            stray(&[("--repo", f.repo.is_some()), ("--suite", f.suite.is_some()), ("--asset", f.asset.is_some())])?;
            set(url, &f.url);
            set(feed, &f.feed);
            set(version_json, &f.version_json);
            set(version_pattern, &f.version_pattern);
            set(version_regex, &f.version_regex);
            set(url_json, &f.url_json);
            set(checksum_json, &f.checksum_json);
        }
        SourceConfig::Apt { repository, package } => {
            stray(&[("--url", f.url.is_some()), ("--feed", f.feed.is_some()), ("--asset", f.asset.is_some())])?;
            let repo_flags = [("--repo", f.repo.is_some()), ("--suite", f.suite.is_some()), ("--component", f.component.is_some()), ("--arch", f.arch.is_some())];
            if let Some((flag, _)) = repo_flags.iter().find(|(_, given)| *given) {
                bail!("{flag} belongs to the repository now: change it with 'pacdeb apt edit {repository} ...', or move the app with --apt <repository>");
            }
            if let Some(r) = &f.apt {
                *repository = r.clone();
            }
            set(package, &f.package);
        }
        SourceConfig::Github { repo, asset, prerelease } => {
            stray(&[("--url", f.url.is_some()), ("--feed", f.feed.is_some()), ("--suite", f.suite.is_some())])?;
            if let Some(v) = &f.repo {
                *repo = v.clone();
            }
            if let Some(v) = &f.asset {
                *asset = v.clone();
            }
            if let Some(v) = f.prerelease {
                *prerelease = v;
            }
        }
        SourceConfig::Manual {} => {
            stray(&[("--url", f.url.is_some()), ("--feed", f.feed.is_some()), ("--repo", f.repo.is_some()), ("--asset", f.asset.is_some())])?;
        }
    }
    Ok(())
}

fn has_key_flags(f: &Flags) -> bool {
    f.key.is_some() || f.key_url.is_some() || f.key_fingerprint.is_some()
}

fn key_source(f: &Flags) -> KeySource {
    KeySource { file: f.key.clone(), url: f.key_url.clone(), inline: None, fingerprint: f.key_fingerprint.clone() }
}

/// An apt source from flags: --apt names a saved repository; --repo, --suite and friends
/// describe one, which is saved under the app's name (or a saved one for the same place
/// is used).
fn apt_source(name: &str, f: &Flags, config: &mut Config, paths: &Paths) -> Result<SourceConfig> {
    let describes = f.repo.is_some() || f.suite.is_some() || f.component.is_some() || f.arch.is_some();
    let repository = match (&f.apt, describes) {
        (Some(_), true) => bail!("--apt names a saved repository, --repo/--suite describe a new one; give one or the other"),
        (Some(r), false) => {
            if !config.apt.contains_key(r) {
                bail!("no apt repository named {r}; 'pacdeb apt list' shows the saved ones");
            }
            if has_key_flags(f) {
                bail!("{r} has its own signing key; change it with 'pacdeb apt key {r} ...'");
            }
            r.clone()
        }
        (None, _) => {
            let (Some(url), Some(suite)) = (&f.repo, &f.suite) else {
                bail!("an apt source needs --apt <saved repository>, or --repo <url> and --suite <suite> (see 'pacdeb apt add')");
            };
            let repo = AptRepoConfig {
                url: url.trim_end_matches('/').to_string(),
                suite: suite.clone(),
                components: vec![f.component.clone().unwrap_or_else(|| "main".into())],
                arch: f.arch.clone(),
                key: None,
                key_url: None,
                key_fingerprint: None,
            };
            aptrepos::save_or_reuse(config, name, repo, &key_source(f), paths)?
        }
    };
    Ok(SourceConfig::Apt { repository, package: f.package.clone() })
}

/// Applies the app level flags shared by add and set.
fn apply_app_flags(app: &mut App, f: &Flags) {
    if let Some(c) = &f.channel {
        app.channel = if c.is_empty() { None } else { Some(c.clone()) };
    }
    if let Some(p) = &f.pkgname {
        app.pkgname = if p.is_empty() { None } else { Some(p.clone()) };
    }
    for (field, value) in [
        (&mut app.provides, &f.provides),
        (&mut app.conflicts, &f.conflicts),
        (&mut app.extra_depends, &f.depends),
        (&mut app.drop_depends, &f.no_depends),
    ] {
        if let Some(v) = value {
            *field = v.clone();
        }
    }
}

pub fn add(name: &str, f: &Flags) -> Result<()> {
    check_app_name(name)?;
    let paths = Paths::from_env()?;
    let mut config = Config::load(&paths.config)?;
    if config.apps.contains_key(name) {
        bail!("{name} is already tracked; change it with 'pacdeb set {name} ...'");
    }
    let all = presets();
    let preset_name = f.preset.clone().or_else(|| (f.source.is_none() && all.contains_key(name)).then(|| name.to_string()));
    let preset = match &preset_name {
        Some(p) => Some(all.get(p).ok_or_else(|| crate::error::Error::new(format!("no preset named {p}; known: {}", all.keys().cloned().collect::<Vec<_>>().join(", "))))?),
        None => None,
    };
    // --apt names a saved repository, so it means an apt source.
    let kind = f.source.clone().or_else(|| f.apt.is_some().then(|| "apt".to_string()));
    let preset = if kind.is_some() && f.preset.is_none() { None } else { preset };
    let preset_name = preset.and(preset_name);
    let mut source = match (&kind, preset) {
        (Some(kind), _) if kind == "apt" => apt_source(name, f, &mut config, &paths)?,
        (Some(kind), _) => source_from_flags(kind, f)?,
        (None, Some(p)) => match (&p.apt, &p.source) {
            (Some(repo), SourceConfig::Apt { repository, package }) => {
                let key = KeySource {
                    file: f.key.clone(),
                    url: f.key_url.clone().or_else(|| repo.key_url.clone()),
                    inline: None,
                    fingerprint: f.key_fingerprint.clone().or_else(|| repo.key_fingerprint.clone()),
                };
                let used = aptrepos::save_or_reuse(&mut config, repository, repo.clone(), &key, &paths)?;
                SourceConfig::Apt { repository: used, package: package.clone() }
            }
            _ => p.source.clone(),
        },
        (None, None) => bail!(
            "no preset for {name}; describe its source with --source direct|apt|github|manual (presets: {})",
            all.keys().cloned().collect::<Vec<_>>().join(", ")
        ),
    };
    if kind.is_none() && !matches!(source, SourceConfig::Apt { .. }) {
        update_source(&mut source, f)?;
    } else if let (None, SourceConfig::Apt { package, .. }) = (&kind, &mut source) {
        if let Some(p) = &f.package {
            *package = Some(p.clone());
        }
    }
    if has_key_flags(f) && !matches!(source, SourceConfig::Apt { .. }) {
        bail!("--key, --key-url and --key-fingerprint only apply to apt sources");
    }
    let mut app = App::new(source);
    apply_app_flags(&mut app, f);
    let kind = app.source.kind();
    let channel = config.channel(&app);
    config.apps.insert(name.to_string(), app);
    config.save(&paths.config)?;
    match &preset_name {
        Some(p) => println!("Tracking {name} ({kind} source, from the {p} preset)"),
        None => println!("Tracking {name} ({kind} source)"),
    }
    if let Some(c) = channel.filter(|_| config.apps[name].source.uses_channel()) {
        println!("Channel: {c}");
    }
    if let Some(note) = preset.and_then(|p| p.note.as_deref()) {
        println!("{note}");
    }
    println!("Run 'pacdeb check {name}' to see the newest version.");
    Ok(())
}

pub fn set(name: Option<&str>, f: &Flags) -> Result<()> {
    let paths = Paths::from_env()?;
    let mut config = Config::load(&paths.config)?;
    let Some(name) = name else {
        // Without an app, only the global channel can be set.
        let Some(c) = &f.channel else {
            bail!("usage: pacdeb set --channel <name> (global), or pacdeb set <app> <options>");
        };
        config.settings.channel = if c.is_empty() { None } else { Some(c.clone()) };
        config.save(&paths.config)?;
        match &config.settings.channel {
            Some(c) => println!("Global channel: {c} (apps with their own channel keep it)"),
            None => println!("Global channel cleared"),
        }
        return Ok(());
    };
    if !config.apps.contains_key(name) {
        bail!("{name} is not tracked; add it with 'pacdeb add {name}'");
    }
    // --apt on an app that does not come from apt yet moves it to that repository.
    let switching = f.apt.is_some() && !matches!(config.apps[name].source, SourceConfig::Apt { .. });
    let new_source = match f.source.as_deref().or(switching.then_some("apt")) {
        Some("apt") => Some(apt_source(name, f, &mut config, &paths)?),
        Some(kind) => Some(source_from_flags(kind, f)?),
        None => None,
    };
    let app = config.apps.get_mut(name).expect("checked above");
    match new_source {
        Some(src) => app.source = src,
        None => {
            update_source(&mut app.source, f)?;
            if has_key_flags(f) {
                match &app.source {
                    SourceConfig::Apt { repository, .. } => bail!("signing keys belong to the repository; change it with 'pacdeb apt key {repository} ...'"),
                    _ => bail!("--key, --key-url and --key-fingerprint only apply to apt sources"),
                }
            }
        }
    }
    if let SourceConfig::Apt { repository, .. } = &app.source {
        if !config.apt.contains_key(repository) {
            bail!("no apt repository named {repository}; 'pacdeb apt list' shows the saved ones");
        }
    }
    let app = config.apps.get_mut(name).expect("checked above");
    apply_app_flags(app, f);
    let app = app.clone();
    config.save(&paths.config)?;
    println!("Updated {name}");
    if app.source.uses_channel() {
        let own = app.channel.as_deref().map(|c| format!("{c} (set for {name})"));
        let shown = own.or_else(|| config.channel(&app).map(|c| format!("{c} (global or default)")));
        println!("Channel: {}", shown.unwrap_or_else(|| "none set".into()));
    }
    Ok(())
}

pub fn remove(name: &str) -> Result<()> {
    let paths = Paths::from_env()?;
    let mut config = Config::load(&paths.config)?;
    let Some(app) = config.apps.remove(name) else {
        bail!("{name} is not tracked; 'pacdeb list' shows the tracked apps");
    };
    config.save(&paths.config)?;
    let mut state = State::load(&paths.state)?;
    if state.apps.remove(name).is_some() {
        state.save(&paths.state)?;
    }
    let pkg = app.pkgname.as_deref().unwrap_or(name);
    if let Some(repo) = &config.settings.repo {
        match crate::repo::unpublish(pkg, &paths, repo) {
            Ok(true) => println!("Took {pkg} out of the [{}] repository.", repo.name),
            Ok(false) => {}
            Err(e) => println!("warning: could not take {pkg} out of the repository: {e}"),
        }
    }
    println!("Stopped tracking {name}. The package stays installed; remove it with 'sudo pacman -R {pkg}' if you want.");
    Ok(())
}

/// pacman's installed version of a package, if any.
pub fn installed_version(pkg: &str) -> Option<String> {
    let out = Command::new("pacman").args(["-Q", pkg]).env("LC_ALL", "C").output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).split_whitespace().nth(1).map(String::from)
}

pub fn list() -> Result<()> {
    let paths = Paths::from_env()?;
    let config = Config::load(&paths.config)?;
    let state = State::load(&paths.state)?;
    if config.apps.is_empty() {
        println!("No apps tracked. Add one with 'pacdeb add <name>'.");
        return Ok(());
    }
    let st = Style::for_stdout();
    let rows: Vec<[String; 5]> = config
        .apps
        .iter()
        .map(|(name, app)| {
            let s = state.apps.get(name);
            let built = s.and_then(|s| s.deb_version.clone().map(|v| format!("{v} (rel {})", s.pkgrel))).unwrap_or_else(|| "-".into());
            let channel = if app.source.uses_channel() {
                match (&app.channel, config.channel(app)) {
                    (Some(c), _) => c.clone(),
                    (None, Some(c)) => format!("{c}*"),
                    (None, None) => "none!".into(),
                }
            } else {
                "-".into()
            };
            let pkg = app.pkgname.as_deref().unwrap_or(name);
            let installed = installed_version(pkg).unwrap_or_else(|| "-".into());
            [name.clone(), app.source.kind().to_string(), channel, built, installed]
        })
        .collect();
    let header = ["APP", "SOURCE", "CHANNEL", "BUILT (DEB VERSION)", "INSTALLED"];
    let widths: Vec<usize> = (0..5).map(|i| rows.iter().map(|r| r[i].len()).chain([header[i].len()]).max().unwrap_or(0)).collect();
    let line = |cols: &[String; 5]| cols.iter().zip(&widths).map(|(c, w)| format!("{c:<w$}")).collect::<Vec<_>>().join("  ");
    println!("{}", st.bold(line(&header.map(String::from)).trim_end()));
    for r in &rows {
        println!("{}", line(r).trim_end());
    }
    if rows.iter().any(|r| r[2].ends_with('*')) {
        println!("{}", st.dim("* channel comes from the global setting or the source's default"));
    }
    Ok(())
}

/// What check found for one app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    UpToDate(String),
    Newer { current: Option<String>, latest: String },
    /// Direct source without a feed: whether the file changed since the last download.
    Changed(bool),
    Manual,
}

pub fn status(latest: &Latest, state: Option<&AppState>) -> Status {
    let current = state.and_then(|s| s.deb_version.clone());
    if let Some(v) = &latest.version {
        let newer = match &current {
            None => true,
            Some(c) => match (DebVersion::parse(v), DebVersion::parse(c)) {
                (Ok(l), Ok(c)) => l > c,
                _ => v != c,
            },
        };
        return if newer { Status::Newer { current, latest: v.clone() } } else { Status::UpToDate(v.clone()) };
    }
    if let Some(head) = &latest.head {
        let s = state.cloned().unwrap_or_default();
        let unchanged = s.deb_version.is_some()
            && ((head.etag.is_some() && head.etag == s.etag) || (head.last_modified.is_some() && head.last_modified == s.last_modified));
        return Status::Changed(!unchanged);
    }
    Status::Manual
}

/// Asks one tracked app's source what its newest version is, downloading no debs.
pub fn check_app(name: &str, config: &Config, state: &State, paths: &Paths) -> Result<Status> {
    let Some(app) = config.apps.get(name) else {
        bail!("{name} is not tracked; 'pacdeb list' shows the tracked apps");
    };
    let channel = config.channel(app);
    let latest = sources::latest(name, &app.source, channel.as_deref(), config, &paths.config, &paths.cache)?;
    Ok(status(&latest, state.apps.get(name)))
}

/// `notify` sends a desktop notification when the updates found differ from the ones
/// last notified about (used by the timer).
pub fn check(name: Option<&str>, notify: bool) -> Result<()> {
    let paths = Paths::from_env()?;
    let config = Config::load(&paths.config)?;
    let state = State::load(&paths.state)?;
    let names: Vec<&String> = match name {
        Some(n) => vec![config.apps.get_key_value(n).map(|(k, _)| k).ok_or_else(|| crate::error::Error::new(format!("{n} is not tracked; 'pacdeb list' shows the tracked apps")))?],
        None => config.apps.keys().collect(),
    };
    if names.is_empty() {
        println!("No apps tracked. Add one with 'pacdeb add <name>'.");
        return Ok(());
    }
    let st = Style::for_stdout();
    let width = names.iter().map(|n| n.len()).max().unwrap_or(0);
    let total = names.len();
    let mut failed = 0;
    let mut pending = Vec::new();
    for n in names {
        let app = &config.apps[n];
        let channel = config.channel(app);
        let label = format!("{n:<width$}");
        let shown_channel = channel.as_deref().filter(|_| app.source.uses_channel()).map(|c| format!(" [{c}]")).unwrap_or_default();
        match check_app(n, &config, &state, &paths) {
            Err(e) => {
                failed += 1;
                println!("{label}  {} {e}", st.bad("error:"));
            }
            Ok(found) => match found {
                Status::UpToDate(v) => println!("{label}  up to date: {v}{shown_channel}"),
                Status::Newer { current: Some(c), latest } => {
                    println!("{label}  {} {c} -> {latest}{shown_channel}", st.warn("update:"));
                    pending.push(format!("{n} {c} -> {latest}"));
                }
                Status::Newer { current: None, latest } => {
                    println!("{label}  {} {latest}{shown_channel} (not built by pacdeb yet)", st.warn("available:"));
                    pending.push(format!("{n} {latest}"));
                }
                Status::Changed(true) => {
                    println!("{label}  {} the download changed since the last build", st.warn("maybe:"));
                    pending.push(format!("{n} (new download)"));
                }
                Status::Changed(false) => println!("{label}  unchanged since the last build"),
                Status::Manual => println!("{label}  manual source: update with 'pacdeb update {n} --file <deb>'"),
            },
        }
    }
    if notify {
        if let Err(e) = crate::notify::updates(&pending, &paths) {
            println!("{} could not send a notification: {e}", st.warn("warning:"));
        }
    }
    if failed > 0 {
        match total {
            1 => bail!("the check failed"),
            _ => bail!("{failed} of {total} apps could not be checked"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::Head;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parses_flags() {
        let (pos, f) = parse_flags(&args("proton-mail --channel EarlyAccess --provides a,b --prerelease --depends x")).unwrap();
        assert_eq!(pos, ["proton-mail"]);
        assert_eq!(f.channel.as_deref(), Some("EarlyAccess"));
        assert_eq!(f.provides, Some(vec!["a".to_string(), "b".to_string()]));
        assert_eq!(f.prerelease, Some(true));
        assert_eq!(f.depends, Some(vec!["x".to_string()]));
        assert!(parse_flags(&args("x --channel")).unwrap_err().to_string().contains("needs a value"));
        assert!(parse_flags(&args("x --bogus 1")).unwrap_err().to_string().contains("unknown option"));
    }

    #[test]
    fn builds_sources_from_flags() {
        let (_, f) = parse_flags(&args("--repo o/r --asset *.deb")).unwrap();
        assert_eq!(source_from_flags("github", &f).unwrap(), SourceConfig::Github { repo: "o/r".into(), asset: "*.deb".into(), prerelease: false });
        assert!(source_from_flags("direct", &f).unwrap_err().to_string().contains("--url, --feed"));
        assert!(source_from_flags("ftp", &f).is_err());

        let mut src = presets()["proton-mail"].source.clone();
        let (_, f) = parse_flags(&args("--asset x")).unwrap();
        assert!(update_source(&mut src, &f).unwrap_err().to_string().contains("--asset does not apply to a direct source"));
    }

    #[test]
    fn apt_sources_name_a_saved_repository() {
        let paths = Paths { config: "/nonexistent/c".into(), state: "/nonexistent/s".into(), cache: "/nonexistent/k".into() };
        let mut config = Config::default();
        config.apt.insert("vendor".into(), AptRepoConfig { url: "https://x".into(), suite: "stable".into(), components: vec!["main".into()], arch: None, key: None, key_url: None, key_fingerprint: None });
        let (_, f) = parse_flags(&args("--apt vendor --package tool-bin")).unwrap();
        assert_eq!(apt_source("tool", &f, &mut config, &paths).unwrap(), SourceConfig::Apt { repository: "vendor".into(), package: Some("tool-bin".into()) });
        let cases = [
            ("--apt nope", "no apt repository named nope"),
            ("--apt vendor --repo https://y --suite s", "one or the other"),
            ("--apt vendor --key-url https://k", "its own signing key"),
            ("--package x", "needs --apt"),
        ];
        for (flags, want) in cases {
            let (_, f) = parse_flags(&args(flags)).unwrap();
            let err = apt_source("tool", &f, &mut config, &paths).unwrap_err().to_string();
            assert!(err.contains(want), "{flags}: {err}");
        }
        // Moving an app between repositories, and refusing repository flags on the app.
        let mut src = SourceConfig::Apt { repository: "vendor".into(), package: None };
        let (_, f) = parse_flags(&args("--apt other --package p")).unwrap();
        update_source(&mut src, &f).unwrap();
        assert_eq!(src, SourceConfig::Apt { repository: "other".into(), package: Some("p".into()) });
        let (_, f) = parse_flags(&args("--suite beta")).unwrap();
        assert!(update_source(&mut src, &f).unwrap_err().to_string().contains("pacdeb apt edit other"));
    }

    #[test]
    fn decides_status() {
        let st = |v: &str| AppState { deb_version: Some(v.into()), pkgrel: 1, ..Default::default() };
        let l = |v: &str| Latest { version: Some(v.into()), ..Latest::default() };
        assert!(matches!(status(&l("1.15.0"), Some(&st("1.14.0"))), Status::Newer { current: Some(_), .. }));
        assert!(matches!(status(&l("1.14.0"), Some(&st("1.15.0"))), Status::UpToDate(_)));
        assert!(matches!(status(&l("1.0~rc1"), Some(&st("1.0"))), Status::UpToDate(_)));
        assert!(matches!(status(&l("1.0"), None), Status::Newer { current: None, .. }));
        assert!(matches!(status(&Latest::default(), None), Status::Manual));

        let head = Head { etag: Some("\"a\"".into()), last_modified: None };
        let lh = Latest { head: Some(head.clone()), ..Latest::default() };
        let mut s = st("1.0");
        assert!(matches!(status(&lh, Some(&s)), Status::Changed(true)));
        s.etag = head.etag.clone();
        assert!(matches!(status(&lh, Some(&s)), Status::Changed(false)));
    }

    #[test]
    fn app_names() {
        for ok in ["proton-mail", "app2", "a.b+c@d"] {
            assert!(check_app_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-x", "App", "a b", "a/b"] {
            assert!(check_app_name(bad).is_err(), "{bad}");
        }
    }
}
