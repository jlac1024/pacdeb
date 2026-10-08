// SPDX-License-Identifier: AGPL-3.0-or-later
//! Parser for a single deb822 paragraph, the format of DEBIAN/control.

use crate::error::{Error, Result, bail};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Control {
    /// Fields in file order. Continuation lines are joined with "\n" and keep any
    /// indentation past the first space.
    fields: Vec<(String, String)>,
}

impl Control {
    pub fn parse(text: &str) -> Result<Control> {
        let mut fields: Vec<(String, String)> = Vec::new();
        let mut ended = false;
        for (i, line) in text.lines().enumerate() {
            let n = i + 1;
            if line.trim().is_empty() {
                ended = !fields.is_empty();
                continue;
            }
            if line.starts_with('#') {
                continue;
            }
            if ended {
                bail!("line {n}: more than one paragraph");
            }
            if line.starts_with([' ', '\t']) {
                let Some((_, value)) = fields.last_mut() else {
                    bail!("line {n}: continuation line before any field");
                };
                value.push('\n');
                value.push_str(line[1..].trim_end());
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                bail!("line {n}: expected 'Field: value', got '{line}'");
            };
            if name.is_empty() || name.contains(char::is_whitespace) {
                bail!("line {n}: bad field name '{name}'");
            }
            if fields.iter().any(|(f, _)| f.eq_ignore_ascii_case(name)) {
                bail!("line {n}: duplicate field '{name}'");
            }
            fields.push((name.to_string(), value.trim().to_string()));
        }
        if fields.is_empty() {
            bail!("control file is empty");
        }
        Ok(Control { fields })
    }

    /// Field lookup. Field names are case insensitive in deb822.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(f, _)| f.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn require(&self, name: &str) -> Result<&str> {
        self.get(name)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Error::new(format!("control file has no {name} field")))
    }

    pub fn fields(&self) -> impl Iterator<Item = (&str, &str)> {
        self.fields.iter().map(|(n, v)| (n.as_str(), v.as_str()))
    }

    /// The Description synopsis and the extended text, with " ." lines turned into
    /// blank lines.
    pub fn description(&self) -> Option<(&str, String)> {
        let value = self.get("Description")?;
        let (synopsis, rest) = value.split_once('\n').unwrap_or((value, ""));
        let long = rest
            .lines()
            .map(|l| if l == "." { "" } else { l })
            .collect::<Vec<_>>()
            .join("\n");
        Some((synopsis, long))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fields() {
        let text = "\
Package: proton-mail
Version: 1.9.0
architecture: amd64
Depends: libgtk-3-0,
 libnss3 (>= 3.26),
 xdg-utils
Description: Proton Mail
 First paragraph
 .
   indented line
Empty:
";
        let c = Control::parse(text).unwrap();
        let cases: &[(&str, Option<&str>)] = &[
            ("Package", Some("proton-mail")),
            ("package", Some("proton-mail")),
            ("Architecture", Some("amd64")),
            ("Depends", Some("libgtk-3-0,\nlibnss3 (>= 3.26),\nxdg-utils")),
            ("Empty", Some("")),
            ("Missing", None),
        ];
        for (name, want) in cases {
            assert_eq!(c.get(name), *want, "field {name}");
        }
        let (synopsis, long) = c.description().unwrap();
        assert_eq!(synopsis, "Proton Mail");
        assert_eq!(long, "First paragraph\n\n  indented line");
        assert!(c.require("Empty").is_err());
        assert_eq!(c.fields().count(), 6);
    }

    #[test]
    fn tolerates_crlf_comments_and_trailing_blank_lines() {
        let c = Control::parse("# note\r\nPackage: a\r\nVersion: 1\r\n\r\n\r\n").unwrap();
        assert_eq!(c.get("Version"), Some("1"));
    }

    #[test]
    fn rejects_bad_input() {
        let cases = [
            ("", "empty"),
            ("\n\n", "empty"),
            (" leading continuation\n", "continuation line before any field"),
            ("Package a\n", "expected 'Field: value'"),
            ("Bad Name: x\n", "bad field name"),
            (": x\n", "bad field name"),
            ("Package: a\npackage: b\n", "duplicate field"),
            ("Package: a\n\nPackage: b\n", "more than one paragraph"),
        ];
        for (input, want) in cases {
            let err = Control::parse(input).unwrap_err().to_string();
            assert!(err.contains(want), "input {input:?}: expected '{want}', got '{err}'");
        }
    }
}
