//! Where Ferry keeps its config, state and cache.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::error::{Result, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
}

impl Paths {
    pub fn from_env() -> Result<Paths> {
        Paths::from_vars(|k| std::env::var_os(k))
    }

    fn from_vars(get: impl Fn(&str) -> Option<OsString>) -> Result<Paths> {
        let var = |k: &str| get(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        if let Some(home) = var("FERRY_HOME") {
            return Ok(Paths {
                config: home.join("config"),
                state: home.join("state"),
                cache: home.join("cache"),
            });
        }
        let Some(home) = var("HOME") else {
            bail!("HOME is not set, so Ferry cannot find its config. Set HOME or FERRY_HOME.");
        };
        // XDG says relative values are invalid and should be ignored.
        let xdg = |k: &str, fallback: &str| {
            var(k).filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(fallback)).join("ferry")
        };
        Ok(Paths {
            config: xdg("XDG_CONFIG_HOME", ".config"),
            state: xdg("XDG_DATA_HOME", ".local/share"),
            cache: xdg("XDG_CACHE_HOME", ".cache"),
        })
    }
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
                ["/home/j/.config/ferry", "/home/j/.local/share/ferry", "/home/j/.cache/ferry"],
            ),
            (
                &[("HOME", "/home/j"), ("FERRY_HOME", "/tmp/sb")],
                ["/tmp/sb/config", "/tmp/sb/state", "/tmp/sb/cache"],
            ),
            (
                &[("HOME", "/home/j"), ("XDG_CONFIG_HOME", "/x/cfg"), ("XDG_CACHE_HOME", "rel")],
                ["/x/cfg/ferry", "/home/j/.local/share/ferry", "/home/j/.cache/ferry"],
            ),
            (
                &[("HOME", "/home/j"), ("FERRY_HOME", "")],
                ["/home/j/.config/ferry", "/home/j/.local/share/ferry", "/home/j/.cache/ferry"],
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
}
