// SPDX-License-Identifier: AGPL-3.0-or-later
//! The apt-like commands. `pacdeb update` refreshes: it reads every saved apt
//! repository and asks every app's source for its newest version, and remembers what
//! it found. `pacdeb upgrade` builds and installs whatever is newer, in one pacman call.
//! `pacdeb install <name>` installs a tracked app, a preset, or any package the saved
//! apt repositories offer.

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

/// How long what 'pacdeb update' found is used by 'pacdeb upgrade' before it asks again.
const REFRESH_MAX_AGE: i64 = 24 * 3600;

pub struct Options {
    pub name: Option<String>,
    pub file: Option<PathBuf>,
    pub direct: bool,
    pub no_install: bool,
    /// Send a desktop notification listing what was built (the timer uses this).
    pub notify: bool,
}

/// What a refresh found: the apps with something newer, as "name old -> new".
pub struct Refreshed {
    pub upgradable: Vec<String>,
    pub failed: usize,
}

/// `pacdeb update`: reads every saved apt repository (keeping its package lists) and
/// asks every app's source what is newest, and remembers it. Downloads no debs.
pub fn refresh(quiet: bool) -> Result<Refreshed> {
    let paths = Paths::from_env()?;
    let config = Config::load(&paths.config)?;
    let mut state = State::load(&paths.state)?;
    let st = Style::for_stdout();
    let say = |s: String| {
        if !quiet {
            println!("{s}");
        }
    };
    let now = crate::sources::release::now();
    let mut failed = 0;
    let mut lists: std::collections::BTreeMap<String, std::result::Result<Vec<(String, String)>, String>> = Default::default();
    for (name, repo) in &config.apt {
        let comps = if repo.is_flat() { String::new() } else { format!(" {}", repo.components.join(" ")) };
        let result = match &repo.key {
            Some(k) => crate::sources::apt_fetch::fetch(name, repo, &paths.config.join(k), &paths.cache).map(|f| f.indexes).map_err(|e| e.to_string()),
            None => Err(format!("no signing key; add one with 'pacdeb apt key {name} --key-url <url>'")),
        };
        match &result {
            Ok(indexes) => {
                // Indexes list every version; count each package once.
                let arch = crate::sources::apt_fetch::arch(repo);
                let mut names: Vec<String> = indexes.iter().flat_map(|(_, i)| crate::sources::apt::list(i, &arch).unwrap_or_default()).map(|l| l.name).collect();
                names.sort();
                names.dedup();
                let count = names.len();
                say(format!("Get: {name} {} {}{comps} ({})", repo.url, repo.suite, crate::human::plural(count, "package", "packages")));
            }
            Err(e) => {
                failed += 1;
                say(format!("{} {name}: {e}", st.bad("Err:")));
            }
        }
        lists.insert(name.clone(), result);
    }
    let mut upgradable = Vec::new();
    for (name, app) in &config.apps {
        let latest = match &app.source {
            SourceConfig::Manual {} => continue,
            SourceConfig::Apt { repository, package } => match (lists.get(repository), config.apt.get(repository)) {
                (Some(Ok(indexes)), Some(repo)) => sources::apt_latest_in(indexes, repository, repo, package.as_deref().unwrap_or(name)),
                (Some(Err(_)), _) => continue,
                _ => Err(crate::error::Error::new(format!("the apt repository {repository} is not saved"))),
            },
            _ => {
                let channel = config.channel(app);
                let latest = sources::latest(name, &app.source, channel.as_deref(), &config, &paths.config, &paths.cache);
                if latest.is_ok() {
                    say(format!("Get: {name} ({})", app.source.kind()));
                }
                latest
            }
        };
        match latest {
            Ok(l) => {
                if let Status::Newer { current, latest } = status(&l, state.apps.get(name)) {
                    upgradable.push(match current {
                        Some(c) => format!("{name} {c} -> {latest}"),
                        None => format!("{name} {latest} (not built yet)"),
                    });
                } else if matches!(status(&l, state.apps.get(name)), Status::Changed(true)) {
                    upgradable.push(format!("{name} (new download)"));
                }
                state.apps.entry(name.clone()).or_default().available = Some(l.to_available(now));
            }
            Err(e) => {
                failed += 1;
                say(format!("{} {name}: {e}", st.bad("Err:")));
            }
        }
    }
    state.save(&paths.state)?;
    if !quiet {
        match upgradable.len() {
            0 => println!("All apps are up to date."),
            n => {
                println!("{} can be upgraded. Run 'pacdeb upgrade' to install, or 'pacdeb list --upgradable' to see them:", crate::human::plural(n, "app", "apps"));
                for u in &upgradable {
                    println!("  {u}");
                }
            }
        }
    }
    if failed > 0 && !quiet {
        println!("{} {} could not be read; see the errors above.", st.warn("warning:"), crate::human::plural(failed, "source", "sources"));
    }
    Ok(Refreshed { upgradable, failed })
}

/// What an app's source offers: what the last refresh found when that is recent,
/// otherwise asked now.
fn latest_for(name: &str, app: &App, config: &Config, paths: &Paths, state: &State) -> Result<Latest> {
    let now = crate::sources::release::now();
    if let Some(a) = state.apps.get(name).and_then(|s| s.available.as_ref()).filter(|a| now - a.checked < REFRESH_MAX_AGE) {
        if a.url.is_some() {
            return Ok(Latest::from_available(a));
        }
    }
    let channel = config.channel(app);
    sources::latest(name, &app.source, channel.as_deref(), config, &paths.config, &paths.cache)
}

