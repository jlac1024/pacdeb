//! The local pacman repository. Every updater on the system (pacman -Syu, the CachyOS
//! updater, Shelly, AUR helpers) reads the repositories in pacman.conf, so publishing
//! builds to a repository is how pacdeb updates arrive with the rest. Packages and the
//! database are signed with a key only pacdeb uses, which pacman is told to trust once.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Context, Result, bail};
use crate::paths::Paths;
use crate::registry::{Config, RepoSettings, State};

pub const DEFAULT_DIR: &str = "/var/lib/pacdeb/repo";
const NAME: &str = "pacdeb";
const KEY_UID: &str = "pacdeb local repository <pacdeb@localhost>";
const PUBLIC_KEY: &str = "pacdeb.pub.asc";

/// pacdeb's own GnuPG home, holding the signing key.
fn gnupg_home(paths: &Paths) -> PathBuf {
    paths.config.join("gnupg")
}

fn gpg(home: &Path) -> Command {
    let mut c = Command::new("gpg");
    // The key has no passphrase; loopback keeps gpg from asking for one anyway.
    c.arg("--homedir").arg(home).args(["--batch", "--pinentry-mode", "loopback", "--passphrase", ""]);
    c
}

fn run(cmd: &mut Command, what: &str) -> Result<String> {
    let out = cmd.output().context(format!("cannot run {what}"))?;
    if !out.status.success() {
        bail!("{what} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The fingerprint of the signing key, made on first use.
fn signing_key(paths: &Paths) -> Result<String> {
    let home = gnupg_home(paths);
    fs::create_dir_all(&home).context(home.display())?;
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).context(home.display())?;
    let list = |home: &Path| run(gpg(home).args(["--with-colons", "--list-secret-keys", KEY_UID]), "gpg");
    if let Ok(text) = list(&home) {
        if let Some(fpr) = first_fingerprint(&text) {
            return Ok(fpr);
        }
    }
    run(gpg(&home).args(["--quick-gen-key", KEY_UID, "ed25519", "sign", "never"]), "gpg (making the signing key)")?;
    first_fingerprint(&list(&home)?).ok_or_else(|| crate::error::Error::new("gpg made a key but does not list it"))
}

fn first_fingerprint(colons: &str) -> Option<String> {
    colons.lines().find_map(|l| l.strip_prefix("fpr:").map(|rest| rest.trim_matches(':').to_string()))
}

fn sign(paths: &Paths, key: &str, file: &Path) -> Result<()> {
    let sig = sig_path(file);
    // repo-add keeps its backup signature as a hard link to this one; writing in place
    // would change the backup too.
    let _ = fs::remove_file(&sig);
    run(gpg(&gnupg_home(paths)).args(["--yes", "--no-armor", "--local-user", key, "--detach-sign", "--output"]).arg(&sig).arg(file), "gpg (signing)")?;
    Ok(())
}

fn sig_path(file: &Path) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(".sig");
    PathBuf::from(s)
}

/// An empty tar, zstd compressed: what pacman reads as a repository with no packages.
fn empty_db() -> Result<Vec<u8>> {
    zstd::encode_all(&[0u8; 1024][..], 19).context("compressing the empty database")
}

/// Points `link` at `target` (a name in the same folder), replacing what was there.
fn relink(dir: &Path, link: &str, target: &str) -> Result<()> {
    let path = dir.join(link);
    let _ = fs::remove_file(&path);
    symlink(target, &path).context(path.display())
}

/// Signs the databases and points the .sig names pacman asks for at the signatures.
fn sign_databases(paths: &Paths, repo: &RepoSettings) -> Result<()> {
    let dir = Path::new(&repo.dir);
    for kind in ["db", "files"] {
        let file = format!("{}.{kind}.tar.zst", repo.name);
        sign(paths, &repo.key, &dir.join(&file))?;
        relink(dir, &format!("{}.{kind}", repo.name), &file)?;
        relink(dir, &format!("{}.{kind}.sig", repo.name), &format!("{file}.sig"))?;
    }
    Ok(())
}

/// `pacdeb repo init [dir]`: gets the folder ready and says how to tell pacman.
pub fn init(dir: Option<&str>) -> Result<()> {
    let paths = Paths::from_env()?;
    let mut config = Config::load(&paths.config)?;
    let dir = dir.map(String::from).or_else(|| config.settings.repo.as_ref().map(|r| r.dir.clone())).unwrap_or(DEFAULT_DIR.into());
    let dir = std::path::absolute(&dir).context(&dir)?;
    if !dir.is_dir() {
        bail!(
            "{} does not exist. Create it, owned by you, with:\n  sudo install -d -o \"$USER\" -m 755 {}\nthen run 'pacdeb repo init {}' again",
            dir.display(),
            dir.display(),
            dir.display()
        );
    }
    let probe = dir.join(".pacdeb-write-test");
    if fs::write(&probe, "").is_err() {
        bail!("{} is not writable by you. Fix it with:\n  sudo chown \"$USER\" {}", dir.display(), dir.display());
    }
    let _ = fs::remove_file(&probe);
    let in_home = std::env::var_os("HOME").filter(|h| !h.is_empty()).is_some_and(|h| dir.starts_with(h));
    if in_home && std::env::var_os("PACDEB_HOME").is_none() {
        println!("warning: pacman downloads as the 'alpm' user, which usually cannot read your home folder; a folder like {DEFAULT_DIR} works better");
    }

    let key = signing_key(&paths)?;
    let repo = RepoSettings { dir: dir.display().to_string(), name: NAME.into(), key: key.clone() };
    let public = dir.join(PUBLIC_KEY);
    let armored = run(gpg(&gnupg_home(&paths)).args(["--armor", "--export", &key]), "gpg (exporting the key)")?;
    fs::write(&public, armored).context(public.display())?;
    for kind in ["db", "files"] {
        let file = dir.join(format!("{NAME}.{kind}.tar.zst"));
        if !file.exists() {
            fs::write(&file, empty_db()?).context(file.display())?;
        }
    }
    sign_databases(&paths, &repo)?;
    publish_existing(&config, &paths, &repo);
    config.settings.repo = Some(repo);
    config.save(&paths.config)?;

    println!("Repository ready in {} (signing key {key}).", dir.display());
    println!("To let pacman use it, once:");
    println!("  1. sudo pacman-key --add {}", public.display());
    println!("  2. sudo pacman-key --lsign-key {key}");
    println!("  3. Add this to the end of /etc/pacman.conf:\n\n{}", conf_section(&dir));
    println!("New builds are published there from now on. 'pacdeb repo status' shows what it holds.");
    Ok(())
}

/// Publishes each tracked app's latest build, so the repository starts out with what
/// is installed instead of waiting for the next versions.
fn publish_existing(config: &Config, paths: &Paths, repo: &RepoSettings) {
    let Ok(state) = State::load(&paths.state) else {
        return;
    };
    for (name, app) in &config.apps {
        let pkgname = app.pkgname.as_deref().unwrap_or(name);
        let latest = state.apps.get(name).and_then(|s| s.packages.last()).map(PathBuf::from);
        let Some(pkg) = latest.filter(|p| p.exists()) else {
            continue;
        };
        // Builds from before a rename carry the old name and stay out.
        if !pkg.file_name().and_then(|n| n.to_str()).is_some_and(|n| is_package_of(n, pkgname)) {
            continue;
        }
        let in_repo = pkg.file_name().map(|n| Path::new(&repo.dir).join(n));
        if in_repo.is_some_and(|p| p.exists() && sig_path(&p).exists()) {
            continue;
        }
        match publish(&pkg, paths, repo) {
            Ok(()) => println!("Published {}", pkg.file_name().unwrap_or_default().to_string_lossy()),
            Err(e) => println!("warning: could not publish {}: {e}", pkg.display()),
        }
    }
}

fn conf_section(dir: &Path) -> String {
    format!("[{NAME}]\nSigLevel = Required\nServer = file://{}\n", dir.display())
}

/// Adds a built package to the repository, replacing its older version there.
pub fn publish(pkg: &Path, paths: &Paths, repo: &RepoSettings) -> Result<()> {
    let dir = Path::new(&repo.dir);
    let Some(name) = pkg.file_name() else {
        bail!("{} is not a package file", pkg.display());
    };
    let dest = dir.join(name);
    fs::copy(pkg, &dest).context(format!("copying {} into {}", pkg.display(), dir.display()))?;
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o644)).context(dest.display())?;
    sign(paths, &repo.key, &dest)?;
    let db = dir.join(format!("{}.db.tar.zst", repo.name));
    run(Command::new("repo-add").args(["--quiet", "--nocolor", "--remove"]).arg(&db).arg(&dest), "repo-add")?;
    sign_databases(paths, repo)?;
    Ok(())
}

