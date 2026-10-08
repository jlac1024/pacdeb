// SPDX-License-Identifier: AGPL-3.0-or-later
//! Debian versions as described in deb-version(7): `[epoch:]upstream[-revision]`.

use std::cmp::Ordering;
use std::fmt;

use crate::error::{Error, Result, bail};

#[derive(Debug, Clone)]
pub struct DebVersion {
    pub epoch: u32,
    pub upstream: String,
    /// None when the version has no "-revision" part, which compares like "0".
    pub revision: Option<String>,
}

impl DebVersion {
    pub fn parse(s: &str) -> Result<DebVersion> {
        let s = s.trim();
        if s.is_empty() {
            bail!("version string is empty");
        }
        if s.contains(char::is_whitespace) {
            bail!("version '{s}' contains spaces");
        }

        let (epoch, rest) = match s.split_once(':') {
            Some((e, rest)) => {
                if e.is_empty() || !e.bytes().all(|b| b.is_ascii_digit()) {
                    bail!("version '{s}': epoch must be a number");
                }
                // dpkg stores the epoch as a signed int, so it caps at i32::MAX too.
                let epoch = e
                    .parse::<i32>()
                    .map_err(|_| Error::new(format!("version '{s}': epoch is too large")))?;
                if rest.is_empty() {
                    bail!("version '{s}': nothing after the epoch");
                }
                (epoch as u32, rest)
            }
            None => (0, s),
        };

        let (upstream, revision) = match rest.rsplit_once('-') {
            Some((_, "")) => bail!("version '{s}': revision after '-' is empty"),
            Some((u, r)) => (u, Some(r)),
            None => (rest, None),
        };
        if upstream.is_empty() {
            bail!("version '{s}': upstream part is empty");
        }
        if !upstream.starts_with(|c: char| c.is_ascii_digit()) {
            bail!("version '{s}': upstream part must start with a digit");
        }
        // A "-" in upstream implies a revision and a ":" implies an epoch; the splits
        // above guarantee both, so they are always fine here.
        if let Some(c) = upstream.chars().find(|c| !c.is_ascii_alphanumeric() && !".+~-:".contains(*c)) {
            bail!("version '{s}': invalid character '{c}' in upstream part");
        }
        if let Some(c) = revision.and_then(|r| r.chars().find(|c| !c.is_ascii_alphanumeric() && !".+~".contains(*c))) {
            bail!("version '{s}': invalid character '{c}' in revision");
        }

        Ok(DebVersion {
            epoch,
            upstream: upstream.to_string(),
            revision: revision.map(String::from),
        })
    }
}

impl Ord for DebVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.epoch
            .cmp(&other.epoch)
            .then_with(|| verrevcmp(&self.upstream, &other.upstream))
            .then_with(|| {
                verrevcmp(
                    self.revision.as_deref().unwrap_or(""),
                    other.revision.as_deref().unwrap_or(""),
                )
            })
    }
}

impl PartialOrd for DebVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// Equality has to follow the ordering: "1.0" and "1.00-0" are the same version.
impl PartialEq for DebVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for DebVersion {}

impl fmt::Display for DebVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.epoch > 0 {
            write!(f, "{}:", self.epoch)?;
        }
        f.write_str(&self.upstream)?;
        if let Some(r) = &self.revision {
            write!(f, "-{r}")?;
        }
        Ok(())
    }
}

/// Sort weight of one character in the non-digit part of a version. "~" sorts before
/// everything including the end of the string, letters before other symbols.
fn order(c: Option<u8>) -> i32 {
    match c {
        None => 0,
        Some(c) if c.is_ascii_digit() => 0,
        Some(c) if c.is_ascii_alphabetic() => c as i32,
        Some(b'~') => -1,
        Some(c) => c as i32 + 256,
    }
}