/// `pacdeb upgrade`: builds whatever is newer than the last build and installs it all
/// with one pacman call.
pub fn upgrade(opts: &Options) -> Result<()> {
    let paths = Paths::from_env()?;
    let config = Config::load(&paths.config)?;
    let mut state = State::load(&paths.state)?;
    let st = Style::for_stdout();

    if opts.file.is_some() && opts.name.is_none() {
        bail!("--file needs an app name: pacdeb upgrade <app> --file <deb>");
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
    let mut built_names = Vec::new();
    let mut failed = Vec::new();
    for name in &names {
        let app = &config.apps[name];
        let result = match &opts.file {
            Some(file) => build_and_record(name, app, file, None, opts.direct, &paths, &mut state).map(Some),
            None => update_one(name, app, &config, opts.direct, &paths, &mut state),
        };
        match result {
            Ok(Some(b)) => {
                built_names.push(format!("{name} {}", b.deb_version));
                built.push(b.path);
            }
            Ok(None) => {}
            Err(e) => {
                println!("{} {name}: {e}", st.bad("error:"));
                failed.push(name.clone());
            }
        }
    }

    if built.is_empty() {
        if failed.is_empty() {
            println!("Nothing to upgrade.");
            return Ok(());
        }
        bail!("could not upgrade: {}", failed.join(", "));
    }
    if opts.notify {
        let ready = if config.settings.repo.is_some() { "Install with your next system update." } else { "Install with 'pacdeb upgrade'." };
        if let Err(e) = crate::notify::built(&built_names, ready) {
            println!("{} could not send a notification: {e}", st.warn("warning:"));
        }
    }
    if opts.no_install {
        for p in &built {
            println!("{} {}", st.good("Built"), p.display());
        }
    } else {
        install::install(&built)?;
    }
    if !failed.is_empty() {
        bail!("could not upgrade: {}", failed.join(", "));
    }
    Ok(())
}

/// Checks one app, and downloads and builds it when there is something newer.
fn update_one(name: &str, app: &App, config: &Config, direct: bool, paths: &Paths, state: &mut State) -> Result<Option<Built>> {
    let latest = latest_for(name, app, config, paths, state)?;
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
            println!("{name}: manual source, nothing to fetch; use 'pacdeb upgrade {name} --file <deb>'");
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
    let mut config = Config::load(&paths.config)?;
    // Keep the new name from now on, so it does not change back if the clash goes away.
    if let Some(pkgname) = &built.renamed {
        if let Some(a) = config.apps.get_mut(name) {
            a.pkgname = Some(pkgname.clone());
            config.save(&paths.config)?;
        }
    }
    // A failed publish leaves a good build, so it only warns.
    if let Some(repo) = &config.settings.repo {
        match crate::repo::publish(&built.path, paths, repo) {
            Ok(()) => println!("{name}: published to the [{}] repository", repo.name),
            Err(e) => println!("{} {name}: could not publish to the repository: {e}", Style::for_stdout().warn("warning:")),
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

/// `pacdeb install <name>`, like apt: a tracked app; else a built in preset; else the
/// newest package of that name in the saved apt repositories, which is then tracked.
/// As in apt, `name/repository` takes the package from that saved repository.
pub fn install_name(spec: &str, direct: bool) -> Result<()> {
    let paths = Paths::from_env()?;
    let config = Config::load(&paths.config)?;
    let (name, only) = match spec.split_once('/') {
        Some((n, r)) => {
            if !config.apt.contains_key(r) {
                bail!("no apt repository named {r}; 'pacdeb apt list' shows the saved ones");
            }
            (n, Some(r))
        }
        None => (spec, None),
    };
    if config.apps.contains_key(name) {
        if let (Some(r), Some((current, _))) = (only, config.apt_repo(&config.apps[name])) {
            if current != r {
                bail!("{name} is tracked from {current}; move it with 'pacdeb set {name} --apt {r}' first");
            }
        }
        return install_app(name, direct);
    }
    if only.is_none() && crate::registry::presets().contains_key(name) {
        crate::apps::add_with(name, &crate::apps::Flags::default(), false)?;
        return install_app(name, direct);
    }
    let (mut all, failed) = crate::aptrepos::all_packages(&config, &paths);
    if let Some(r) = only {
        all.retain(|f| f.repo == r);
    }
    let st = Style::for_stdout();
    for f in &failed {
        println!("{} {f}", st.warn("warning:"));
    }
    let Some(found) = crate::aptrepos::find(&all, name) else {
        let similar: Vec<String> = crate::aptrepos::search(&all, name).into_iter().take(5).map(|f| format!("{}/{}", f.package.name, f.repo)).collect();
        let hint = if similar.is_empty() { String::new() } else { format!("\nSimilar: {}", similar.join(", ")) };
        if config.apt.is_empty() {
            bail!("{name} is not tracked, not built in, and no apt repositories are saved to look in; add one with 'pacdeb apt add'");
        }
        bail!("no package named {name} in the saved apt repositories ('pacdeb update' refreshes their lists){hint}");
    };
    let others: Vec<String> = all.iter().filter(|f| f.package.name == name && f.repo != found.repo).map(|f| format!("{} {}", f.repo, f.package.version)).collect();
    let also = if others.is_empty() { String::new() } else { format!(" (also in {})", others.join(", ")) };
    println!("Found {name} {} in {}{also}", found.package.version, found.repo);
    let flags = crate::apps::Flags { apt: Some(found.repo.clone()), ..Default::default() };
    crate::apps::add_with(name, &flags, false)?;
    install_app(name, direct)
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
    let latest = sources::latest(name, &app.source, channel.as_deref(), &config, &paths.config, &paths.cache)?;
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
