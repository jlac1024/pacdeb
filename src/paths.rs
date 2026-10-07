//! Where pacdeb keeps its config, state and cache.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Context, Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
}

impl Paths {
    /// Paths are made absolute, since makepkg runs in another directory.
    pub fn from_env() -> Result<Paths> {
        let p = Paths::from_vars(|k| std::env::var_os(k))?;
        let abs = |p: PathBuf| std::path::absolute(&p).context(p.display());
        let p = Paths { config: abs(p.config)?, state: abs(p.state)?, cache: abs(p.cache)? };
        if std::env::var_os("PACDEB_HOME").is_none_or(|v| v.is_empty()) {
            migrate_from_ferry(&p)?;
        }
        Ok(p)
    }

    /// Where makepkg work directories live.
    pub fn work_dir(&self) -> PathBuf {
        self.cache.join("build")
    }

    /// Where finished packages go unless --out says otherwise.
    pub fn packages_dir(&self) -> PathBuf {
        self.cache.join("packages")
    }

    fn from_vars(get: impl Fn(&str) -> Option<OsString>) -> Result<Paths> {
        let var = |k: &str| get(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        if let Some(home) = var("PACDEB_HOME") {
            return Ok(Paths {
                config: home.join("config"),
                state: home.join("state"),
                cache: home.join("cache"),
            });
        }
        let Some(home) = var("HOME") else {
            bail!("HOME is not set, so pacdeb cannot find its config. Set HOME or PACDEB_HOME.");
        };
        // XDG says relative values are invalid and should be ignored.
        let xdg = |k: &str, fallback: &str| {
            var(k).filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(fallback)).join("pacdeb")
        };
        Ok(Paths {
            config: xdg("XDG_CONFIG_HOME", ".config"),
            state: xdg("XDG_DATA_HOME", ".local/share"),
            cache: xdg("XDG_CACHE_HOME", ".cache"),
        })
    }
}

/// pacdeb was called Ferry while it was built. The first run moves the old folders to
/// the new names, and points the build paths recorded in state.toml at the new cache.
/// Nothing happens once the new folders exist.
fn migrate_from_ferry(p: &Paths) -> Result<()> {
    let old = |new: &Path| new.with_file_name("ferry");
    let mut moved = Vec::new();
    for new in [&p.config, &p.state, &p.cache] {
        let legacy = old(new);
        if !new.exists() && legacy.is_dir() {
            fs::rename(&legacy, new).context(format!("moving {} to {}", legacy.display(), new.display()))?;
            moved.push(format!("{} -> {}", legacy.display(), new.display()));
        }
    }
    if moved.is_empty() {
        return Ok(());
    }
    let state = p.state.join("state.toml");
    if let Ok(text) = fs::read_to_string(&state) {
        let (from, to) = (old(&p.cache).display().to_string(), p.cache.display().to_string());
        let fixed = text.replace(&format!("{from}/"), &format!("{to}/"));
        if fixed != text {
            fs::write(&state, fixed).context(state.display())?;
        }
    }
    eprintln!("pacdeb (formerly Ferry) moved its folders: {}", moved.join(", "));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(vars: &[(&str, &str)]) -> Result<Paths> {
        Paths::from_vars(|k| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v)))
    }

    #[test]
    fn picks_locations() {
        let cases: &[(&[(&str, &str)], [&str; 3])] = &[
            (
                &[("HOME", "/home/j")],
                ["/home/j/.config/pacdeb", "/home/j/.local/share/pacdeb", "/home/j/.cache/pacdeb"],
            ),
            (
                &[("HOME", "/home/j"), ("PACDEB_HOME", "/tmp/sb")],
                ["/tmp/sb/config", "/tmp/sb/state", "/tmp/sb/cache"],
            ),
            (
                &[("HOME", "/home/j"), ("XDG_CONFIG_HOME", "/x/cfg"), ("XDG_CACHE_HOME", "rel")],
                ["/x/cfg/pacdeb", "/home/j/.local/share/pacdeb", "/home/j/.cache/pacdeb"],
            ),
            (
                &[("HOME", "/home/j"), ("PACDEB_HOME", "")],
                ["/home/j/.config/pacdeb", "/home/j/.local/share/pacdeb", "/home/j/.cache/pacdeb"],
            ),
        ];
        for (vars, [config, state, cache]) in cases {
            let p = paths(vars).unwrap();
            assert_eq!(
                (p.config.to_str().unwrap(), p.state.to_str().unwrap(), p.cache.to_str().unwrap()),
                (*config, *state, *cache),
                "{vars:?}"
            );
        }
    }

    #[test]
    fn needs_home() {
        assert!(paths(&[]).unwrap_err().to_string().contains("HOME is not set"));
    }

    #[test]
    fn moves_the_old_ferry_folders_once() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("build/sandbox/test-migrate");
        let _ = fs::remove_dir_all(&base);
        let (config, state, cache) = (base.join(".config"), base.join(".local/share"), base.join(".cache"));
        for d in [&config, &state, &cache] {
            fs::create_dir_all(d.join("ferry")).unwrap();
        }
        fs::write(config.join("ferry/apps.toml"), "[settings]\n").unwrap();
        let old_pkg = cache.join("ferry/packages/app-1.0-1-x86_64.pkg.tar.zst");
        fs::write(state.join("ferry/state.toml"), format!("[apps.app]\npackages = [\"{}\"]\n", old_pkg.display())).unwrap();

        let p = Paths { config: config.join("pacdeb"), state: state.join("pacdeb"), cache: cache.join("pacdeb") };
        migrate_from_ferry(&p).unwrap();
        assert!(p.config.join("apps.toml").exists());
        assert!(!config.join("ferry").exists());
        let new_state = fs::read_to_string(p.state.join("state.toml")).unwrap();
        assert!(new_state.contains(&cache.join("pacdeb/packages/app-1.0-1-x86_64.pkg.tar.zst").display().to_string()), "{new_state}");

        // A second run, or one where the new folders already exist, changes nothing.
        fs::create_dir_all(config.join("ferry")).unwrap();
        migrate_from_ferry(&p).unwrap();
        assert!(config.join("ferry").exists());
    }
}
