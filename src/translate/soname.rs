//! Suggests Arch packages for the shared libraries the package's binaries load. This
//! only ever suggests: nothing found here is added to depends.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

/// Packages every Arch system has; suggesting them is noise.
const BASE: [&str; 4] = ["glibc", "libgcc", "libstdc++", "gcc-libs"];

#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    /// soname to the "repo/package" names that ship /usr/lib/<soname>.
    Found(BTreeMap<String, Vec<String>>),
    NoDatabase,
    Unavailable(String),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SonameReport {
    /// Libraries an Arch package in depends already provides.
    pub covered: usize,
    /// soname, packages that provide it.
    pub suggestions: Vec<(String, Vec<String>)>,
    /// Libraries no Arch package provides at /usr/lib.
    pub missing: Vec<String>,
    /// Set when the lookup could not run at all.
    pub problem: Option<String>,
}

/// `needed` is every DT_NEEDED name; `bundled` are file names the package ships itself.
pub fn report(
    needed: &BTreeSet<String>,
    bundled: &BTreeSet<String>,
    depends: &[String],
    lookup: impl FnOnce(&[String]) -> Lookup,
) -> SonameReport {
    let wanted: Vec<String> = needed.iter().filter(|s| !bundled.contains(*s)).cloned().collect();
    let mut out = SonameReport::default();
    if wanted.is_empty() {
        return out;
    }
    let found = match lookup(&wanted) {
        Lookup::Found(f) => f,
        Lookup::NoDatabase => {
            out.problem = Some("pacman's file database is missing; run 'sudo pacman -Fy' once to get library suggestions".into());
            return out;
        }
        Lookup::Unavailable(why) => {
            out.problem = Some(format!("could not look up libraries: {why}"));
            return out;
        }
    };
    for soname in wanted {
        let owners = found.get(&soname).cloned().unwrap_or_default();
        let names: Vec<&str> = owners.iter().map(|o| o.rsplit('/').next().unwrap_or(o)).collect();
        if owners.is_empty() {
            out.missing.push(soname);
        } else if names.iter().any(|n| BASE.contains(n) || depends.iter().any(|d| d == n)) {
            out.covered += 1;
        } else {
            out.suggestions.push((soname, owners));
        }
    }
    out
}

/// Asks `pacman -F` which packages ship each library under /usr/lib.
pub fn pacman_lookup(sonames: &[String]) -> Lookup {
    let output = Command::new("pacman")
        .args(["-F", "--machinereadable"])
        .args(sonames)
        .env("LC_ALL", "C")
        .output();
    let output = match output {
        Ok(o) => o,
        Err(e) => return Lookup::Unavailable(format!("cannot run pacman: {e}")),
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("-Fy") {
        return Lookup::NoDatabase;
    }
    Lookup::Found(parse_machinereadable(&output.stdout))
}

/// Lines are "repo\0pkgname\0pkgver\0path\n". Only files directly in /usr/lib count,
/// which skips lib32 copies and private copies inside other apps.
fn parse_machinereadable(out: &[u8]) -> BTreeMap<String, Vec<String>> {
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in out.split(|b| *b == b'\n') {
        let fields: Vec<&[u8]> = line.split(|b| *b == 0).collect();
        let [repo, pkg, _ver, path] = fields.as_slice() else {
            continue;
        };
        let path = String::from_utf8_lossy(path);
        let Some(soname) = path.strip_prefix("usr/lib/").filter(|s| !s.contains('/')) else {
            continue;
        };
        let owner = format!("{}/{}", String::from_utf8_lossy(repo), String::from_utf8_lossy(pkg));
        let owners = found.entry(soname.to_string()).or_default();
        if !owners.contains(&owner) {
            owners.push(owner);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_pacman_output() {
        let out = b"extra\0nss\x003.101-1\0usr/lib/libnss3.so\n\
multilib\0lib32-nss\x003.101-1\0usr/lib32/libnss3.so\n\
extra\0gtk3\x001:3.24-1\0usr/lib/libgtk-3.so.0\n\
extra\0someapp\x001-1\0usr/lib/someapp/libnss3.so\n\
core\0glibc\x002.40-1\0usr/lib/libc.so.6\n";
        let found = parse_machinereadable(out);
        assert_eq!(found.get("libnss3.so").unwrap(), &["extra/nss"]);
        assert_eq!(found.get("libgtk-3.so.0").unwrap(), &["extra/gtk3"]);
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn sorts_libraries() {
        let needed = set(&["libc.so.6", "libnss3.so", "libgtk-3.so.0", "libffmpeg.so", "libweird.so.9", "libsecret-1.so.0"]);
        let bundled = set(&["libffmpeg.so"]);
        let depends = vec!["nss".to_string()];
        let r = report(&needed, &bundled, &depends, |wanted| {
            assert!(!wanted.contains(&"libffmpeg.so".to_string()));
            Lookup::Found(BTreeMap::from([
                ("libc.so.6".into(), vec!["core/glibc".into()]),
                ("libnss3.so".into(), vec!["extra/nss".into()]),
                ("libgtk-3.so.0".into(), vec!["extra/gtk3".into()]),
                ("libsecret-1.so.0".into(), vec!["extra/libsecret".into()]),
            ]))
        });
        assert_eq!(r.covered, 2);
        assert_eq!(
            r.suggestions,
            [("libgtk-3.so.0".to_string(), vec!["extra/gtk3".to_string()]), ("libsecret-1.so.0".to_string(), vec!["extra/libsecret".to_string()])]
        );
        assert_eq!(r.missing, ["libweird.so.9"]);
        assert_eq!(r.problem, None);
    }

    #[test]
    fn reports_lookup_problems() {
        let needed = set(&["libx.so"]);
        let r = report(&needed, &set(&[]), &[], |_| Lookup::NoDatabase);
        assert!(r.problem.unwrap().contains("pacman -Fy"));
        let r = report(&needed, &set(&[]), &[], |_| Lookup::Unavailable("boom".into()));
        assert!(r.problem.unwrap().contains("boom"));
        let r = report(&set(&[]), &set(&[]), &[], |_| panic!("no lookup for nothing"));
        assert_eq!(r, SonameReport::default());
    }
}
