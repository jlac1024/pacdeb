// SPDX-License-Identifier: AGPL-3.0-or-later
//! A tiny path language for picking values out of a JSON feed:
//! `Releases[CategoryName=Stable].File[Identifier=.deb (Ubuntu/Debian)].Url`.
//! A segment is a key, optionally followed by `[Field=Value]` (the first array element
//! whose Field equals Value) or `[N]` (the Nth element).

use serde_json::Value;

use crate::error::{Result, bail};

pub fn get<'a>(root: &'a Value, path: &str) -> Result<&'a Value> {
    let mut cur = root;
    for seg in segments(path)? {
        if !seg.key.is_empty() {
            cur = match cur.get(seg.key) {
                Some(v) => v,
                None => bail!("feed has no '{}' (in {path})", seg.key),
            };
        }
        if let Some(filter) = seg.filter {
            let Some(items) = cur.as_array() else {
                bail!("'{}' is not a list (in {path})", seg.key);
            };
            cur = match filter.split_once('=') {
                Some((field, want)) => match items.iter().find(|i| i.get(field).is_some_and(|v| scalar(v) == Some(want.to_string()))) {
                    Some(v) => v,
                    None => bail!("no entry in '{}' has {field} = {want} (in {path})", seg.key),
                },
                None => {
                    let Ok(n) = filter.parse::<usize>() else {
                        bail!("bad filter [{filter}] (in {path})");
                    };
                    match items.get(n) {
                        Some(v) => v,
                        None => bail!("'{}' has no entry {n} (in {path})", seg.key),
                    }
                }
            };
        }
    }
    Ok(cur)
}

/// The value at `path` as a string; numbers are written out, other types refused.
pub fn get_string(root: &Value, path: &str) -> Result<String> {
    let v = get(root, path)?;
    match scalar(v) {
        Some(s) => Ok(s),
        None => bail!("'{path}' is not a string or number in the feed"),
    }
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

struct Segment<'a> {
    key: &'a str,
    filter: Option<&'a str>,
}

/// Splits on dots outside brackets, since filter values may contain dots.
fn segments(path: &str) -> Result<Vec<Segment<'_>>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut depth = 0;
    let bytes = path.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'[' => depth += 1,
            b']' if depth > 0 => depth -= 1,
            b'.' if depth == 0 => {
                out.push(segment(&path[start..i], path)?);
                start = i + 1;
            }
            _ => {}
        }
    }
    if depth != 0 {
        bail!("unclosed '[' in {path}");
    }
    out.push(segment(&path[start..], path)?);
    Ok(out)
}

fn segment<'a>(s: &'a str, path: &str) -> Result<Segment<'a>> {
    match s.split_once('[') {
        Some((key, rest)) => match rest.strip_suffix(']') {
            Some(filter) => Ok(Segment { key, filter: Some(filter) }),
            None => bail!("bad segment '{s}' in {path}"),
        },
        None if s.is_empty() => bail!("empty segment in {path}"),
        None => Ok(Segment { key: s, filter: None }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proton() -> Value {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sources/proton-version.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn reads_the_proton_feed() {
        let v = proton();
        let stable = get_string(&v, "Releases[CategoryName=Stable].Version").unwrap();
        assert_eq!(stable, "1.14.0");
        let url = get_string(&v, "Releases[CategoryName=EarlyAccess].File[Identifier=.deb (Ubuntu/Debian)].Url").unwrap();
        assert_eq!(url, "https://proton.me/download/mail/linux/1.15.0/ProtonMail-desktop-beta.deb");
        let sum = get_string(&v, "Releases[CategoryName=EarlyAccess].File[Identifier=.deb (Ubuntu/Debian)].Sha512CheckSum").unwrap();
        assert!(sum.starts_with("9a0f3f4a1190b010"), "{sum}");
        assert_eq!(get_string(&v, "Releases[0].Version").unwrap(), "1.15.1");
    }

    #[test]
    fn reports_what_is_missing() {
        let v = proton();
        let cases = [
            ("Nope", "feed has no 'Nope'"),
            ("Releases[CategoryName=Beta].Version", "no entry in 'Releases' has CategoryName = Beta"),
            ("Releases[999].Version", "has no entry 999"),
            ("Releases[x].Version", "bad filter [x]"),
            ("Releases[0]", "is not a string or number"),
            ("Releases[0", "unclosed '['"),
            ("Releases..Version", "empty segment"),
        ];
        for (path, want) in cases {
            let err = get_string(&v, path).unwrap_err().to_string();
            assert!(err.contains(want), "{path}: expected '{want}', got '{err}'");
        }
    }
}
