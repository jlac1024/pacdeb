//! Debian relationship fields to pacman depends and optdepends.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use crate::control::Control;
use crate::error::{Context, Result};
use crate::relation::{Atom, format_group, parse_relations};

const BUILTIN: &str = include_str!("../../data/depmap.toml");

/// Fields that name Debian packages pacman knows nothing about. Ferry lists them
/// instead of carrying them over; per app overrides cover real conflicts.
const NOT_CARRIED: [&str; 5] = ["Conflicts", "Breaks", "Provides", "Replaces", "Enhances"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MapFile {
    #[serde(default)]
    map: HashMap<String, String>,
}

pub struct DepMap {
    map: HashMap<String, String>,
}

impl DepMap {
    pub fn builtin() -> DepMap {
        DepMap {
            map: parse(BUILTIN, "built in depmap").expect("data/depmap.toml is checked by tests"),
        }
    }

    /// The built in table with the user's `depmap.toml` from the config dir on top.
    pub fn load(config_dir: &Path) -> Result<DepMap> {
        let mut dm = DepMap::builtin();
        let user = config_dir.join("depmap.toml");
        match std::fs::read_to_string(&user) {
            Ok(text) => dm.map.extend(parse(&text, &user.display().to_string())?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context(user.display()),
        }
        Ok(dm)
    }

    /// Some("") means Arch does not need the dependency at all.
    pub fn get(&self, debian: &str) -> Option<&str> {
        self.map.get(debian).map(String::as_str)
    }
}

fn parse(text: &str, source: &str) -> Result<HashMap<String, String>> {
    Ok(toml::from_str::<MapFile>(text).context(source)?.map)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Deps {
    pub depends: Vec<String>,
    pub optdepends: Vec<(String, String)>,
    /// Informational: dropped constraints, deps the map says Arch does not need,
    /// fields that are not carried over.
    pub notes: Vec<String>,
    /// Groups with no mapping, like "Depends: libfoo2 | libfoo3".
    pub unmapped: Vec<String>,
}

/// `installed` says which Arch packages are installed; when the deb offers
/// alternatives, an installed one wins over the first one listed.
pub fn translate(
    control: &Control,
    deb_arch: &str,
    map: &DepMap,
    installed: impl FnOnce(&[String]) -> HashSet<String>,
) -> Result<Deps> {
    let mut deps = Deps::default();
    let mut constraints = Vec::new();
    let fields = [
        ("Pre-Depends", None),
        ("Depends", None),
        ("Recommends", Some("recommended by the deb")),
        ("Suggests", Some("suggested by the deb")),
    ];
    let mut parsed = Vec::new();
    for (field, optional) in fields {
        if let Some(value) = control.get(field) {
            let groups: Vec<Vec<Atom>> = parse_relations(value)
                .context(field)?
                .into_iter()
                .map(|g| g.into_iter().filter(|a| applies_to(a, deb_arch)).collect::<Vec<_>>())
                .filter(|g| !g.is_empty())
                .collect();
            parsed.push((field, optional, groups));
        }
    }

    let mut choices: Vec<String> = Vec::new();
    for (_, _, groups) in &parsed {
        for g in groups.iter().filter(|g| g.len() > 1) {
            for m in g.iter().filter_map(|a| map.get(&a.name)).filter(|m| !m.is_empty()) {
                if !choices.iter().any(|c| c == m) {
                    choices.push(m.to_string());
                }
            }
        }
    }
    let installed = if choices.is_empty() { HashSet::new() } else { installed(&choices) };

    for (field, optional, groups) in &parsed {
        for group in groups {
            let shown = format_group(group);
            let mapped: Vec<(&Atom, &str)> = group.iter().filter_map(|a| map.get(&a.name).map(|m| (a, m))).collect();
            let Some(&(first_atom, first)) = mapped.first() else {
                deps.unmapped.push(format!("{field}: {shown}"));
                continue;
            };
            if first.is_empty() {
                deps.notes.push(format!("{field}: {} is not needed on Arch", first_atom.name));
                continue;
            }
            let (atom, arch) = mapped
                .iter()
                .find(|(_, m)| !m.is_empty() && installed.contains(*m))
                .copied()
                .unwrap_or((first_atom, first));
            if arch != first {
                deps.notes.push(format!("{field}: picked {arch} from '{shown}' because it is installed"));
            }
            if atom.version.is_some() {
                constraints.push(atom.to_string());
            }
            let arch = arch.to_string();
            match *optional {
                None if !deps.depends.contains(&arch) => deps.depends.push(arch),
                Some(reason) if !deps.depends.contains(&arch) && !deps.optdepends.iter().any(|(p, _)| *p == arch) => {
                    deps.optdepends.push((arch, reason.to_string()));
                }
                _ => {}
            }
        }
    }
    // A dep can show up in Recommends and later turn out to be required.
    deps.optdepends.retain(|(p, _)| !deps.depends.contains(p));

    if !constraints.is_empty() {
        deps.notes.push(format!(
            "version constraints dropped, Debian and Arch versions do not compare: {}",
            constraints.join(", ")
        ));
    }
    for field in NOT_CARRIED {
        if let Some(value) = control.get(field) {
            deps.notes.push(format!("{field}: {} (Debian names, not carried over)", value.replace('\n', " ")));
        }
    }
    Ok(deps)
}

/// Asks `pacman -Qq` which names are installed. pacman resolves provides and prints
/// the provider, so the answer is read from which names it reports as missing.
pub fn pacman_installed(names: &[String]) -> HashSet<String> {
    let Ok(output) = Command::new("pacman").arg("-Qq").args(names).env("LC_ALL", "C").output() else {
        return HashSet::new();
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    let missing: HashSet<&str> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("error: package '")?.split_once('\'').map(|(n, _)| n))
        .collect();
    names.iter().filter(|n| !missing.contains(n.as_str())).cloned().collect()
}

/// Honors a "[amd64 !i386]" restriction, which binary packages rarely carry.
fn applies_to(atom: &Atom, deb_arch: &str) -> bool {
    if atom.arches.is_empty() {
        return true;
    }
    let negated = atom.arches.iter().all(|a| a.starts_with('!'));
    if negated {
        !atom.arches.iter().any(|a| &a[1..] == deb_arch)
    } else {
        atom.arches.iter().any(|a| a == deb_arch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> DepMap {
        DepMap { map: pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect() }
    }

    #[test]
    fn builtin_map_parses_and_has_basics() {
        let m = DepMap::builtin();
        let cases = [("libgtk-3-0", "gtk3"), ("libnss3", "nss"), ("libasound2t64", "alsa-lib"), ("libc6", "glibc")];
        for (deb, arch) in cases {
            assert_eq!(m.get(deb), Some(arch), "{deb}");
        }
        for (deb, arch) in &m.map {
            assert!(!arch.contains(char::is_whitespace), "{deb} maps to '{arch}'");
        }
    }

    #[test]
    fn rejects_unknown_keys_in_map_files() {
        assert!(parse("[map]\na = \"b\"\n", "x").is_ok());
        assert!(parse("[mapp]\na = \"b\"\n", "x").is_err());
        assert!(parse("[map]\na = 1\n", "x").is_err());
    }

    #[test]
    fn user_file_overrides_builtin() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("build/sandbox/test-depmap");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("depmap.toml"), "[map]\n\"libnss3\" = \"nss-custom\"\n\"newlib\" = \"newpkg\"\n").unwrap();
        let m = DepMap::load(&dir).unwrap();
        assert_eq!(m.get("libnss3"), Some("nss-custom"));
        assert_eq!(m.get("newlib"), Some("newpkg"));
        assert_eq!(m.get("libgtk-3-0"), Some("gtk3"));
        let missing = DepMap::load(&dir.join("nope")).unwrap();
        assert_eq!(missing.get("libnss3"), Some("nss"));
    }

    #[test]
    fn translates_fields() {
        let m = map(&[
            ("libgtk-3-0", "gtk3"),
            ("libnss3", "nss"),
            ("libasound2t64", "alsa-lib"),
            ("pulseaudio", "libpulse"),
            ("lsb-base", ""),
            ("libsecret-1-0", "libsecret"),
            ("libgtk-3-0t64", "gtk3"),
        ]);
        let control = Control::parse(
            "Package: a\nVersion: 1\nArchitecture: amd64\n\
             Pre-Depends: lsb-base\n\
             Depends: libgtk-3-0 (>= 3.10), libnss3, libasound2 | libasound2t64, libunknown1, libgtk-3-0t64\n\
             Recommends: pulseaudio | libasound2, libsecret-1-0, libnss3\n\
             Suggests: libsecret-1-0, missing-suggest\n\
             Conflicts: other-app\n",
        )
        .unwrap();
        let d = translate(&control, "amd64", &m, |_| HashSet::new()).unwrap();
        assert_eq!(d.depends, ["gtk3", "nss", "alsa-lib"]);
        assert_eq!(
            d.optdepends,
            [("libpulse".to_string(), "recommended by the deb".to_string()), ("libsecret".to_string(), "recommended by the deb".to_string())]
        );
        assert_eq!(d.unmapped, ["Depends: libunknown1", "Suggests: missing-suggest"]);
        assert_eq!(
            d.notes,
            [
                "Pre-Depends: lsb-base is not needed on Arch",
                "version constraints dropped, Debian and Arch versions do not compare: libgtk-3-0 (>= 3.10)",
                "Conflicts: other-app (Debian names, not carried over)",
            ]
        );
    }

    #[test]
    fn prefers_installed_alternatives() {
        let m = map(&[("portal-gtk", "xdg-desktop-portal-gtk"), ("portal-kde", "xdg-desktop-portal-kde"), ("solo", "solo")]);
        let control = Control::parse("Package: a\nVersion: 1\nArchitecture: amd64\nDepends: portal-gtk | portal-kde, solo\n").unwrap();
        let d = translate(&control, "amd64", &m, |names| {
            // Only groups with a real choice are asked about.
            assert_eq!(names, ["xdg-desktop-portal-gtk", "xdg-desktop-portal-kde"]);
            HashSet::from(["xdg-desktop-portal-kde".to_string()])
        })
        .unwrap();
        assert_eq!(d.depends, ["xdg-desktop-portal-kde", "solo"]);
        assert_eq!(d.notes, ["Depends: picked xdg-desktop-portal-kde from 'portal-gtk | portal-kde' because it is installed"]);

        let d = translate(&control, "amd64", &m, |_| HashSet::new()).unwrap();
        assert_eq!(d.depends, ["xdg-desktop-portal-gtk", "solo"]);
    }

    #[test]
    fn arch_restrictions() {
        let atom = |arches: &[&str]| Atom {
            name: "x".into(),
            arch_qualifier: None,
            version: None,
            arches: arches.iter().map(|s| s.to_string()).collect(),
        };
        let cases: &[(&[&str], bool)] = &[(&[], true), (&["amd64"], true), (&["arm64"], false), (&["!amd64"], false), (&["!i386"], true)];
        for (arches, want) in cases {
            assert_eq!(applies_to(&atom(arches), "amd64"), *want, "{arches:?}");
        }
    }

    #[test]
    fn bad_relation_is_an_error() {
        let control = Control::parse("Package: a\nVersion: 1\nArchitecture: amd64\nDepends: foo (>= 1\n").unwrap();
        let err = translate(&control, "amd64", &map(&[]), |_| HashSet::new()).unwrap_err().to_string();
        assert!(err.starts_with("Depends: "), "{err}");
    }
}
