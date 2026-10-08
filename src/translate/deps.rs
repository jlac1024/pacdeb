// SPDX-License-Identifier: AGPL-3.0-or-later
//! Debian relationship fields to pacman depends and optdepends.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

use crate::control::Control;
use crate::error::{Context, Result};
use crate::relation::{Atom, format_group, parse_relations};

const BUILTIN: &str = include_str!("../../data/depmap.toml");

/// Fields that name Debian packages pacman knows nothing about. pacdeb lists them
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
/// alternatives, an installed one wins over the first one listed. `in_repos` says
/// which names exist in the sync repos, which is what lets a naming rule map a name
/// the table does not know.
pub fn translate(
    control: &Control,
    deb_arch: &str,
    map: &DepMap,
    installed: impl FnOnce(&[String]) -> HashSet<String>,
    in_repos: impl FnOnce(&[String]) -> HashSet<String>,
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

    // Names the table does not know get naming rules, but only names pacman confirms.
    let unknown: Vec<&str> = parsed
        .iter()
        .flat_map(|(_, _, groups)| groups.iter().flatten())
        .map(|a| a.name.as_str())
        .filter(|n| map.get(n).is_none())
        .collect();
    let mut wanted: Vec<String> = Vec::new();
    for n in &unknown {
        for (c, _) in rule_candidates(n) {
            if !wanted.contains(&c) {
                wanted.push(c);
            }
        }
    }
    let existing = if wanted.is_empty() { HashSet::new() } else { in_repos(&wanted) };
    let by_rule: HashMap<&str, (String, &str)> = unknown
        .iter()
        .filter_map(|n| rule_candidates(n).into_iter().find(|(c, _)| existing.contains(c)).map(|hit| (*n, hit)))
        .collect();
    let resolve = |name: &str| -> Option<String> {
        map.get(name).map(String::from).or_else(|| by_rule.get(name).map(|(c, _)| c.clone()))
    };

    let mut choices: Vec<String> = Vec::new();
    for (_, _, groups) in &parsed {
        for g in groups.iter().filter(|g| g.len() > 1) {
            for m in g.iter().filter_map(|a| resolve(&a.name)) {
                for part in m.split_whitespace() {
                    if !choices.iter().any(|c| c == part) {
                        choices.push(part.to_string());
                    }
                }
            }
        }
    }
    let installed = if choices.is_empty() { HashSet::new() } else { installed(&choices) };

    for (field, optional, groups) in &parsed {
        for group in groups {
            let shown = format_group(group);
            let mapped: Vec<(&Atom, String)> = group.iter().filter_map(|a| resolve(&a.name).map(|m| (a, m))).collect();
            let Some((first_atom, first)) = mapped.first().cloned() else {
                deps.unmapped.push(format!("{field}: {shown}"));
                continue;
            };
            if first.is_empty() {
                deps.notes.push(format!("{field}: {} is not needed on Arch", first_atom.name));
                continue;
            }
            let (atom, arch) = mapped
                .iter()
                .find(|(_, m)| !m.is_empty() && m.split_whitespace().all(|p| installed.contains(p)))
                .cloned()
                .unwrap_or((first_atom, first.clone()));
            if arch != first {
                deps.notes.push(format!("{field}: picked {arch} from '{shown}' because it is installed"));
            }
            if let Some((_, rule)) = by_rule.get(atom.name.as_str()) {
                deps.notes.push(format!("{field}: {} -> {arch} by naming rule ({rule}), confirmed in your repos", atom.name));
            }
            if atom.version.is_some() {
                constraints.push(atom.to_string());
            }
            // One Debian package can need several Arch ones, written space separated.
            for part in arch.split_whitespace().map(String::from) {
                match *optional {
                    None if !deps.depends.contains(&part) => deps.depends.push(part),
                    Some(reason) if !deps.depends.contains(&part) && !deps.optdepends.iter().any(|(p, _)| *p == part) => {
                        deps.optdepends.push((part, reason.to_string()));
                    }
                    _ => {}
                }
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

/// Arch names a Debian name might have, most specific rule first. Each is only used
/// if the sync repos have a package by exactly that name.
fn rule_candidates(name: &str) -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = Vec::new();
    let mut push = |cand: String, rule: &'static str| {
        if !cand.is_empty() && !out.iter().any(|(c, _)| *c == cand) {
            out.push((cand, rule));
        }
    };
    // Debian's 64 bit time_t transition renamed many libraries with a t64 suffix.
    let base = name.strip_suffix("t64").unwrap_or(name);
    if let Some(rest) = base.strip_prefix("python3-") {
        push(format!("python-{rest}"), "python3-* is python-* on Arch");
    }
    if let Some(rest) = base.strip_suffix("-dev") {
        push(rest.to_string(), "Arch has no separate -dev packages");
    }
    if let Some(stem) = strip_soversion(base) {
        push(stem.to_string(), "library name without its soname version");
        if let Some(bare) = stem.strip_prefix("lib") {
            push(bare.to_string(), "library name without lib and its soname version");
        }
    }
    push(base.to_string(), "same name on Arch");
    out
}

/// "libsecret-1-0" -> "libsecret", "libnotify4" -> "libnotify". Only for lib* names.
fn strip_soversion(name: &str) -> Option<&str> {
    if !name.starts_with("lib") {
        return None;
    }
    let stem = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '-');
    (stem != name && stem.len() > 3).then_some(stem)
}

/// Asks `pacman -Si` which of these exact names are packages in the sync repos.
pub fn pacman_in_repos(names: &[String]) -> HashSet<String> {
    let Ok(output) = Command::new("pacman").arg("-Si").args(names).env("LC_ALL", "C").output() else {
        return HashSet::new();
    };
    let wanted: HashSet<&str> = names.iter().map(String::as_str).collect();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("Name")?.trim_start().strip_prefix(':').map(str::trim))
        .filter(|n| wanted.contains(n))
        .map(String::from)
        .collect()
}

