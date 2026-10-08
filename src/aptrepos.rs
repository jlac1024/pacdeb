//! Saved apt repositories (`[apt.<name>]` in apps.toml) and `pacdeb apt`: adding them
//! from a vendor's apt line, showing their health, editing them, replacing their
//! signing key, browsing and removing them. Changes are checked against the live
//! repository before they are saved.

use std::fs;
use std::path::{Path, PathBuf};

use crate::aptline::{self, SignedBy};
use crate::error::{Context, Error, Result, bail};
use crate::human::plural;
use crate::net;
use crate::paths::Paths;
use crate::registry::{AptRepoConfig, Config};
use crate::sources::apt::{self, Listed};
use crate::sources::apt_fetch;
use crate::sources::gpg::{self, KeyInfo};
use crate::sources::release::{self, ReleaseInfo};
use crate::style::Style;

/// Where a signing key comes from.
#[derive(Debug, Default, Clone)]
pub struct KeySource {
    pub file: Option<String>,
    pub url: Option<String>,
    /// Key text pasted into a .sources file.
    pub inline: Option<String>,
    /// The fingerprint the key must have.
    pub fingerprint: Option<String>,
}

impl KeySource {
    fn is_empty(&self) -> bool {
        self.file.is_none() && self.url.is_none() && self.inline.is_none()
    }
}

fn normalize_fpr(f: &str) -> String {
    f.replace(' ', "").to_ascii_uppercase()
}

/// Fetches a key into a scratch file and checks it against the pinned fingerprint.
fn fetch_key(repo: &str, src: &KeySource, paths: &Paths) -> Result<(PathBuf, Vec<String>)> {
    let (bytes, label) = if let Some(text) = &src.inline {
        (text.clone().into_bytes(), "the key in the .sources text".to_string())
    } else if let Some(file) = &src.file {
        (fs::read(file).context(file)?, file.clone())
    } else if let Some(url) = &src.url {
        println!("Downloading the signing key from {url}");
        (net::get_bytes(url, &[])?, url.clone())
    } else {
        bail!("no signing key given: use --key-url <url> (and --key-fingerprint <fpr> to pin it) or --key <file>");
    };
    let tmp = paths.cache.join("keys").join(format!("apt-{repo}.new.asc"));
    fs::create_dir_all(tmp.parent().unwrap()).context(paths.cache.display())?;
    fs::write(&tmp, bytes).context(tmp.display())?;
    let fprs = gpg::fingerprints(&tmp, &paths.cache.join("gnupg").join("check"))?;
    if fprs.is_empty() {
        let _ = fs::remove_file(&tmp);
        bail!("{label} holds no public key");
    }
    if let Some(want) = src.fingerprint.as_deref().map(normalize_fpr).filter(|f| !f.is_empty()) {
        if !fprs.iter().any(|f| f.eq_ignore_ascii_case(&want)) {
            let _ = fs::remove_file(&tmp);
            bail!("the key from {label} has fingerprint {}, not the pinned {want}; not using it", fprs.join(", "));
        }
    }
    Ok((tmp, fprs))
}

/// Moves a fetched key into the config dir as the repository's key.
fn commit_key(repo: &str, tmp: &Path, paths: &Paths) -> Result<String> {
    let rel = format!("keys/apt-{repo}.asc");
    let dest = paths.config.join(&rel);
    fs::create_dir_all(dest.parent().unwrap()).context(paths.config.display())?;
    fs::copy(tmp, &dest).context(dest.display())?;
    let _ = fs::remove_file(tmp);
    Ok(rel)
}

fn say_fingerprint(fprs: &[String], pinned: bool) {
    if pinned {
        println!("Signing key checked: fingerprint {} matches", fprs.join(", "));
    } else {
        println!("Signing key fingerprint {}. Compare it with the one the vendor publishes, or pin it with --key-fingerprint.", fprs.join(", "));
    }
}

fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty() && !name.starts_with(['-', '.']) && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._+-".contains(c));
    if !ok {
        bail!("'{name}' is not a usable repository name; use lowercase letters, digits and ._+-");
    }
    Ok(())
}

/// Saves `repo` as `name`, unless a saved repository already points at the same place,
/// which is then used instead (its components grow to include the new ones). A new
/// repository's key is fetched and its Release checked before anything is saved.
/// Returns the name in use.
pub fn save_or_reuse(config: &mut Config, name: &str, mut repo: AptRepoConfig, key: &KeySource, paths: &Paths) -> Result<String> {
    if let Some((existing, saved)) = config.apt.iter_mut().find(|(_, r)| r.same_place(&repo)) {
        for c in &repo.components {
            if !saved.components.contains(c) {
                saved.components.push(c.clone());
            }
        }
        println!("Using the saved apt repository {existing}");
        return Ok(existing.clone());
    }
    check_name(name)?;
    if config.apt.contains_key(name) {
        bail!("an apt repository named {name} already exists for another place; pick another name");
    }
    let (tmp, fprs) = fetch_key(name, key, paths)?;
    let verified = apt_fetch::fetch_release(name, &repo, &tmp, &paths.cache);
    if let Err(e) = verified {
        let _ = fs::remove_file(&tmp);
        return Err(Error::new(format!("{e}\nNothing was saved.")));
    }
    say_fingerprint(&fprs, key.fingerprint.is_some());
    repo.key = Some(commit_key(name, &tmp, paths)?);
    repo.key_url = key.url.clone().or(repo.key_url);
    repo.key_fingerprint = key.fingerprint.as_deref().map(normalize_fpr).or(repo.key_fingerprint);
    config.apt.insert(name.to_string(), repo);
    Ok(name.to_string())
}

/// What is known about a repository's health.
#[derive(Debug, Default)]
pub struct Health {
    pub info: Option<ReleaseInfo>,
    /// When the Release file was last read successfully.
    pub checked: Option<i64>,
    pub keys: Vec<KeyInfo>,
    /// Why reading the repository failed, when checked live.
    pub error: Option<String>,
    pub warnings: Vec<String>,
}

/// Looks at a repository: live (fetching its Release now) or from the last check.
pub fn health(name: &str, repo: &AptRepoConfig, paths: &Paths, live: bool) -> Health {
    let mut h = Health::default();
    let key = repo.key.as_ref().map(|k| paths.config.join(k));
    match &key {
        Some(k) => match gpg::key_info(k, &paths.cache.join("gnupg").join("check")) {
            Ok(keys) => h.keys = keys,
            Err(e) => h.warnings.push(format!("cannot read the signing key: {e}")),
        },
        None => h.warnings.push(format!("no signing key; add one with 'pacdeb apt key {name} --key-url <url>'")),
    }
    if live {
        if let Some(k) = &key {
            match apt_fetch::fetch_release(name, repo, k, &paths.cache) {
                Ok(_) => {}
                Err(e) => h.error = Some(e.to_string()),
            }
        }
    }
    if let Some((when, info)) = apt_fetch::last_checked(name, &paths.cache) {
        h.checked = Some(when);
        h.info = Some(info);
    }
    h.warnings.extend(warnings(repo, &h, release::now()));
    h
}

