// SPDX-License-Identifier: AGPL-3.0-or-later
//! `pacdeb show <name|name/repository>`, like apt show: a package's details from the
//! saved apt repositories, what its dependencies become on Arch, and how pacdeb tracks
//! it. Apps from download links and GitHub show their source and versions instead.

use crate::control::Control;
use crate::error::{Result, bail};
use crate::human;
use crate::paths::Paths;
use crate::registry::{App, Config, SourceConfig, State};
use crate::sources::apt;
use crate::sources::apt_fetch;
use crate::sources::{Latest, release};
use crate::style::Style;
use crate::translate::{LiveSystem, System, Tables, deps};
use crate::version::DebVersion;

/// One package entry found in a saved repository.
struct Entry {
    repo: String,
    version: String,
    paragraph: String,
}

pub fn run(spec: &str) -> Result<()> {
    let paths = Paths::from_env()?;
    let config = Config::load(&paths.config)?;
    let state = State::load(&paths.state)?;
    let (name, only) = match spec.split_once('/') {
        Some((n, r)) if config.apt.contains_key(r) => (n, Some(r)),
        Some((_, r)) => bail!("no apt repository named {r}; 'pacdeb apt list' shows the saved ones"),
        None => (spec, None),
    };
    let st = Style::for_stdout();

    // A tracked app named this way: what it follows, and its package name.
    let tracked = config.apps.iter().find(|(n, a)| {
        *n == name || a.pkgname.as_deref() == Some(name) || matches!(&a.source, SourceConfig::Apt { package: Some(p), .. } if p == name)
    });

    let mut entries = Vec::new();
    for (repo_name, repo) in config.apt.iter().filter(|(n, _)| only.is_none_or(|o| o == *n)) {
        let arch = apt_fetch::arch(repo);
        let package = match tracked {
            Some((n, a)) if matches!(&a.source, SourceConfig::Apt { repository, .. } if repository == repo_name) => match &a.source {
                SourceConfig::Apt { package, .. } => package.clone().unwrap_or_else(|| n.clone()),
                _ => name.to_string(),
            },
            _ => name.to_string(),
        };
        for (_, index) in apt_fetch::cached_indexes(repo_name, &paths.cache) {
            if let Some((version, paragraph)) = apt::details(&index, &package, &arch)? {
                entries.push(Entry { repo: repo_name.clone(), version, paragraph });
            }
        }
    }
    entries.sort_by(|a, b| match (DebVersion::parse(&b.version), DebVersion::parse(&a.version)) {
        (Ok(x), Ok(y)) => x.cmp(&y),
        _ => b.version.cmp(&a.version),
    });

    if let (Some(r), true) = (only, entries.is_empty()) {
        bail!("{r} has no package named {name}; 'pacdeb apt packages {r}' lists what it has");
    }
    if let Some(best) = entries.first() {
        show_entry(best, &config, st)?;
        if entries.len() > 1 {
            let others: Vec<String> = entries[1..].iter().map(|e| format!("{} {}", e.repo, e.version)).collect();
            println!("{}", st.dim(&format!("Also in: {} (pacdeb show {name}/<repository>)", others.join(", "))));
        }
    }
    if let Some((app_name, app)) = tracked {
        if !entries.is_empty() {
            println!();
        }
        show_app(app_name, app, &config, &state, st);
    }
    if entries.is_empty() && tracked.is_none() {
        let cached_any = config.apt.keys().any(|r| !apt_fetch::cached_indexes(r, &paths.cache).is_empty());
        let hint = if cached_any { "'pacdeb search' finds packages by part of their name" } else { "run 'pacdeb update' first to fetch the package lists" };
        bail!("no package or app named {name}; {hint}");
    }
    Ok(())
}

