//! Parser for Debian relationship fields such as Depends and Conflicts.

use std::fmt;

use crate::error::{Error, Result, bail};

pub const RELATION_FIELDS: [&str; 9] = [
    "Pre-Depends",
    "Depends",
    "Recommends",
    "Suggests",
    "Enhances",
    "Breaks",
    "Conflicts",
    "Provides",
    "Replaces",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionOp {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
}

impl VersionOp {
    fn parse(s: &str) -> Option<VersionOp> {
        Some(match s {
            "<<" => VersionOp::Lt,
            // "<" and ">" are obsolete spellings that dpkg reads as "<=" and ">=".
            "<=" | "<" => VersionOp::Le,
            "=" => VersionOp::Eq,
            ">=" | ">" => VersionOp::Ge,
            ">>" => VersionOp::Gt,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            VersionOp::Lt => "<<",
            VersionOp::Le => "<=",
            VersionOp::Eq => "=",
            VersionOp::Ge => ">=",
            VersionOp::Gt => ">>",
        }
    }
}

/// One package reference, like `libc6:any (>= 2.34) [amd64]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Atom {
    pub name: String,
    /// Multiarch qualifier after ":", such as "any".
    pub arch_qualifier: Option<String>,
    pub version: Option<(VersionOp, String)>,
    /// Architecture restriction list from "[...]", kept as written ("amd64", "!i386").
    pub arches: Vec<String>,
}

/// A comma separated field becomes a list of groups; each group is a list of
/// alternatives joined by "|".
pub type Relations = Vec<Vec<Atom>>;

pub fn parse_relations(field: &str) -> Result<Relations> {
    let mut out = Vec::new();
    for group in field.split(',') {
        if group.trim().is_empty() {
            continue;
        }
        let alts = group.split('|').map(parse_atom).collect::<Result<Vec<_>>>()?;
        out.push(alts);
    }
    Ok(out)
}

fn parse_atom(raw: &str) -> Result<Atom> {
    let s = raw.trim();
    let end = s
        .find(|c: char| c.is_whitespace() || "([<".contains(c))
        .unwrap_or(s.len());
    let (full_name, rest) = s.split_at(end);
    let (name, arch_qualifier) = match full_name.split_once(':') {
        Some((n, a)) => (n, Some(a.to_string())),
        None => (full_name, None),
    };
    if name.is_empty() || arch_qualifier.as_deref() == Some("") {
        bail!("missing package name in '{s}'");
    }

    let mut rest = rest.trim_start();
    let mut version = None;
    if let Some(r) = rest.strip_prefix('(') {
        let Some((inner, after)) = r.split_once(')') else {
            bail!("unclosed '(' in '{s}'");
        };
        let inner = inner.trim();
        let op_len = inner.find(|c: char| !"<>=".contains(c)).unwrap_or(inner.len());
        let (op, ver) = inner.split_at(op_len);
        let op = VersionOp::parse(op)
            .ok_or_else(|| Error::new(format!("bad version operator '{op}' in '{s}'")))?;
        let ver = ver.trim();
        if ver.is_empty() || ver.contains(char::is_whitespace) {
            bail!("bad version '{ver}' in '{s}'");
        }
        version = Some((op, ver.to_string()));
        rest = after.trim_start();
    }

    let mut arches = Vec::new();
    if let Some(r) = rest.strip_prefix('[') {
        let Some((inner, after)) = r.split_once(']') else {
            bail!("unclosed '[' in '{s}'");
        };
        arches = inner.split_whitespace().map(String::from).collect();
        if arches.is_empty() {
            bail!("empty architecture list in '{s}'");
        }
        rest = after.trim_start();
    }

    // Build profiles only matter when building from source, so they are accepted and dropped.
    while let Some(r) = rest.strip_prefix('<') {
        let Some((_, after)) = r.split_once('>') else {
            bail!("unclosed '<' in '{s}'");
        };
        rest = after.trim_start();
    }

    if !rest.is_empty() {
        bail!("unexpected '{rest}' in '{s}'");
    }
    Ok(Atom {
        name: name.to_string(),
        arch_qualifier,
        version,
        arches,
    })
}

impl fmt::Display for Atom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)?;
        if let Some(q) = &self.arch_qualifier {
            write!(f, ":{q}")?;
        }
        if let Some((op, v)) = &self.version {
            write!(f, " ({} {v})", op.as_str())?;
        }
        if !self.arches.is_empty() {
            write!(f, " [{}]", self.arches.join(" "))?;
        }
        Ok(())
    }
}

pub fn format_group(group: &[Atom]) -> String {
    group.iter().map(Atom::to_string).collect::<Vec<_>>().join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(field: &str) -> Vec<String> {
        parse_relations(field).unwrap().iter().map(|g| format_group(g)).collect()
    }

    #[test]
    fn parses_relations() {
        let cases: &[(&str, &[&str])] = &[
            ("", &[]),
            ("libc6", &["libc6"]),
            ("libc6 (>= 2.34)", &["libc6 (>= 2.34)"]),
            ("libc6(>=2.34)", &["libc6 (>= 2.34)"]),
            ("a, b , c,", &["a", "b", "c"]),
            ("libasound2 | libasound2t64", &["libasound2 | libasound2t64"]),
            ("python3:any", &["python3:any"]),
            ("foo (= 1:2.0-1~bpo1)", &["foo (= 1:2.0-1~bpo1)"]),
            ("foo (<< 2), bar (>> 1)", &["foo (<< 2)", "bar (>> 1)"]),
            ("old (< 2), older (> 1)", &["old (<= 2)", "older (>= 1)"]),
            ("foo [amd64 !i386]", &["foo [amd64 !i386]"]),
            ("foo (>= 1) [amd64] <!nocheck> <cross>", &["foo (>= 1) [amd64]"]),
            ("libgtk-3-0,\nlibnss3 (>= 3.26),\n xdg-utils", &["libgtk-3-0", "libnss3 (>= 3.26)", "xdg-utils"]),
            ("libstdc++6, libgl1-mesa-glx | libgl1", &["libstdc++6", "libgl1-mesa-glx | libgl1"]),
        ];
        for (input, want) in cases {
            assert_eq!(render(input), *want, "input {input:?}");
        }
    }

    #[test]
    fn fills_atom_fields() {
        let rel = parse_relations("libc6:amd64 (>= 2.34) [amd64]").unwrap();
        assert_eq!(
            rel[0][0],
            Atom {
                name: "libc6".into(),
                arch_qualifier: Some("amd64".into()),
                version: Some((VersionOp::Ge, "2.34".into())),
                arches: vec!["amd64".into()],
            }
        );
    }

    #[test]
    fn rejects_bad_relations() {
        let cases = [
            ("foo (>= 1", "unclosed '('"),
            ("foo (~= 1)", "bad version operator"),
            ("foo (>=)", "bad version"),
            ("foo (>= 1 2)", "bad version"),
            ("foo [amd64", "unclosed '['"),
            ("foo []", "empty architecture list"),
            ("foo <nocheck", "unclosed '<'"),
            ("foo bar", "unexpected 'bar'"),
            ("a | | b", "missing package name"),
            ("(>= 1)", "missing package name"),
            ("foo:", "missing package name"),
        ];
        for (input, want) in cases {
            let err = parse_relations(input).unwrap_err().to_string();
            assert!(err.contains(want), "input {input:?}: expected '{want}', got '{err}'");
        }
    }
}