/// Takes a package out of the repository and deletes its files there.
pub fn unpublish(pkgname: &str, paths: &Paths, repo: &RepoSettings) -> Result<bool> {
    let dir = Path::new(&repo.dir);
    let files: Vec<PathBuf> = fs::read_dir(dir)
        .context(dir.display())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| is_package_of(n, pkgname)))
        .collect();
    if files.is_empty() {
        return Ok(false);
    }
    let db = dir.join(format!("{}.db.tar.zst", repo.name));
    run(Command::new("repo-remove").args(["--quiet", "--nocolor"]).arg(&db).arg(pkgname), "repo-remove")?;
    sign_databases(paths, repo)?;
    for f in files {
        let _ = fs::remove_file(f);
    }
    Ok(true)
}

/// Whether a file in the repository is a package (or its signature) named `pkgname`:
/// the name, a dash, then a version starting with a digit.
fn is_package_of(file: &str, pkgname: &str) -> bool {
    let is_pkg = file.ends_with(".pkg.tar.zst") || file.ends_with(".pkg.tar.zst.sig");
    is_pkg && file.strip_prefix(pkgname).and_then(|r| r.strip_prefix('-')).is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
}

/// `pacdeb repo remove`: stop publishing. The folder and pacman's settings are left
/// for the person (or setup.sh --remove) to take away.
pub fn forget() -> Result<()> {
    let paths = Paths::from_env()?;
    let mut config = Config::load(&paths.config)?;
    let Some(repo) = config.settings.repo.take() else {
        println!("No repository set up.");
        return Ok(());
    };
    config.save(&paths.config)?;
    println!("pacdeb no longer publishes to {}. The folder and its packages are left as they are.", repo.dir);
    Ok(())
}