fn show_entry(e: &Entry, config: &Config, st: Style) -> Result<()> {
    let c = Control::parse(&e.paragraph)?;
    let row = |label: &str, value: &str| println!("{} {value}", st.bold(&format!("{label}:")));
    row("Package", c.get("Package").unwrap_or_default());
    row("Version", &e.version);
    if let Some(repo) = config.apt.get(&e.repo) {
        row("Repository", &format!("{} ({} {})", e.repo, repo.url, repo.suite));
    }
    for (label, field) in [("Maintainer", "Maintainer"), ("Homepage", "Homepage"), ("Section", "Section")] {
        if let Some(v) = c.get(field) {
            row(label, v.trim());
        }
    }
    if let Some(kib) = c.get("Installed-Size").and_then(|v| v.trim().parse::<u64>().ok()) {
        row("Installed size", &human::size(kib * 1024));
    }
    if let Some(bytes) = c.get("Size").and_then(|v| v.trim().parse::<u64>().ok()) {
        row("Download size", &human::size(bytes));
    }
    for (label, field) in [("Depends (Debian)", "Depends"), ("Recommends", "Recommends"), ("Suggests", "Suggests")] {
        if let Some(v) = c.get(field) {
            row(label, &v.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }

    // What the dependencies become on Arch, as a build would decide.
    let paths = Paths::from_env()?;
    let tables = Tables::load(&paths.config)?;
    let arch = c.get("Architecture").unwrap_or("amd64").to_string();
    let system = LiveSystem;
    let mapped = deps::translate(&c, &arch, &tables.depmap, |n| system.installed(n), |n| system.in_repos(n))?;
    row("Depends (Arch)", &if mapped.depends.is_empty() { "nothing".to_string() } else { mapped.depends.join(" ") });
    if !mapped.optdepends.is_empty() {
        row("Optional (Arch)", &mapped.optdepends.iter().map(|(d, _)| d.as_str()).collect::<Vec<_>>().join(" "));
    }
    for u in &mapped.unmapped {
        println!("{} {u}", st.warn("No Arch name, left out:"));
    }

    if let Some(desc) = c.get("Description") {
        let mut lines = desc.lines().map(str::trim);
        let summary = lines.by_ref().find(|l| !l.is_empty()).unwrap_or_default();
        row("Description", summary);
        for l in lines {
            // In deb822, a line holding only "." is an empty line.
            println!("  {}", if l == "." { "" } else { l });
        }
    }
    Ok(())
}

fn show_app(name: &str, app: &App, config: &Config, state: &State, st: Style) {
    let row = |label: &str, value: &str| println!("{} {value}", st.bold(&format!("{label}:")));
    let pkg = app.pkgname.clone().unwrap_or_else(|| name.to_string());
    row("Tracked as", &format!("{name} (pacman package {pkg})"));
    let source = match &app.source {
        SourceConfig::Apt { repository, package } => format!("apt repository {repository}, package {}", package.as_deref().unwrap_or(name)),
        SourceConfig::Direct { url, feed, .. } => match (feed, url) {
            (Some(f), _) => format!("download, version feed {f}"),
            (None, Some(u)) => format!("download {u}"),
            _ => "download".into(),
        },
        SourceConfig::Github { repo, asset, .. } => format!("GitHub releases of {repo} ({asset})"),
        SourceConfig::Manual {} => "by hand ('pacdeb upgrade <app> --file <deb>')".into(),
    };
    row("Source", &source);
    if app.source.uses_channel() {
        row("Channel", &config.channel(app).unwrap_or_else(|| "not set".into()));
    }
    let s = state.apps.get(name);
    row("Built", &s.and_then(|s| s.deb_version.clone()).unwrap_or_else(|| "nothing yet".into()));
    row("Installed", &crate::apps::installed_version(&pkg).unwrap_or_else(|| "no".into()));
    if let Some(a) = s.and_then(|s| s.available.as_ref()) {
        let found = crate::apps::status(&Latest::from_available(a), s);
        let what = match found {
            crate::apps::Status::Newer { latest, .. } => format!("{latest} (newer)"),
            crate::apps::Status::UpToDate(v) => format!("{v} (up to date)"),
            crate::apps::Status::Changed(true) => "a changed download".into(),
            _ => "nothing new".into(),
        };
        row("Available", &format!("{what}, checked {}", release::relative(a.checked, release::now())));
    }
    let open = crate::apps::unaccepted(app, s).len();
    if open > 0 {
        row("Warnings", &format!("{open} not reviewed yet ('pacdeb accept {name}' once read)"));
    }
}