fn warnings(repo: &AptRepoConfig, h: &Health, now: i64) -> Vec<String> {
    let mut w = Vec::new();
    let day = 86_400;
    for k in &h.keys {
        match k.expires {
            Some(t) if t < now => w.push(format!("the signing key {} expired {}; fetch the vendor's new key with 'pacdeb apt key'", short(&k.fingerprint), release::relative(t, now))),
            Some(t) if t < now + 30 * day => w.push(format!("the signing key {} expires {}", short(&k.fingerprint), release::relative(t, now))),
            _ => {}
        }
    }
    match (&h.info, h.checked) {
        (None, _) => w.push("never checked yet".into()),
        (Some(info), _) => {
            if let Some(t) = info.date.as_deref().and_then(release::parse_date).filter(|t| *t < now - 365 * day) {
                w.push(format!("the repository was last updated {}", release::relative(t, now)));
            }
            if let Some(t) = info.valid_until.as_deref().and_then(release::parse_date).filter(|t| *t < now + 2 * day) {
                w.push(format!("its Release file is only valid until {} ({})", info.valid_until.as_deref().unwrap_or(""), release::relative(t, now)));
            }
            if !repo.is_flat() && !info.components.is_empty() {
                // Release lists components by their last path part (main, not stable/main).
                let offered: Vec<&str> = info.components.iter().map(|c| c.rsplit('/').next().unwrap_or(c)).collect();
                for c in &repo.components {
                    if !offered.contains(&c.as_str()) {
                        w.push(format!("component {c} is not offered (it has {})", info.components.join(", ")));
                    }
                }
            }
            let arch = apt_fetch::arch(repo);
            if !info.architectures.is_empty() && !info.architectures.iter().any(|a| *a == arch || a == "all") {
                w.push(format!("architecture {arch} is not offered (it has {})", info.architectures.join(", ")));
            }
        }
    }
    w
}

fn short(fpr: &str) -> &str {
    &fpr[fpr.len().saturating_sub(16)..]
}

/// Every package a saved repository offers, across its components.
pub fn packages(name: &str, repo: &AptRepoConfig, paths: &Paths) -> Result<Vec<Listed>> {
    let Some(key) = &repo.key else {
        bail!("apt repository {name} has no signing key; add one with 'pacdeb apt key {name} --key-url <url>'");
    };
    let fetched = apt_fetch::fetch(name, repo, &paths.config.join(key), &paths.cache)?;
    let arch = apt_fetch::arch(repo);
    let mut all: Vec<Listed> = Vec::new();
    for (_, index) in &fetched.indexes {
        for l in apt::list(index, &arch)? {
            match all.iter_mut().find(|a| a.name == l.name) {
                Some(a) => {
                    if matches!((crate::version::DebVersion::parse(&l.version), crate::version::DebVersion::parse(&a.version)), (Ok(n), Ok(o)) if n > o) {
                        *a = l;
                    }
                }
                None => all.push(l),
            }
        }
    }
    all.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(all)
}

/// Options of the `pacdeb apt` commands.
#[derive(Debug, Default)]
struct Opts {
    positional: Vec<String>,
    line: Option<String>,
    file: Option<String>,
    url: Option<String>,
    suite: Option<String>,
    components: Option<Vec<String>>,
    arch: Option<String>,
    key: KeySource,
    with_apps: bool,
    offline: bool,
}

fn parse_opts(args: &[String]) -> Result<Opts> {
    let mut o = Opts::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if !a.starts_with("--") {
            o.positional.push(a.clone());
            continue;
        }
        match a.as_str() {
            "--with-apps" => o.with_apps = true,
            "--offline" => o.offline = true,
            flag => {
                let v = it.next().ok_or_else(|| Error::new(format!("{flag} needs a value")))?.clone();
                match flag {
                    "--line" => o.line = Some(v),
                    "--file" => o.file = Some(v),
                    "--url" => o.url = Some(v),
                    "--suite" => o.suite = Some(v),
                    "--components" => o.components = Some(v.split([',', ' ']).filter(|s| !s.is_empty()).map(String::from).collect()),
                    "--arch" => o.arch = Some(v),
                    "--key" => o.key.file = Some(v),
                    "--key-url" => o.key.url = Some(v),
                    "--key-fingerprint" => o.key.fingerprint = Some(v),
                    other => bail!("unknown option '{other}'"),
                }
            }
        }
    }
    Ok(o)
}