/// `pacdeb repo status`.
pub fn status() -> Result<()> {
    let paths = Paths::from_env()?;
    let config = Config::load(&paths.config)?;
    let Some(repo) = &config.settings.repo else {
        println!("No repository set up. 'pacdeb repo init' sets one up in {DEFAULT_DIR}.");
        return Ok(());
    };
    let dir = Path::new(&repo.dir);
    println!("Repository: [{}] in {}", repo.name, dir.display());
    println!("Signing key: {}", repo.key);
    let mut pkgs: Vec<String> = fs::read_dir(dir)
        .context(dir.display())?
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| n.ends_with(".pkg.tar.zst"))
        .collect();
    pkgs.sort();
    match pkgs.len() {
        0 => println!("Packages: none yet"),
        _ => println!("Packages:\n  {}", pkgs.join("\n  ")),
    }
    let conf = fs::read_to_string("/etc/pacman.conf").unwrap_or_default();
    if conf.lines().any(|l| l.trim() == format!("[{}]", repo.name)) {
        println!("pacman.conf has the [{}] section.", repo.name);
    } else {
        println!("pacman.conf does not list it yet; add this to the end of /etc/pacman.conf:\n\n{}", conf_section(dir));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_package_files() {
        let cases = [
            ("app-deb-1.0-1-x86_64.pkg.tar.zst", "app-deb", true),
            ("app-deb-1.0-1-x86_64.pkg.tar.zst.sig", "app-deb", true),
            ("app-deb-1.0-1-x86_64.pkg.tar.zst", "app", false),
            ("app-2:1.0-1-x86_64.pkg.tar.zst", "app", true),
            ("app.db.tar.zst", "app", false),
            ("app-deb-1.0-1-x86_64.pkg.tar.zst.part", "app-deb", false),
        ];
        for (file, name, want) in cases {
            assert_eq!(is_package_of(file, name), want, "{file} {name}");
        }
    }

    #[test]
    fn reads_fingerprints() {
        let colons = "sec:u:255:22:ABCD:1:::u:::scESC:::+:::ed25519:::0:\nfpr:::::::::0123456789ABCDEF0123456789ABCDEF01234567:\ngrp:::::::::X:\n";
        assert_eq!(first_fingerprint(colons).as_deref(), Some("0123456789ABCDEF0123456789ABCDEF01234567"));
        assert_eq!(first_fingerprint(""), None);
    }

    #[test]
    fn empty_database_is_an_empty_tar() {
        let raw = zstd::decode_all(&empty_db().unwrap()[..]).unwrap();
        let mut ar = tar::Archive::new(&raw[..]);
        assert_eq!(ar.entries().unwrap().count(), 0);
    }
}
