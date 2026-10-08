// SPDX-License-Identifier: AGPL-3.0-or-later
//! Suggests Arch packages for the shared libraries the package's binaries load. This
//! only ever suggests: nothing found here is added to depends.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::process::Command;

/// Packages every Arch system has; suggesting them is noise.
const BASE: [&str; 4] = ["glibc", "libgcc", "libstdc++", "gcc-libs"];

/// Dependency trees are shallow; this only guards against a runaway loop.
const MAX_ROUNDS: usize = 30;

#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    /// soname to the package names that ship /usr/lib/<soname>.
    Found(BTreeMap<String, Vec<String>>),
    NoDatabase,
    Unavailable(String),
}

/// Everything the depends list pulls in, directly or through other packages.
#[derive(Debug, Default)]
pub struct Closure {
    pub packages: HashSet<String>,
    /// Library provides such as "libasound.so" (from "libasound.so=2-64").
    pub libs: HashSet<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SonameReport {
    /// Libraries depends already provides, directly or through its own dependencies.
    pub covered: usize,
    /// (packages that provide it, libraries), one entry per package choice.
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
    closure: impl FnOnce(&[String]) -> Closure,
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
    let closure = closure(depends);
    let lib_covered = |soname: &str| {
        closure
            .libs
            .iter()
            .any(|base| soname == base || soname.strip_prefix(base.as_str()).is_some_and(|r| r.starts_with('.')))
    };

    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for soname in wanted {
        let owners = found.get(&soname).cloned().unwrap_or_default();
        if owners.is_empty() {
            out.missing.push(soname);
        } else if lib_covered(&soname)
            || owners
                .iter()
                .any(|o| BASE.contains(&o.as_str()) || depends.contains(o) || closure.packages.contains(o))
        {
            out.covered += 1;
        } else {
            grouped.entry(owners.join(" or ")).or_default().push(soname);
        }
    }
    out.suggestions = grouped.into_iter().collect();
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

/// Walks the dependency tree of `roots` with `pacman -Si`, which reads the sync
/// database only. Virtual names that no package is called (like "libgl") end the walk
/// on that branch, so the result can miss a little; it only feeds suggestions.
pub fn pacman_closure(roots: &[String]) -> Closure {
    let mut closure = Closure::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = roots.iter().cloned().collect();
    for _ in 0..MAX_ROUNDS {
        let batch: Vec<String> = queue.drain(..).filter(|n| seen.insert(n.clone())).collect();
        if batch.is_empty() {
            break;
        }
        let Ok(output) = Command::new("pacman").arg("-Si").args(&batch).env("LC_ALL", "C").output() else {
            break;
        };
        for pkg in parse_si(&String::from_utf8_lossy(&output.stdout)) {
            closure.packages.insert(pkg.name);
            for p in pkg.provides {
                if p.contains(".so") {
                    closure.libs.insert(p.clone());
                }
                closure.packages.insert(p);
            }
            for d in pkg.depends {
                if d.contains(".so") {
                    closure.libs.insert(d);
                } else if !seen.contains(&d) {
                    queue.push_back(d);
                }
            }
        }
    }
    closure
}

#[derive(Debug, PartialEq, Eq)]
struct SiPackage {
    name: String,
    depends: Vec<String>,
    provides: Vec<String>,
}

/// Reads the Name, Provides and Depends On fields of `pacman -Si` output, with version
/// parts like ">=1.2" or "=2-64" cut off.
fn parse_si(text: &str) -> Vec<SiPackage> {
    let names = |v: &str| -> Vec<String> {
        if v.trim() == "None" {
            return Vec::new();
        }
        v.split_whitespace()
            .map(|d| d.split(['<', '>', '=']).next().unwrap_or(d).to_string())
            .collect()
    };
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let mut pkg = SiPackage { name: String::new(), depends: Vec::new(), provides: Vec::new() };
        for line in block.lines() {
            let Some((key, value)) = line.split_once(" : ") else {
                continue;
            };
            match key.trim() {
                "Name" => pkg.name = value.trim().to_string(),
                "Depends On" => pkg.depends = names(value),
                "Provides" => pkg.provides = names(value),
                _ => {}
            }
        }
        if !pkg.name.is_empty() {
            out.push(pkg);
        }
    }
    out
}

/// Lines are "repo\0pkgname\0pkgver\0path\n". Only files directly in /usr/lib count,
/// which skips lib32 copies and private copies inside other apps. The same package
/// from several repos is listed once.
fn parse_machinereadable(out: &[u8]) -> BTreeMap<String, Vec<String>> {
    let mut found: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for line in out.split(|b| *b == b'\n') {
        let fields: Vec<&[u8]> = line.split(|b| *b == 0).collect();
        let [_repo, pkg, _ver, path] = fields.as_slice() else {
            continue;
        };
        let path = String::from_utf8_lossy(path);
        let Some(soname) = path.strip_prefix("usr/lib/").filter(|s| !s.contains('/')) else {
            continue;
        };
        let pkg = String::from_utf8_lossy(pkg).into_owned();
        let owners = found.entry(soname.to_string()).or_default();
        if !owners.contains(&pkg) {
            owners.push(pkg);
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

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_pacman_f_output() {
        let out = b"extra\0nss\x003.101-1\0usr/lib/libnss3.so\n\
cachyos-core-znver4\0nss\x003.101-1.1\0usr/lib/libnss3.so\n\
cachyos\0nss-hg\x003.102-1\0usr/lib/libnss3.so\n\
multilib\0lib32-nss\x003.101-1\0usr/lib32/libnss3.so\n\
extra\0gtk3\x001:3.24-1\0usr/lib/libgtk-3.so.0\n\
extra\0someapp\x001-1\0usr/lib/someapp/libnss3.so\n";
        let found = parse_machinereadable(out);
        assert_eq!(found.get("libnss3.so").unwrap(), &["nss", "nss-hg"]);
        assert_eq!(found.get("libgtk-3.so.0").unwrap(), &["gtk3"]);
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn parses_pacman_si_output() {
        let text = "Repository      : extra\nName            : alsa-lib\nVersion         : 1.2-1\n\
Provides        : libasound.so=2-64  libatopology.so=2-64\n\
Depends On      : alsa-topology-conf  glibc>=2.40\n\n\
Repository      : extra\nName            : tiny\nProvides        : None\nDepends On      : None\n";
        assert_eq!(
            parse_si(text),
            [
                SiPackage {
                    name: "alsa-lib".into(),
                    depends: strings(&["alsa-topology-conf", "glibc"]),
                    provides: strings(&["libasound.so", "libatopology.so"]),
                },
                SiPackage { name: "tiny".into(), depends: vec![], provides: vec![] },
            ]
        );
    }

    #[test]
    fn sorts_libraries() {
        let needed = set(&[
            "libc.so.6",
            "libnss3.so",
            "libX11.so.6",
            "libdbus-1.so.3",
            "libffmpeg.so",
            "libweird.so.9",
            "libasound.so.2",
            "libsecret-1.so.0",
            "libsecret-extra.so.0",
        ]);
        let bundled = set(&["libffmpeg.so"]);
        let depends = strings(&["nss", "gtk3"]);
        let r = report(
            &needed,
            &bundled,
            &depends,
            |wanted| {
                assert!(!wanted.contains(&"libffmpeg.so".to_string()));
                Lookup::Found(BTreeMap::from([
                    ("libc.so.6".into(), strings(&["glibc"])),
                    ("libnss3.so".into(), strings(&["nss"])),
                    ("libX11.so.6".into(), strings(&["libx11"])),
                    ("libdbus-1.so.3".into(), strings(&["dbus"])),
                    ("libasound.so.2".into(), strings(&["alsa-lib"])),
                    ("libsecret-1.so.0".into(), strings(&["libsecret"])),
                    ("libsecret-extra.so.0".into(), strings(&["libsecret"])),
                ]))
            },
            |roots| {
                assert_eq!(roots, ["nss", "gtk3"]);
                Closure {
                    packages: ["nss", "gtk3", "libx11"].iter().map(|s| s.to_string()).collect(),
                    libs: ["libdbus-1.so"].iter().map(|s| s.to_string()).collect(),
                }
            },
        );
        assert_eq!(r.covered, 4);
        assert_eq!(
            r.suggestions,
            [
                ("alsa-lib".to_string(), strings(&["libasound.so.2"])),
                ("libsecret".to_string(), strings(&["libsecret-1.so.0", "libsecret-extra.so.0"])),
            ]
        );
        assert_eq!(r.missing, ["libweird.so.9"]);
        assert_eq!(r.problem, None);
    }

    #[test]
    fn reports_lookup_problems() {
        let needed = set(&["libx.so"]);
        let no_closure = |_: &[String]| -> Closure { panic!("closure not needed") };
        let r = report(&needed, &set(&[]), &[], |_| Lookup::NoDatabase, no_closure);
        assert!(r.problem.unwrap().contains("pacman -Fy"));
        let r = report(&needed, &set(&[]), &[], |_| Lookup::Unavailable("boom".into()), no_closure);
        assert!(r.problem.unwrap().contains("boom"));
        let r = report(&set(&[]), &set(&[]), &[], |_| panic!("no lookup for nothing"), no_closure);
        assert_eq!(r, SonameReport::default());
    }
}
