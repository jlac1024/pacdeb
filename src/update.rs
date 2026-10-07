//! `pacdeb update` and `pacdeb install`: fetch new debs, build them and install the
//! results with one pacman call.

use std::fs;
use std::path::{Path, PathBuf};

use crate::apps::{Status, status};
use crate::convert::{self, Built};
use crate::deb::Deb;
use crate::error::{Result, bail};
use crate::install;
use crate::net;
use crate::paths::Paths;
use crate::registry::{App, AppState, Config, SourceConfig, State};
use crate::sources::{self, Latest};
use crate::style::Style;

/// How many built packages to keep per app, for rolling back by hand.
const KEEP_BUILDS: usize = 2;

pub struct Options {
    pub name: Option<String>,
    pub file: Option<PathBuf>,
    pub direct: bool,
    pub no_install: bool,
}

pub fn update(opts: &Options) -> Result<()> {
    let paths = Paths::from_env()?;
    let config = Config::load(&paths.config)?;
    let mut state = State::load(&paths.state)?;
    let st = Style::for_stdout();

    if opts.file.is_some() && opts.name.is_none() {
        bail!("--file needs an app name: pacdeb update <app> --file <deb>");
    }
    let names: Vec<String> = match &opts.name {
        Some(n) if config.apps.contains_key(n) => vec![n.clone()],
        Some(n) => bail!("{n} is not tracked; add it with 'pacdeb add {n}'"),
        None => config.apps.keys().cloned().collect(),
    };
    if names.is_empty() {
        println!("No apps tracked. Add one with 'pacdeb add <name>'.");
        return Ok(());
    }

    let mut built = Vec::new();
    let mut failed = Vec::new();
    for name in &names {
        let app = &config.apps[name];
        let result = match &opts.file {
            Some(file) => build_and_record(name, app, file, None, opts.direct, &paths, &mut state).map(Some),
            None => update_one(name, app, &config, opts.direct, &paths, &mut state),
        };
        match result {
            Ok(Some(b)) => built.push(b.path),
            Ok(None) => {}
            Err(e) => {
                println!("{} {name}: {e}", st.bad("error:"));
                failed.push(name.clone());
            }
        }
    }

    if built.is_empty() {
        if failed.is_empty() {
            println!("Nothing to update.");
            return Ok(());
        }
        bail!("could not update: {}", failed.join(", "));
    }
    if opts.no_install {
        for p in &built {
            println!("{} {}", st.good("Built"), p.display());
        }
    } else {
        install::install(&built)?;
    }
    if !failed.is_empty() {
        bail!("could not update: {}", failed.join(", "));
    }
    Ok(())
}

/// Checks one app, and downloads and builds it when there is something newer.
fn update_one(name: &str, app: &App, config: &Config, direct: bool, paths: &Paths, state: &mut State) -> Result<Option<Built>> {
    let channel = config.channel(app);
    let latest = sources::latest(name, &app.source, channel.as_deref(), &paths.config, &paths.cache)?;
    match status(&latest, state.apps.get(name)) {
        Status::UpToDate(v) => {
            println!("{name}: up to date ({v})");
            Ok(None)
        }
        Status::Changed(false) => {
            println!("{name}: unchanged since the last build");
            Ok(None)
        }
        Status::Manual => {
            println!("{name}: manual source, nothing to fetch; use 'pacdeb update {name} --file <deb>'");
            Ok(None)
        }
        Status::Newer { .. } | Status::Changed(true) => {
            let deb = download(name, &latest, paths)?;
            // Without a feed the version is only known from the deb itself.
            if latest.version.is_none() {
                let v = Deb::open(&deb)?.control.require("Version")?.to_string();
                if state.apps.get(name).and_then(|s| s.deb_version.as_deref()) == Some(v.as_str()) {
                    let s = state.apps.entry(name.to_string()).or_default();
                    remember_head(s, &latest);
                    state.save(&paths.state)?;
                    println!("{name}: the download changed but is still version {v}; nothing to build");
                    return Ok(None);
                }
            }
            build_and_record(name, app, &deb, Some(&latest), direct, paths, state).map(Some)
        }
    }
}

/// Fetches the deb a source offers into the cache.
fn download(name: &str, latest: &Latest, paths: &Paths) -> Result<PathBuf> {
    let Some(url) = &latest.url else {
        bail!("the source gave no download URL");
    };
    let file = url.rsplit('/').next().filter(|f| f.ends_with(".deb")).map(String::from).unwrap_or_else(|| {
        format!("{name}_{}.deb", latest.version.as_deref().unwrap_or("latest"))
    });
    let dir = paths.cache.join("debs").join(name);
    // Keep only the deb being built; old ones are not needed once built.
    if let Ok(entries) = fs::read_dir(&dir) {
        for e in entries.flatten() {
            let _ = fs::remove_file(e.path());
        }
    }
    let dest = dir.join(file);
    let check = if latest.checksum.is_some() { "checking its checksum" } else { "no published checksum to check" };
    println!("{name}: downloading {url} ({check})");
    net::download(url, &dest, latest.checksum.as_ref())?;
    Ok(dest)
}

fn remember_head(s: &mut AppState, latest: &Latest) {
    if let Some(h) = &latest.head {
        s.etag = h.etag.clone();
        s.last_modified = h.last_modified.clone();
    }
}