/// The installed Python as "X.Y", from `pacman -Q python`.
pub fn pacman_python_version() -> Option<String> {
    let output = Command::new("pacman").args(["-Q", "python"]).env("LC_ALL", "C").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text.split_whitespace().nth(1)?;
    let version = version.split_once(':').map_or(version, |(_, v)| v);
    let mut parts = version.split(['.', '-']);
    let (major, minor) = (parts.next()?, parts.next()?);
    (major.chars().all(|c| c.is_ascii_digit()) && minor.chars().all(|c| c.is_ascii_digit()))
        .then(|| format!("{major}.{minor}"))
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
            for part in arch.split_whitespace() {
                assert!(part.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "@._+-".contains(c)), "{deb} maps to '{arch}'");
            }
            assert!(!arch.starts_with(' ') && !arch.ends_with(' ') && !arch.contains("  "), "{deb} maps to '{arch}'");
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
        let d = translate(&control, "amd64", &m, |_| HashSet::new(), |_| HashSet::new()).unwrap();
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
        let d = translate(
            &control,
            "amd64",
            &m,
            |names| {
                // Only groups with a real choice are asked about.
                assert_eq!(names, ["xdg-desktop-portal-gtk", "xdg-desktop-portal-kde"]);
                HashSet::from(["xdg-desktop-portal-kde".to_string()])
            },
            |_| HashSet::new(),
        )
        .unwrap();
        assert_eq!(d.depends, ["xdg-desktop-portal-kde", "solo"]);
        assert_eq!(d.notes, ["Depends: picked xdg-desktop-portal-kde from 'portal-gtk | portal-kde' because it is installed"]);

        let d = translate(&control, "amd64", &m, |_| HashSet::new(), |_| HashSet::new()).unwrap();
        assert_eq!(d.depends, ["xdg-desktop-portal-gtk", "solo"]);
    }

    #[test]
    fn rule_candidates_by_name() {
        let names = |n: &str| rule_candidates(n).into_iter().map(|(c, _)| c).collect::<Vec<_>>();
        let cases: &[(&str, &[&str])] = &[
            ("python3-requests", &["python-requests", "python3-requests"]),
            ("libsecret-1-0", &["libsecret", "secret", "libsecret-1-0"]),
            ("libnotify4", &["libnotify", "notify", "libnotify4"]),
            ("libfoo3t64", &["libfoo", "foo", "libfoo3"]),
            ("libfoo-dev", &["libfoo", "libfoo-dev"]),
            ("psmisc", &["psmisc"]),
            ("libpam0g", &["libpam0g"]),
        ];
        for (input, want) in cases {
            assert_eq!(names(input), *want, "{input}");
        }
    }

    #[test]
    fn maps_by_rule_only_when_the_repos_have_it() {
        let control = Control::parse(
            "Package: a\nVersion: 1\nArchitecture: amd64\nDepends: python3-requests (>= 2), libfoo2, psmisc, made-up-thing\n",
        )
        .unwrap();
        let d = translate(&control, "amd64", &map(&[]), |_| HashSet::new(), |asked| {
            assert!(asked.contains(&"python-requests".to_string()));
            ["python-requests", "libfoo", "psmisc"].iter().map(|s| s.to_string()).collect()
        })
        .unwrap();
        assert_eq!(d.depends, ["python-requests", "libfoo", "psmisc"]);
        assert_eq!(d.unmapped, ["Depends: made-up-thing"]);
        assert!(d.notes.iter().any(|n| n == "Depends: python3-requests -> python-requests by naming rule (python3-* is python-* on Arch), confirmed in your repos"), "{:?}", d.notes);
        assert!(d.notes.iter().any(|n| n.contains("psmisc -> psmisc by naming rule (same name on Arch)")), "{:?}", d.notes);
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
        let err = translate(&control, "amd64", &map(&[]), |_| HashSet::new(), |_| HashSet::new()).unwrap_err().to_string();
        assert!(err.starts_with("Depends: "), "{err}");
    }
}