pub fn run(args: &[String]) -> Result<()> {
    let Some((cmd, rest)) = args.split_first() else {
        return list();
    };
    let o = parse_opts(rest)?;
    let one = |what: &str| -> Result<String> {
        match o.positional.as_slice() {
            [n] => Ok(n.clone()),
            _ => bail!("usage: pacdeb apt {what} <repository>"),
        }
    };
    match cmd.as_str() {
        "list" => list(),
        "add" => add(&o),
        "show" => show(&one("show")?, o.offline),
        "edit" => edit(&one("edit")?, &o),
        "key" => key(&one("key")?, &o),
        "remove" => remove(&one("remove")?, o.with_apps),
        "packages" => print_packages(&one("packages")?),
        other => bail!("unknown apt command '{other}'; use list, add, show, edit, key, remove or packages"),
    }
}

fn list() -> Result<()> {
    let paths = Paths::from_env()?;
    let config = Config::load(&paths.config)?;
    if config.apt.is_empty() {
        println!("No apt repositories saved. Add one with 'pacdeb apt add <name> --line \"deb https://... stable main\" --key-url <url>'.");
        return Ok(());
    }
    let st = Style::for_stdout();
    let now = release::now();
    for (name, repo) in &config.apt {
        let h = health(name, repo, &paths, false);
        let apps = config.apps_using(name);
        let comps = if repo.is_flat() { "flat".to_string() } else { repo.components.join(" ") };
        println!("{}  {} {} {}", st.bold(name), repo.url, repo.suite, comps);
        let used = if apps.is_empty() { "no apps".to_string() } else { apps.join(", ") };
        let checked = h.checked.map(|t| format!("checked {}", release::relative(t, now))).unwrap_or_else(|| "not checked yet".into());
        println!("  {used} · {checked}");
        for w in &h.warnings {
            println!("  {} {w}", st.warn("warning:"));
        }
    }
    Ok(())
}

fn add(o: &Opts) -> Result<()> {
    let paths = Paths::from_env()?;
    let mut config = Config::load(&paths.config)?;
    let text = match (&o.line, &o.file) {
        (Some(l), _) => Some(l.clone()),
        (None, Some(f)) => Some(fs::read_to_string(f).context(f)?),
        (None, None) => None,
    };
    let (name, described) = match (text, o.positional.as_slice()) {
        (Some(t), [n]) => (Some(n.clone()), aptline::parse(&t)?),
        (Some(t), []) => (None, aptline::parse(&t)?),
        (None, [n, url, suite, comps @ ..]) => {
            let line = format!("deb {url} {suite} {}", comps.join(" "));
            (Some(n.clone()), aptline::parse(&line)?)
        }
        _ => bail!("usage: pacdeb apt add <name> <url> <suite> <components...>, or pacdeb apt add [name] --line '<deb line>' / --file <file.list|file.sources>"),
    };
    if described.len() > 1 && name.is_some() {
        bail!("that text describes {} repositories; add them without a name to have names picked, or one at a time", described.len());
    }
    for d in described {
        let repo_name = name.clone().unwrap_or_else(|| {
            let base = aptline::suggest_name(&d.url);
            let mut n = base.clone();
            let mut i = 2;
            while config.apt.contains_key(&n) {
                n = format!("{base}-{i}");
                i += 1;
            }
            n
        });
        let mut key = o.key.clone();
        match &d.signed_by {
            Some(SignedBy::Inline(k)) if key.is_empty() => key.inline = Some(k.clone()),
            Some(SignedBy::Path(p)) if key.is_empty() && Path::new(p).is_file() => key.file = Some(p.clone()),
            Some(SignedBy::Path(p)) if key.is_empty() => bail!("the apt line names the key file {p}, which is not on this system; give the key's download link with --key-url"),
            _ => {}
        }
        let components = o.components.clone().unwrap_or(d.components);
        let repo = AptRepoConfig { url: d.url.trim_end_matches('/').to_string(), suite: d.suite, components, arch: o.arch.clone().or(d.arch), key: None, key_url: None, key_fingerprint: None };
        let used = save_or_reuse(&mut config, &repo_name, repo, &key, &paths)?;
        config.save(&paths.config)?;
        if used == repo_name {
            println!("Saved apt repository {used}. See what it offers with 'pacdeb apt packages {used}'.");
        }
    }
    Ok(())
}

