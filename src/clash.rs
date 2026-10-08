//! Package names that a repository or the AUR also uses. pacman and AUR helpers match
//! installed packages by name alone, so a pacdeb build named like a repo or AUR package
//! gets replaced by that package on the next system update. Such builds get a suffix and
//! provide and conflict with the original name instead.

use std::process::Command;

use serde_json::Value;

use crate::error::{Result, bail};
use crate::net;

const SUFFIXES: &[&str] = &["-deb", "-pacdeb"];

/// Where a name is already taken.
pub trait Lookup {
    /// The sync repository that has `name`, if any.
    fn repo_of(&self, name: &str) -> Option<String>;
    /// Which of `names` exist in the AUR.
    fn in_aur(&self, names: &[String]) -> Result<Vec<String>>;
}

#[derive(Debug, PartialEq, Eq)]
pub struct Renamed {
    pub name: String,
    /// Where the original name is used, for telling the person why.
    pub because: String,
}

/// The name to build `base` under: None when `base` is free, otherwise the first free
/// suffixed name.
pub fn choose(base: &str, lookup: &dyn Lookup) -> Result<Option<Renamed>> {
    let candidates: Vec<String> = std::iter::once(base.to_string()).chain(SUFFIXES.iter().map(|s| format!("{base}{s}"))).collect();
    let aur = lookup.in_aur(&candidates)?;
    let taken = |n: &str| -> Option<String> {
        lookup.repo_of(n).map(|r| format!("the {r} repository")).or_else(|| aur.iter().any(|a| a == n).then(|| "the AUR".to_string()))
    };
    let Some(because) = taken(base) else {
        return Ok(None);
    };
    for name in &candidates[1..] {
        if taken(name).is_none() {
            return Ok(Some(Renamed { name: name.clone(), because }));
        }
    }
    bail!("{base} and its suffixed names are all taken in the repos or the AUR; pick a name with --pkgname")
}

/// Asks pacman and the AUR's web API. pacdeb's own local repository (`own_repo`) holds
/// earlier pacdeb builds, which are not a clash.
pub struct LiveLookup {
    pub own_repo: Option<String>,
}

impl LiveLookup {
    /// With the local repository named in pacdeb's settings, or its usual name: a
    /// [pacdeb] repository in pacman.conf holds pacdeb's builds even when the settings
    /// no longer say so.
    pub fn new() -> LiveLookup {
        let own_repo = crate::paths::Paths::from_env()
            .and_then(|p| crate::registry::Config::load(&p.config))
            .ok()
            .and_then(|c| c.settings.repo.map(|r| r.name))
            .unwrap_or_else(|| "pacdeb".to_string());
        LiveLookup { own_repo: Some(own_repo) }
    }
}

impl Lookup for LiveLookup {
    fn repo_of(&self, name: &str) -> Option<String> {
        let out = Command::new("pacman").args(["-Si", "--", name]).env("LC_ALL", "C").output().ok()?;
        if !out.status.success() {
            return None;
        }
        // A name can be in several repositories; any other than pacdeb's own is a clash.
        let text = String::from_utf8_lossy(&out.stdout);
        text.lines()
            .filter_map(|l| l.strip_prefix("Repository")?.split_once(':').map(|(_, v)| v.trim().to_string()))
            .find(|r| Some(r) != self.own_repo.as_ref())
    }

    fn in_aur(&self, names: &[String]) -> Result<Vec<String>> {
        let query: Vec<String> = names.iter().map(|n| format!("arg[]={n}")).collect();
        let text = net::get_text(&format!("https://aur.archlinux.org/rpc/v5/info?{}", query.join("&")), &[])?;
        aur_names(&text)
    }
}

/// The package names in an AUR RPC info response.
fn aur_names(text: &str) -> Result<Vec<String>> {
    let Ok(json) = serde_json::from_str::<Value>(text) else {
        bail!("the AUR answered with something that is not JSON");
    };
    if let Some(err) = json.get("error").and_then(Value::as_str) {
        bail!("the AUR says: {err}");
    }
    Ok(json
        .get("results")
        .and_then(Value::as_array)
        .map(|rs| rs.iter().filter_map(|r| r.get("Name")?.as_str().map(String::from)).collect())
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        repo: &'static [(&'static str, &'static str)],
        aur: &'static [&'static str],
    }

    impl Lookup for Fake {
        fn repo_of(&self, name: &str) -> Option<String> {
            self.repo.iter().find(|(n, _)| *n == name).map(|(_, r)| r.to_string())
        }
        fn in_aur(&self, names: &[String]) -> Result<Vec<String>> {
            Ok(names.iter().filter(|n| self.aur.contains(&n.as_str())).cloned().collect())
        }
    }

    #[test]
    fn picks_a_free_name() {
        let renamed = |name: &str, because: &str| Some(Renamed { name: name.into(), because: because.into() });
        let cases: &[(&str, Fake, Option<Renamed>)] = &[
            ("app", Fake { repo: &[], aur: &[] }, None),
            ("app", Fake { repo: &[("app", "cachyos")], aur: &[] }, renamed("app-deb", "the cachyos repository")),
            ("app", Fake { repo: &[], aur: &["app"] }, renamed("app-deb", "the AUR")),
            ("app", Fake { repo: &[("app", "extra")], aur: &["app", "app-deb"] }, renamed("app-pacdeb", "the extra repository")),
            // Only the original name matters for whether to rename.
            ("app", Fake { repo: &[], aur: &["app-deb"] }, None),
        ];
        for (base, lookup, want) in cases {
            assert_eq!(choose(base, lookup).unwrap(), *want, "{base}");
        }
        let all = Fake { repo: &[], aur: &["app", "app-deb", "app-pacdeb"] };
        assert!(choose("app", &all).unwrap_err().to_string().contains("--pkgname"));
    }

    #[test]
    fn reads_aur_answers() {
        let text = r#"{"resultcount":2,"results":[{"Name":"proton-mail","Version":"1.15.1-1"},{"Name":"proton-mail-bin"}],"type":"multiinfo","version":5}"#;
        assert_eq!(aur_names(text).unwrap(), ["proton-mail", "proton-mail-bin"]);
        assert_eq!(aur_names(r#"{"resultcount":0,"results":[]}"#).unwrap(), Vec::<String>::new());
        assert!(aur_names(r#"{"error":"Too many package arguments."}"#).unwrap_err().to_string().contains("Too many"));
        assert!(aur_names("<html>").is_err());
    }
}
