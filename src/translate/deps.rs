//! Debian relationship fields to pacman depends and optdepends.

use std::collections::HashMap;
use std::path::Path;

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

pub fn translate(control: &Control, deb_arch: &str, map: &DepMap) -> Result<Deps> {
    let mut deps = Deps::default();
    let mut constraints = Vec::new();
    let fields = [
        ("Pre-Depends", None),
        ("Depends", None),
        ("Recommends", Some("recommended by the deb")),
        ("Suggests", Some("suggested by the deb")),
    ];
    for (field, optional) in fields {
        let Some(value) = control.get(field) else {
            continue;
        };
        for group in parse_relations(value).context(field)? {
            let group: Vec<&Atom> = group.iter().filter(|a| applies_to(a, deb_arch)).collect();
            if group.is_empty() {
                continue;
            }
            let shown = format_group(&group.iter().map(|a| (*a).clone()).collect::<Vec<_>>());
            let Some((atom, arch)) = group.iter().find_map(|a| map.get(&a.name).map(|m| (*a, m))) else {
                deps.unmapped.push(format!("{field}: {shown}"));
                continue;
            };
            if arch.is_empty() {
                deps.notes.push(format!("{field}: {} is not needed on Arch", atom.name));
                continue;
            }
            if atom.version.is_some() {
                constraints.push(atom.to_string());
            }
            let arch = arch.to_string();
            match optional {
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
        let d = translate(&control, "amd64", &m).unwrap();
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
        let err = translate(&control, "amd64", &map(&[])).unwrap_err().to_string();
        assert!(err.starts_with("Depends: "), "{err}");
    }
}