/// Compares an upstream or revision string the way dpkg does: alternate runs of
/// non-digits (compared with `order`) and digits (compared as numbers).
fn verrevcmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let digit_at = |s: &[u8], i: usize| s.get(i).is_some_and(u8::is_ascii_digit);
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        while (i < a.len() && !a[i].is_ascii_digit()) || (j < b.len() && !b[j].is_ascii_digit()) {
            let (ac, bc) = (order(a.get(i).copied()), order(b.get(j).copied()));
            if ac != bc {
                return ac.cmp(&bc);
            }
            i += 1;
            j += 1;
        }
        while a.get(i) == Some(&b'0') {
            i += 1;
        }
        while b.get(j) == Some(&b'0') {
            j += 1;
        }
        let mut first_diff = Ordering::Equal;
        while digit_at(a, i) && digit_at(b, j) {
            if first_diff == Ordering::Equal {
                first_diff = a[i].cmp(&b[j]);
            }
            i += 1;
            j += 1;
        }
        if digit_at(a, i) {
            return Ordering::Greater;
        }
        if digit_at(b, j) {
            return Ordering::Less;
        }
        if first_diff != Ordering::Equal {
            return first_diff;
        }
    }
    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;
    use Ordering::*;

    fn v(s: &str) -> DebVersion {
        DebVersion::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn parses_parts() {
        let cases: &[(&str, u32, &str, Option<&str>)] = &[
            ("1.0", 0, "1.0", None),
            ("1.0-1", 0, "1.0", Some("1")),
            ("2:1.0-1ubuntu1", 2, "1.0", Some("1ubuntu1")),
            ("0.9j-20080306-4", 0, "0.9j-20080306", Some("4")),
            ("1:2:3-4", 1, "2:3", Some("4")),
            ("1.0~rc1+dfsg-0~bpo1", 0, "1.0~rc1+dfsg", Some("0~bpo1")),
            (" 1.0 ", 0, "1.0", None),
        ];
        for (input, epoch, upstream, revision) in cases {
            let got = v(input);
            assert_eq!((got.epoch, got.upstream.as_str(), got.revision.as_deref()), (*epoch, *upstream, *revision), "{input}");
        }
    }

    #[test]
    fn rejects_bad_versions() {
        let cases = [
            ("", "empty"),
            ("1.0 2", "contains spaces"),
            (":1.0", "epoch must be a number"),
            ("a:1.0", "epoch must be a number"),
            ("-1:1.0", "epoch must be a number"),
            ("99999999999:1.0", "epoch is too large"),
            ("1:", "nothing after the epoch"),
            ("1.0-", "revision after '-' is empty"),
            ("-1", "upstream part is empty"),
            ("1:-1", "upstream part is empty"),
            ("v1.0", "must start with a digit"),
            ("1.0_1", "invalid character '_' in upstream"),
            ("1.0/2", "invalid character '/' in upstream"),
            ("1.0-1_2", "invalid character '_' in revision"),
            ("1.0-1~a@", "invalid character '@' in revision"),
        ];
        for (input, want) in cases {
            let err = DebVersion::parse(input).unwrap_err().to_string();
            assert!(err.contains(want), "{input:?}: expected '{want}', got '{err}'");
        }
    }

    /// Vectors from deb-version(7) and dpkg's own test suite.
    #[test]
    fn compares_like_dpkg() {
        let cases = [
            ("1.0", "1.0", Equal),
            ("1.0", "1.1", Less),
            ("1.0-1", "1.0-2", Less),
            ("1.0-1", "2.0-2", Less),
            ("2.2~rc-4", "2.2-1", Less),
            ("2.2-1", "2.2~rc-4", Greater),
            ("1.0000-1", "1.0-1", Equal),
            ("1", "0:1", Equal),
            ("0", "0:0-0", Equal),
            ("2:2.5", "1:7.5", Greater),
            ("1:0foo", "0foo", Greater),
            ("0:0foo", "0foo", Equal),
            ("0foo-0", "0foo", Equal),
            ("0foo", "0fo", Greater),
            ("0foo-0", "0foo+", Less),
            ("0foo~1", "0foo", Less),
            ("0foo~foo+Bar", "0foo~foo+bar", Less),
            ("0foo~~", "0foo~", Less),
            ("1~", "1", Less),
            ("1.0~rc1", "1.0", Less),
            ("1.0~rc1", "1.0~rc2", Less),
            ("1.0~~a", "1.0~~", Greater),
            ("12345+that-really-is-some-ver-0", "12345+that-really-is-some-ver-10", Less),
            ("0foo-0", "0foo-01", Less),
            ("0foo.bar", "0foobar", Greater),
            ("0foo.bar", "0foo1bar", Greater),
            ("0foo.bar", "0foo0bar", Greater),
            ("0foo1bar-1", "0foobar-1", Less),
            ("0foo2.0", "0foo2", Greater),
            ("0foo2.0.0", "0foo2.10.0", Less),
            ("0foo2.0", "0foo2.0.0", Less),
            ("0foo2.0", "0foo2.10", Less),
            ("0foo2.1", "0foo2.10", Less),
            ("1.09", "1.9", Equal),
            ("1.010", "1.10", Equal),
            ("1.0.8+nmu1", "1.0.8", Greater),
            ("3.11", "3.10+nmu1", Greater),
            ("0.9j-20080306-4", "0.9i-20070324-2", Greater),
            ("1.2.0~b7-1", "1.2.0~b6-1", Greater),
            ("1.011-1", "1.06-2", Greater),
            ("0.0.9+dfsg1-1", "0.0.8+dfsg1-3", Greater),
            ("4.6.99+svn6582-1", "4.6.99+svn6496-1", Greater),
            ("53", "52", Greater),
            ("0.9.9~pre122-1", "0.9.9~pre111-1", Greater),
            ("2:2.3.2-2+lenny2", "2:2.3.2-2", Greater),
            ("1:3.8.1-1", "3.8.GA-1", Greater),
            ("1.0.1+gpl-1", "1:1.0.1~beta-1", Less),
            ("1:1.0.1+gpl-1", "1:1.0.1~beta-1", Greater),
            ("1.0a", "1.0", Greater),
            ("1.0.a", "1.0.1", Greater),
            ("2.0-1", "2.0-1ubuntu1", Less),
            ("1.14.0", "1.9.2", Greater),
        ];
        for (a, b, want) in cases {
            assert_eq!(v(a).cmp(&v(b)), want, "{a} vs {b}");
            assert_eq!(v(b).cmp(&v(a)), want.reverse(), "{b} vs {a}");
        }
    }

    #[test]
    fn displays_canonical_form() {
        let cases = [("1.0", "1.0"), ("0:1.0-1", "1.0-1"), ("3:1.0~rc1-2", "3:1.0~rc1-2")];
        for (input, want) in cases {
            assert_eq!(v(input).to_string(), want);
        }
    }
}
