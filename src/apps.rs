//! The registry commands: add, set, list, remove and check.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::error::{Context, Result, bail};
use crate::paths::Paths;
use crate::registry::{App, AppState, Config, SourceConfig, State, presets};
use crate::sources::{self, Latest, gpg};
use crate::style::Style;
use crate::version::DebVersion;

/// Options shared by add and set. Unset fields leave things as they are.
#[derive(Debug, Default)]
pub struct Flags {
    pub preset: Option<String>,
    pub source: Option<String>,
    pub channel: Option<String>,
    pub key: Option<String>,
    pub pkgname: Option<String>,
    pub provides: Option<Vec<String>>,
    pub conflicts: Option<Vec<String>>,
    pub depends: Option<Vec<String>>,
    pub no_depends: Option<Vec<String>>,
    pub url: Option<String>,
    pub feed: Option<String>,
    pub version_json: Option<String>,
    pub version_pattern: Option<String>,
    pub url_json: Option<String>,
    pub checksum_json: Option<String>,
    pub repo: Option<String>,
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
                    "--pkgname" => f.pkgname = Some(v),
                    "--provides" => f.provides = Some(list(&v)),
                    "--conflicts" => f.conflicts = Some(list(&v)),
                    "--depends" => f.depends = Some(list(&v)),
                    "--no-depends" => f.no_depends = Some(list(&v)),
                    "--url" => f.url = Some(v),
                    "--feed" => f.feed = Some(v),
                    "--version-json" => f.version_json = Some(v),
                    "--version-pattern" => f.version_pattern = Some(v),
                    "--url-json" => f.url_json = Some(v),
                    "--checksum-json" => f.checksum_json = Some(v),
                    "--repo" => f.repo = Some(v),
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
                url_json: f.url_json.clone(),
                checksum_json: f.checksum_json.clone(),
                default_channel: None,
            }
        }
        "apt" => SourceConfig::Apt {
            repo: need(&f.repo, "--repo")?,
            suite: need(&f.suite, "--suite")?,
            component: f.component.clone().unwrap_or_else(|| "main".into()),
            package: f.package.clone(),
            arch: f.arch.clone(),
            key: None,
        },
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
        SourceConfig::Direct { url, feed, version_json, version_pattern, url_json, checksum_json, .. } => {
            stray(&[("--repo", f.repo.is_some()), ("--suite", f.suite.is_some()), ("--asset", f.asset.is_some())])?;
            set(url, &f.url);
            set(feed, &f.feed);
            set(version_json, &f.version_json);
            set(version_pattern, &f.version_pattern);
            set(url_json, &f.url_json);
            set(checksum_json, &f.checksum_json);
        }
        SourceConfig::Apt { repo, suite, component, package, arch, .. } => {
            stray(&[("--url", f.url.is_some()), ("--feed", f.feed.is_some()), ("--asset", f.asset.is_some())])?;
            if let Some(v) = &f.repo {
                *repo = v.clone();
            }
            if let Some(v) = &f.suite {
                *suite = v.clone();
            }
            if let Some(v) = &f.component {
                *component = v.clone();
            }
            set(package, &f.package);
            set(arch, &f.arch);
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

/// Copies a signing key into the config dir, checking its fingerprint when one is known.
fn import_key(name: &str, key: &Path, expected: Option<&str>, paths: &Paths) -> Result<String> {
    let fprs = gpg::fingerprints(key, &paths.cache.join("gnupg").join("check"))?;
    if fprs.is_empty() {
        bail!("{} holds no public key", key.display());
    }
    if let Some(want) = expected {
        let want = want.replace(' ', "").to_ascii_uppercase();
        if !fprs.iter().any(|f| f.eq_ignore_ascii_case(&want)) {
            bail!("{} has fingerprint {}, but {name}'s published key is {want}; not using it", key.display(), fprs.join(", "));
        }
    }
    let rel = format!("keys/{name}.asc");
    let dest = paths.config.join(&rel);
    fs::create_dir_all(dest.parent().unwrap()).context(paths.config.display())?;
    fs::copy(key, &dest).context(dest.display())?;
    println!("Using signing key {} (fingerprint {})", key.display(), fprs.join(", "));
    Ok(rel)
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
        bail!("{name} is already tracked; change it with 'ferry set {name} ...'");
    }
    let all = presets();
    let preset_name = f.preset.clone().or_else(|| (f.source.is_none() && all.contains_key(name)).then(|| name.to_string()));
    let preset = match &preset_name {
        Some(p) => Some(all.get(p).ok_or_else(|| crate::error::Error::new(format!("no preset named {p}; known: {}", all.keys().cloned().collect::<Vec<_>>().join(", "))))?),
        None => None,
    };
    let mut source = match (&f.source, preset) {
        (Some(kind), _) => source_from_flags(kind, f)?,
        (None, Some(p)) => p.source.clone(),
        (None, None) => bail!(
            "no preset for {name}; describe its source with --source direct|apt|github|manual (presets: {})",
            all.keys().cloned().collect::<Vec<_>>().join(", ")
        ),
    };
    if f.source.is_none() {
        update_source(&mut source, f)?;
    }
    if let SourceConfig::Apt { key, .. } = &mut source {
        match &f.key {
            Some(file) => *key = Some(import_key(name, Path::new(file), preset.and_then(|p| p.key_fingerprint.as_deref()), &paths)?),
            None => {
                let hint = preset
                    .and_then(|p| p.key_url.as_deref())
                    .map(|u| format!("\nIts key is published at {u}; download it, for example:\n  curl -fsSLo {name}.asc {u}\nthen run this again with --key {name}.asc"))
                    .unwrap_or_default();
                bail!("an apt source needs the repository's signing key: add --key <file>{hint}");
            }
        }
    } else if f.key.is_some() {
        bail!("--key only applies to apt sources");
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
    println!("Run 'ferry check {name}' to see the newest version.");
    Ok(())
}

pub fn set(name: Option<&str>, f: &Flags) -> Result<()> {
    let paths = Paths::from_env()?;
    let mut config = Config::load(&paths.config)?;
    let Some(name) = name else {
        // Without an app, only the global channel can be set.
        let Some(c) = &f.channel else {
            bail!("usage: ferry set --channel <name> (global), or ferry set <app> <options>");
        };
        config.settings.channel = if c.is_empty() { None } else { Some(c.clone()) };
        config.save(&paths.config)?;
        match &config.settings.channel {
            Some(c) => println!("Global channel: {c} (apps with their own channel keep it)"),
            None => println!("Global channel cleared"),
        }
        return Ok(());
    };
    let Some(app) = config.apps.get_mut(name) else {
        bail!("{name} is not tracked; add it with 'ferry add {name}'");
    };
    if let Some(kind) = &f.source {
        app.source = source_from_flags(kind, f)?;
    } else {
        update_source(&mut app.source, f)?;
    }
    if let Some(file) = &f.key {
        let SourceConfig::Apt { .. } = &app.source else {
            bail!("--key only applies to apt sources");
        };
        let expected = presets().get(name).and_then(|p| p.key_fingerprint.clone());
        let rel = import_key(name, Path::new(file), expected.as_deref(), &paths)?;
        if let SourceConfig::Apt { key, .. } = &mut app.source {
            *key = Some(rel);
        }
    }
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
        bail!("{name} is not tracked");
    };
    config.save(&paths.config)?;
    let mut state = State::load(&paths.state)?;
    if state.apps.remove(name).is_some() {
        state.save(&paths.state)?;
    }
    let pkg = app.pkgname.as_deref().unwrap_or(name);
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
        println!("No apps tracked. Add one with 'ferry add <name>'.");
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

pub fn check(name: Option<&str>) -> Result<()> {
    let paths = Paths::from_env()?;
    let config = Config::load(&paths.config)?;
    let state = State::load(&paths.state)?;
    let names: Vec<&String> = match name {
        Some(n) => vec![config.apps.get_key_value(n).map(|(k, _)| k).ok_or_else(|| crate::error::Error::new(format!("{n} is not tracked")))?],
        None => config.apps.keys().collect(),
    };
    if names.is_empty() {
        println!("No apps tracked. Add one with 'ferry add <name>'.");
        return Ok(());
    }
    let st = Style::for_stdout();
    let width = names.iter().map(|n| n.len()).max().unwrap_or(0);
    let mut failed = false;
    for n in names {
        let app = &config.apps[n];
        let channel = config.channel(app);
        let label = format!("{n:<width$}");
        let shown_channel = channel.as_deref().filter(|_| app.source.uses_channel()).map(|c| format!(" [{c}]")).unwrap_or_default();
        match sources::latest(n, &app.source, channel.as_deref(), &paths.config, &paths.cache) {
            Err(e) => {
                failed = true;
                println!("{label}  {} {e}", st.bad("error:"));
            }
            Ok(latest) => match status(&latest, state.apps.get(n)) {
                Status::UpToDate(v) => println!("{label}  up to date: {v}{shown_channel}"),
                Status::Newer { current: Some(c), latest } => {
                    println!("{label}  {} {c} -> {latest}{shown_channel}", st.warn("update:"))
                }
                Status::Newer { current: None, latest } => {
                    println!("{label}  {} {latest}{shown_channel} (not built by Ferry yet)", st.warn("available:"))
                }
                Status::Changed(true) => println!("{label}  {} the download changed since the last build", st.warn("maybe:")),
                Status::Changed(false) => println!("{label}  unchanged since the last build"),
                Status::Manual => println!("{label}  manual source: update with 'ferry update {n} --file <deb>'"),
            },
        }
    }
    if failed {
        bail!("some apps could not be checked");
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
        assert!(source_from_flags("apt", &f).unwrap_err().to_string().contains("needs --suite"));
        assert!(source_from_flags("direct", &f).unwrap_err().to_string().contains("--url, --feed"));
        assert!(source_from_flags("ftp", &f).is_err());

        let mut src = presets()["proton-mail"].source.clone();
        let (_, f) = parse_flags(&args("--asset x")).unwrap();
        assert!(update_source(&mut src, &f).unwrap_err().to_string().contains("--asset does not apply to a direct source"));
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
