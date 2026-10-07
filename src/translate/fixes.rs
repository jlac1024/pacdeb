//! Known fixes for specific debs: files to add or replace, extra optdepends and an
//! install note. They cover what general translation cannot know, like an app that
//! hands off to a Debian-only system script.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::error::{Context, Result};
use crate::model::{Node, NodeKind, Package, Source};

const BUILTIN: &str = include_str!("../../data/fixes.toml");

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fix {
    #[serde(default)]
    pub optdepends: Vec<String>,
    #[serde(default)]
    pub install_note: Vec<String>,
    #[serde(default)]
    pub replace_note: bool,
    #[serde(default)]
    pub files: Vec<FixFile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixFile {
    pub path: String,
    pub content: String,
    #[serde(default = "default_mode")]
    pub mode: String,
    pub why: String,
}

fn default_mode() -> String {
    "644".into()
}

pub struct Fixes {
    by_package: BTreeMap<String, Fix>,
}

impl Fixes {
    pub fn builtin() -> Fixes {
        Fixes { by_package: parse(BUILTIN, "built in fixes").expect("data/fixes.toml is checked by tests") }
    }

    /// The built in fixes with the user's `fixes.toml` from the config dir on top; a
    /// user entry replaces the built in one for that package.
    pub fn load(config_dir: &Path) -> Result<Fixes> {
        let mut fixes = Fixes::builtin();
        let user = config_dir.join("fixes.toml");
        match std::fs::read_to_string(&user) {
            Ok(text) => fixes.by_package.extend(parse(&text, &user.display().to_string())?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context(user.display()),
        }
        Ok(fixes)
    }

    /// Applies the fix for `deb_name`, if any. Returns change notes.
    pub fn apply(&self, deb_name: &str, pkg: &mut Package) -> Result<Vec<String>> {
        let Some(fix) = self.by_package.get(deb_name) else {
            return Ok(Vec::new());
        };
        let mut changes = Vec::new();
        for f in &fix.files {
            let mode = u32::from_str_radix(&f.mode, 8)
                .with_context_msg(|| format!("fix for {deb_name}: bad mode '{}' for {}", f.mode, f.path))?;
            // TOML multi line strings start after the newline, but a leading blank line
            // would break a #! line, so it is trimmed.
            let content = f.content.trim_start_matches('\n').as_bytes().to_vec();
            let node = Node { path: f.path.clone(), kind: NodeKind::File, mode, size: content.len() as u64, source: Source::Inline(content) };
            match pkg.nodes.iter_mut().find(|n| n.path == f.path) {
                Some(existing) => {
                    *existing = node;
                    changes.push(format!("replaced {} (known fix: {})", f.path, f.why));
                }
                None => {
                    pkg.nodes.push(node);
                    changes.push(format!("added {} (known fix: {})", f.path, f.why));
                }
            }
        }
        for o in &fix.optdepends {
            let (name, why) = o.split_once(':').map_or((o.as_str(), ""), |(n, w)| (n.trim(), w.trim()));
            if !pkg.optdepends.iter().any(|(p, _)| p == name) && !pkg.depends.iter().any(|d| d == name) {
                pkg.optdepends.push((name.to_string(), why.to_string()));
            }
        }
        if !fix.install_note.is_empty() {
            if fix.replace_note {
                pkg.install_note.clear();
            }
            pkg.install_note.extend(fix.install_note.iter().cloned());
        }
        Ok(changes)
    }
}

/// Small helper so a parse failure can carry a computed message.
trait WithContextMsg<T> {
    fn with_context_msg(self, msg: impl FnOnce() -> String) -> Result<T>;
}

impl<T, E: std::fmt::Display> WithContextMsg<T> for std::result::Result<T, E> {
    fn with_context_msg(self, msg: impl FnOnce() -> String) -> Result<T> {
        self.map_err(|e| crate::error::Error::new(format!("{}: {e}", msg())))
    }
}

fn parse(text: &str, source: &str) -> Result<BTreeMap<String, Fix>> {
    toml::from_str(text).context(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::{ArchVersion, DebVersion};

    fn package() -> Package {
        let v = DebVersion::parse("1.0").unwrap();
        Package {
            name: "chrome-remote-desktop".into(),
            version: ArchVersion::from_debian(&v, 1),
            deb_version: v,
            arch: "x86_64".into(),
            description: String::new(),
            url: None,
            license: "custom".into(),
            depends: vec![],
            optdepends: vec![],
            provides: vec![],
            conflicts: vec![],
            backup: vec![],
            install_note: vec!["generic service note".into()],
            nodes: vec![Node {
                path: "/opt/google/chrome-remote-desktop/Xsession".into(),
                kind: NodeKind::File,
                mode: 0o755,
                size: 100,
                source: Source::Deb("/opt/google/chrome-remote-desktop/Xsession".into()),
            }],
        }
    }

    #[test]
    fn builtin_fixes_parse_and_modes_are_octal() {
        let f = Fixes::builtin();
        for (pkg, fix) in &f.by_package {
            for file in &fix.files {
                assert!(u32::from_str_radix(&file.mode, 8).is_ok(), "{pkg}: {}", file.path);
                assert!(file.path.starts_with('/'), "{pkg}: {}", file.path);
            }
        }
    }

    #[test]
    fn applies_the_chrome_remote_desktop_fix() {
        let mut p = package();
        let changes = Fixes::builtin().apply("chrome-remote-desktop", &mut p).unwrap();
        assert_eq!(changes.len(), 2, "{changes:?}");
        assert!(changes.iter().any(|c| c.starts_with("added /usr/lib/systemd/system/chrome-remote-desktop@.service.d/10-pacdeb.conf")));
        assert!(changes.iter().any(|c| c.starts_with("replaced /opt/google/chrome-remote-desktop/Xsession")));

        let xsession = p.nodes.iter().find(|n| n.path.ends_with("/Xsession")).unwrap();
        assert_eq!(xsession.mode, 0o755);
        let Source::Inline(bytes) = &xsession.source else { panic!("not replaced") };
        assert!(bytes.starts_with(b"#!/bin/bash\n"), "{}", String::from_utf8_lossy(bytes));

        let dropin = p.nodes.iter().find(|n| n.path.ends_with("10-pacdeb.conf")).unwrap();
        let Source::Inline(bytes) = &dropin.source else { panic!() };
        assert!(String::from_utf8_lossy(bytes).contains("Environment=CHROME_REMOTE_DESKTOP_USE_XVFB=1"));

        assert!(!p.install_note.iter().any(|l| l == "generic service note"));
        assert!(p.install_note[0].contains("remotedesktop.google.com/headless"));
    }

    #[test]
    fn other_packages_are_untouched() {
        let mut p = package();
        assert!(Fixes::builtin().apply("proton-mail", &mut p).unwrap().is_empty());
        assert_eq!(p.install_note, ["generic service note"]);
    }
}