fn date_line(label: &str, value: Option<&str>, now: i64) -> Option<String> {
    let v = value?;
    let rel = release::parse_date(v).map(|t| format!(" ({})", release::relative(t, now))).unwrap_or_default();
    Some(format!("  {label:<13}{v}{rel}"))
}

fn show(name: &str, offline: bool) -> Result<()> {
    let paths = Paths::from_env()?;
    let config = Config::load(&paths.config)?;
    let Some(repo) = config.apt.get(name) else {
        bail!("no apt repository named {name}; 'pacdeb apt list' shows the saved ones");
    };
    let st = Style::for_stdout();
    let now = release::now();
    let h = health(name, repo, &paths, !offline);
    println!("{}", st.bold(name));
    let row = |label: &str, value: String| println!("  {label:<13}{value}");
    row("URL", repo.url.clone());
    row("Suite", repo.suite.clone());
    row("Components", if repo.is_flat() { "(flat repository)".into() } else { repo.components.join(" ") });
    row("Architecture", apt_fetch::arch(repo));
    let apps = config.apps_using(name);
    row("Apps", if apps.is_empty() { "none".into() } else { apps.join(", ") });
    if let Some(info) = &h.info {
        let who: Vec<&str> = [info.origin.as_deref(), info.label.as_deref()].into_iter().flatten().collect();
        if !who.is_empty() {
            row("Published by", who.join(" / "));
        }
        for l in [date_line("Updated", info.date.as_deref(), now), date_line("Valid until", info.valid_until.as_deref(), now)].into_iter().flatten() {
            println!("{l}");
        }
        if !info.components.is_empty() {
            row("Offers", format!("{} for {}", info.components.join(" "), info.architectures.join(" ")));
        }
    }
    for k in &h.keys {
        let expiry = match k.expires {
            Some(t) => format!("expires {}", release::relative(t, now)),
            None => "does not expire".into(),
        };
        let pinned = repo.key_fingerprint.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(&k.fingerprint));
        row("Signing key", format!("{} ({}){}", k.fingerprint, expiry, if pinned { ", pinned" } else { "" }));
        if let Some(uid) = &k.uid {
            row("", uid.clone());
        }
    }
    if let Some(u) = &repo.key_url {
        row("Key from", u.clone());
    }
    row("Checked", h.checked.map(|t| release::relative(t, now)).unwrap_or_else(|| "never".into()));
    if let Some(e) = &h.error {
        println!("  {} {e}", st.bad("error:"));
    }
    for w in &h.warnings {
        println!("  {} {w}", st.warn("warning:"));
    }
    if h.error.is_some() {
        bail!("{name} could not be read");
    }
    Ok(())
}

fn edit(name: &str, o: &Opts) -> Result<()> {
    let paths = Paths::from_env()?;
    let mut config = Config::load(&paths.config)?;
    let Some(old) = config.apt.get(name).cloned() else {
        bail!("no apt repository named {name}; 'pacdeb apt list' shows the saved ones");
    };
    if !o.key.is_empty() || o.key.fingerprint.is_some() {
        bail!("change the signing key with 'pacdeb apt key {name} ...'");
    }
    let mut repo = old.clone();
    if let Some(u) = &o.url {
        repo.url = u.trim_end_matches('/').to_string();
    }
    if let Some(s) = &o.suite {
        repo.suite = s.clone();
    }
    if let Some(c) = &o.components {
        repo.components = c.clone();
    }
    if let Some(a) = &o.arch {
        repo.arch = if a.is_empty() { None } else { Some(a.clone()) };
    }
    if repo == old {
        bail!("nothing to change; give --url, --suite, --components or --arch");
    }
    let Some(key) = &repo.key else {
        bail!("{name} has no signing key; add one first with 'pacdeb apt key {name} --key-url <url>'");
    };
    // Check the whole new setup, including every component's index, before saving.
    if let Err(e) = apt_fetch::fetch(&format!("{name}.new"), &repo, &paths.config.join(key), &paths.cache) {
        apt_fetch::forget(&format!("{name}.new"), &paths.cache);
        bail!("the changed repository does not work, so nothing was saved: {e}");
    }
    apt_fetch::forget(&format!("{name}.new"), &paths.cache);
    apt_fetch::forget(name, &paths.cache);
    config.apt.insert(name.to_string(), repo);
    config.save(&paths.config)?;
    let apps = config.apps_using(name);
    println!("Updated apt repository {name}{}", if apps.is_empty() { String::new() } else { format!(" (used by {})", apps.join(", ")) });
    Ok(())
}