/// Builds a deb for a tracked app and records it, keeping the last few builds.
fn build_and_record(name: &str, app: &App, deb: &Path, latest: Option<&Latest>, direct: bool, paths: &Paths, state: &mut State) -> Result<Built> {
    let prev = state.apps.get(name).cloned();
    let built = convert::build_package(deb, direct, None, Some(app), prev.as_ref())?;
    record(state, name, &built, latest);
    state.save(&paths.state)?;
    // Keep the new name from now on, so it does not change back if the clash goes away.
    if let Some(pkgname) = &built.renamed {
        let mut config = Config::load(&paths.config)?;
        if let Some(a) = config.apps.get_mut(name) {
            a.pkgname = Some(pkgname.clone());
            config.save(&paths.config)?;
        }
    }
    Ok(built)
}

fn record(state: &mut State, name: &str, built: &Built, latest: Option<&Latest>) {
    let s = state.apps.entry(name.to_string()).or_default();
    s.deb_version = Some(built.deb_version.clone());
    s.pkgrel = built.pkgrel;
    if let Some(l) = latest {
        remember_head(s, l);
    }
    let path = built.path.display().to_string();
    s.packages.retain(|p| *p != path);
    s.packages.push(path);
    while s.packages.len() > KEEP_BUILDS {
        let old = s.packages.remove(0);
        let _ = fs::remove_file(&old);
    }
}

/// `pacdeb install <app>`: the newest version from the app's source, built and installed
/// even when it is already the version pacdeb built last.
pub fn install_app(name: &str, direct: bool) -> Result<()> {
    let paths = Paths::from_env()?;
    let config = Config::load(&paths.config)?;
    let mut state = State::load(&paths.state)?;
    let Some(app) = config.apps.get(name) else {
        bail!("{name} is not tracked; add it with 'pacdeb add {name}', or pass a .deb file");
    };
    if let SourceConfig::Manual {} = app.source {
        bail!("{name} has a manual source; install a deb file with 'pacdeb install <file.deb>'");
    }
    let channel = config.channel(app);
    let latest = sources::latest(name, &app.source, channel.as_deref(), &paths.config, &paths.cache)?;
    // Reuse the last build when it is already the newest version under the app's
    // current package name (it may have been renamed since).
    let last = state.apps.get(name).and_then(|s| {
        let pkgname = app.pkgname.as_deref()?;
        let file = s.packages.last().filter(|p| Path::new(p).exists())?;
        let file_name = Path::new(file).file_name()?.to_string_lossy();
        file_name.strip_prefix(pkgname)?.strip_prefix('-')?.starts_with(|c: char| c.is_ascii_digit()).then_some(())?;
        (latest.version.is_some() && s.deb_version == latest.version).then(|| PathBuf::from(file))
    });
    let pkg = match last {
        Some(p) => {
            println!("{name}: {} is already built", latest.version.as_deref().unwrap_or_default());
            p
        }
        None => {
            let deb = download(name, &latest, &paths)?;
            build_and_record(name, app, &deb, Some(&latest), direct, &paths, &mut state)?.path
        }
    };
    install::install(&[pkg])
}

/// `pacdeb install <file.deb>`: builds and installs a local deb. A deb whose package is
/// not tracked yet is registered as a manual app so list and update know it.
pub fn install_file(deb: &Path, direct: bool) -> Result<()> {
    let paths = Paths::from_env()?;
    let mut config = Config::load(&paths.config)?;
    let mut state = State::load(&paths.state)?;
    let deb_name = Deb::open(deb)?.control.require("Package")?.to_string();
    // A tracked app with that pkgname or name owns this deb.
    let tracked = config
        .apps
        .iter()
        .find(|(n, a)| **n == deb_name || a.pkgname.as_deref() == Some(deb_name.as_str()))
        .map(|(n, a)| (n.clone(), a.clone()));
    let (name, app, registered) = match tracked {
        Some((n, a)) => (n, a, false),
        None => (deb_name.clone(), App::new(SourceConfig::Manual {}), true),
    };
    let built = build_and_record(&name, &app, deb, None, direct, &paths, &mut state)?;
    if registered {
        let mut app = app;
        app.pkgname = built.renamed.clone();
        config.apps.insert(name.clone(), app);
        config.save(&paths.config)?;
        println!("Now tracking {name} as a manual app; give it a source with 'pacdeb set {name} --source ...' to get updates");
    }
    install::install(&[built.path])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built(path: &str, v: &str, rel: u32) -> Built {
        Built { path: PathBuf::from(path), deb_version: v.into(), pkgrel: rel, renamed: None }
    }

    #[test]
    fn records_builds_and_keeps_the_last_two() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("build/sandbox/test-record");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let files: Vec<String> = (1..=3).map(|i| dir.join(format!("app-{i}.pkg.tar.zst")).display().to_string()).collect();
        for f in &files {
            fs::write(f, "x").unwrap();
        }
        let mut state = State::default();
        for (i, f) in files.iter().enumerate() {
            record(&mut state, "app", &built(f, &format!("1.{i}"), 1), None);
        }
        let s = &state.apps["app"];
        assert_eq!(s.packages, files[1..]);
        assert_eq!(s.deb_version.as_deref(), Some("1.2"));
        assert!(!Path::new(&files[0]).exists(), "oldest build should be deleted");
        assert!(Path::new(&files[2]).exists());

        // Recording the same file again does not count twice.
        record(&mut state, "app", &built(&files[2], "1.2", 1), None);
        assert_eq!(state.apps["app"].packages, files[1..]);
    }
}