fn key(name: &str, o: &Opts) -> Result<()> {
    let paths = Paths::from_env()?;
    let mut config = Config::load(&paths.config)?;
    let Some(mut repo) = config.apt.get(name).cloned() else {
        bail!("no apt repository named {name}; 'pacdeb apt list' shows the saved ones");
    };
    let mut src = o.key.clone();
    if src.is_empty() {
        src.url = repo.key_url.clone();
        if src.url.is_none() {
            bail!("{name} has no key link saved; give one with --key-url <url> or a file with --key <file>");
        }
    }
    // A pinned fingerprint holds until a new one is given on purpose.
    if src.fingerprint.is_none() {
        src.fingerprint = repo.key_fingerprint.clone();
    }
    let old: Vec<String> = repo.key.as_ref().and_then(|k| gpg::fingerprints(&paths.config.join(k), &paths.cache.join("gnupg").join("check")).ok()).unwrap_or_default();
    let (tmp, fprs) = match fetch_key(name, &src, &paths) {
        Ok(k) => k,
        Err(e) if repo.key_fingerprint.is_some() && o.key.fingerprint.is_none() => {
            bail!("{e}\nIf the vendor really changed its key, pass the new fingerprint with --key-fingerprint.")
        }
        Err(e) => return Err(e),
    };
    if let Err(e) = apt_fetch::fetch_release(name, &repo, &tmp, &paths.cache) {
        let _ = fs::remove_file(&tmp);
        bail!("the repository is not signed by that key, so it was not changed: {e}");
    }
    repo.key = Some(commit_key(name, &tmp, &paths)?);
    if let Some(u) = &o.key.url {
        repo.key_url = Some(u.clone());
    }
    if let Some(f) = &o.key.fingerprint {
        repo.key_fingerprint = if f.is_empty() { None } else { Some(normalize_fpr(f)) };
    }
    config.apt.insert(name.to_string(), repo.clone());
    config.save(&paths.config)?;
    if old == fprs {
        println!("The key is unchanged: {}", fprs.join(", "));
    } else {
        let before = if old.is_empty() { "none".to_string() } else { old.join(", ") };
        println!("Signing key replaced: {before} -> {}", fprs.join(", "));
    }
    say_fingerprint(&fprs, repo.key_fingerprint.is_some());
    Ok(())
}

fn remove(name: &str, with_apps: bool) -> Result<()> {
    let paths = Paths::from_env()?;
    let config = Config::load(&paths.config)?;
    let Some(repo) = config.apt.get(name).cloned() else {
        bail!("no apt repository named {name}; 'pacdeb apt list' shows the saved ones");
    };
    let apps: Vec<String> = config.apps_using(name).into_iter().map(String::from).collect();
    if !apps.is_empty() && !with_apps {
        let them = if apps.len() == 1 { "it" } else { "them" };
        bail!("{} from {name}: {}. Remove {them} too with --with-apps, or move {them} to another repository first", plural(apps.len(), "app comes", "apps come"), apps.join(", "));
    }
    for a in &apps {
        crate::apps::remove(a)?;
    }
    let mut config = Config::load(&paths.config)?;
    config.apt.remove(name);
    config.save(&paths.config)?;
    if let Some(k) = &repo.key {
        let used_elsewhere = config.apt.values().any(|r| r.key.as_deref() == Some(k.as_str()));
        if !used_elsewhere {
            let _ = fs::remove_file(paths.config.join(k));
        }
    }
    apt_fetch::forget(name, &paths.cache);
    println!("Removed apt repository {name}");
    Ok(())
}

fn print_packages(name: &str) -> Result<()> {
    let paths = Paths::from_env()?;
    let config = Config::load(&paths.config)?;
    let Some(repo) = config.apt.get(name) else {
        bail!("no apt repository named {name}; 'pacdeb apt list' shows the saved ones");
    };
    let list = packages(name, repo, &paths)?;
    let tracked: Vec<String> = config
        .apps
        .iter()
        .filter_map(|(n, a)| match &a.source {
            crate::registry::SourceConfig::Apt { repository, package } if repository == name => Some(package.clone().unwrap_or_else(|| n.clone())),
            _ => None,
        })
        .collect();
    println!("{} ({}): {}", name, repo.url, plural(list.len(), "package", "packages"));
    let width = list.iter().map(|l| l.name.len()).max().unwrap_or(0);
    let vwidth = list.iter().map(|l| l.version.len()).max().unwrap_or(0);
    for l in &list {
        let mark = if tracked.contains(&l.name) { "*" } else { " " };
        println!("{mark} {:<width$}  {:<vwidth$}  {}", l.name, l.version, l.summary);
    }
    println!("* tracked. Track another with: pacdeb add <package> --apt {name}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> AptRepoConfig {
        AptRepoConfig { url: "https://x.example/apt".into(), suite: "stable".into(), components: vec!["main".into(), "beta".into()], arch: Some("amd64".into()), key: None, key_url: None, key_fingerprint: None }
    }

    #[test]
    fn warns_about_repository_health() {
        let now = 2_000_000_000;
        let day = 86_400;
        let info = ReleaseInfo {
            date: Some("Thu, 01 Jan 2026 00:00:00 UTC".into()),
            valid_until: None,
            components: vec!["stable/main".into()],
            architectures: vec!["arm64".into()],
            ..Default::default()
        };
        let h = Health {
            info: Some(info),
            checked: Some(now),
            keys: vec![
                KeyInfo { fingerprint: "A".repeat(40), uid: None, created: None, expires: Some(now - day) },
                KeyInfo { fingerprint: "B".repeat(40), uid: None, created: None, expires: Some(now + 10 * day) },
                KeyInfo { fingerprint: "C".repeat(40), uid: None, created: None, expires: None },
            ],
            ..Default::default()
        };
        let w = warnings(&repo(), &h, now);
        let has = |s: &str| w.iter().any(|x| x.contains(s));
        assert!(has("AAAAAAAAAAAAAAAA expired 24 hours ago"), "{w:?}");
        assert!(has("expires in 10 days"), "{w:?}");
        assert!(!has("CCCCCCCC"), "{w:?}");
        assert!(has("last updated"), "{w:?}");
        assert!(has("component beta is not offered"), "{w:?}");
        assert!(!has("component main"), "{w:?}");
        assert!(has("architecture amd64 is not offered"), "{w:?}");

        let fresh = Health { info: None, ..Default::default() };
        assert_eq!(warnings(&repo(), &fresh, now), ["never checked yet"]);
    }

    #[test]
    fn reads_apt_command_options() {
        let args: Vec<String> = ["example", "--components", "main,beta", "--key-url", "https://x/k.asc", "--with-apps"].iter().map(|s| s.to_string()).collect();
        let o = parse_opts(&args).unwrap();
        assert_eq!(o.positional, ["example"]);
        assert_eq!(o.components.as_deref(), Some(&["main".to_string(), "beta".to_string()][..]));
        assert_eq!(o.key.url.as_deref(), Some("https://x/k.asc"));
        assert!(o.with_apps);
        assert!(parse_opts(&["--nope".to_string(), "x".to_string()]).is_err());
    }
}
