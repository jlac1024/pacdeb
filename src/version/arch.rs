//! Arch versions (`[epoch:]pkgver-pkgrel`), pacman's vercmp, and the Debian to Arch mapping.

use std::cmp::Ordering;
use std::fmt;

use super::DebVersion;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchVersion {
    pub epoch: u32,
    pub pkgver: String,
    pub pkgrel: u32,
}

impl ArchVersion {
    /// Maps a Debian version. The Debian revision is dropped because pkgrel is pacdeb's own
    /// rebuild counter, and ":" and "-" become "_" because pkgver may not contain them.
    pub fn from_debian(v: &DebVersion, pkgrel: u32) -> ArchVersion {
        debug_assert!(pkgrel >= 1, "pkgrel starts at 1");
        ArchVersion {
            epoch: v.epoch,
            pkgver: v.upstream.replace([':', '-'], "_"),
            pkgrel,
        }
    }
}

impl fmt::Display for ArchVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.epoch > 0 {
            write!(f, "{}:", self.epoch)?;
        }
        write!(f, "{}-{}", self.pkgver, self.pkgrel)
    }
}

/// pacman's vercmp: compares full `[epoch:]pkgver[-pkgrel]` strings. The pkgrel only
/// counts when both sides have one.
#[allow(dead_code)] // first used by update checks against the installed package
pub fn vercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (e1, v1, r1) = split_evr(a);
    let (e2, v2, r2) = split_evr(b);
    rpmvercmp(e1, e2)
        .then_with(|| rpmvercmp(v1, v2))
        .then_with(|| match (r1, r2) {
            (Some(r1), Some(r2)) => rpmvercmp(r1, r2),
            _ => Ordering::Equal,
        })
}

/// Splits into (epoch, version, release). A missing epoch is "0", and the release is
/// whatever follows the last "-".
fn split_evr(s: &str) -> (&str, &str, Option<&str>) {
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    let (epoch, version_start) = if s[digits..].starts_with(':') {
        (if digits == 0 { "0" } else { &s[..digits] }, digits + 1)
    } else {
        ("0", 0)
    };
    match s[version_start..].rfind('-') {
        Some(i) => {
            let dash = version_start + i;
            (epoch, &s[version_start..dash], Some(&s[dash + 1..]))
        }
        None => (epoch, &s[version_start..], None),
    }
}

/// The segment comparison behind pacman's vercmp. Versions are split into runs of digits
/// or letters; everything else is a separator. Numbers beat letters, and a trailing
/// letter run counts as older than nothing at all ("1.0rc" < "1.0").
fn rpmvercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut one, mut two) = (0, 0);
    let (mut p1, mut p2) = (0, 0);

    while one < a.len() && two < b.len() {
        while one < a.len() && !a[one].is_ascii_alphanumeric() {
            one += 1;
        }
        while two < b.len() && !b[two].is_ascii_alphanumeric() {
            two += 1;
        }
        if one >= a.len() || two >= b.len() {
            break;
        }
        // Different separator lengths decide on their own ("2___a" > "2_a").
        if one - p1 != two - p2 {
            return (one - p1).cmp(&(two - p2));
        }

        p1 = one;
        p2 = two;
        let numeric = a[p1].is_ascii_digit();
        let same_class = |c: u8| if numeric { c.is_ascii_digit() } else { c.is_ascii_alphabetic() };
        while p1 < a.len() && same_class(a[p1]) {
            p1 += 1;
        }
        while p2 < b.len() && same_class(b[p2]) {
            p2 += 1;
        }
        let (seg1, seg2) = (&a[one..p1], &b[two..p2]);
        if seg2.is_empty() {
            // The two segments are of different types: numbers are newer.
            return if numeric { Ordering::Greater } else { Ordering::Less };
        }
        let ord = if numeric {
            let s1 = trim_zeros(seg1);
            let s2 = trim_zeros(seg2);
            s1.len().cmp(&s2.len()).then_with(|| s1.cmp(s2))
        } else {
            seg1.cmp(seg2)
        };
        if ord != Ordering::Equal {
            return ord;
        }
        one = p1;
        two = p2;
    }

    let (c1, c2) = (a.get(one).copied(), b.get(two).copied());
    if c1.is_none() && c2.is_none() {
        return Ordering::Equal;
    }
    // A leftover letter run never beats running out ("1.0a" < "1.0"); any other
    // leftover is newer.
    let alpha = |c: Option<u8>| c.is_some_and(|c| c.is_ascii_alphabetic());
    if (c1.is_none() && !alpha(c2)) || alpha(c1) {
        Ordering::Less
    } else {
        Ordering::Greater
    }
}

fn trim_zeros(s: &[u8]) -> &[u8] {
    let start = s.iter().take_while(|&&c| c == b'0').count();
    &s[start..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use Ordering::*;

    /// Vectors from pacman's vercmp test script and the vercmp(8) man page.
    #[test]
    fn compares_like_pacman() {
        let cases = [
            ("1.5.0", "1.5.0", Equal),
            ("1.5.1", "1.5.0", Greater),
            ("1.5.1", "1.5", Greater),
            ("1.5.0-1", "1.5.0-1", Equal),
            ("1.5.0-1", "1.5.0-2", Less),
            ("1.5.0-1", "1.5.1-1", Less),
            ("1.5.0-2", "1.5.1-1", Less),
            ("1.5-1", "1.5.1-1", Less),
            ("1.5-2", "1.5.1-1", Less),
            ("1.5-2", "1.5.1-2", Less),
            ("1.5", "1.5-1", Equal),
            ("1.5-1", "1.5", Equal),
            ("1.1-1", "1.1", Equal),
            ("1.0-1", "1.1", Less),
            ("1.1-1", "1.0", Greater),
            ("1.5b-1", "1.5-1", Less),
            ("1.5b", "1.5", Less),
            ("1.5b-1", "1.5", Less),
            ("1.5b", "1.5.1", Less),
            ("1.0a", "1.0alpha", Less),
            ("1.0alpha", "1.0b", Less),
            ("1.0b", "1.0beta", Less),
            ("1.0beta", "1.0rc", Less),
            ("1.0rc", "1.0", Less),
            ("1.5.a", "1.5", Greater),
            ("1.5.b", "1.5.a", Greater),
            ("1.5.1", "1.5.b", Greater),
            ("1.5.b-1", "1.5.b", Equal),
            ("1.5-1", "1.5.b", Less),
            ("2.0", "2_0", Equal),
            ("2.0_a", "2_0.a", Equal),
            ("2.0a", "2.0.a", Less),
            ("2___a", "2_a", Greater),
            ("0:1.0", "0:1.0", Equal),
            ("0:1.0", "0:1.1", Less),
            ("1:1.0", "0:1.0", Greater),
            ("1:1.0", "0:1.1", Greater),
            ("1:1.0", "2:1.1", Less),
            ("1:1.0", "0:1.0-1", Greater),
            ("1:1.0-1", "0:1.1-1", Greater),
            ("0:1.0", "1.0", Equal),
            ("0:1.0", "1.1", Less),
            ("0:1.1", "1.0", Greater),
            ("1:1.0", "1.0", Greater),
            ("1:1.0", "1.1", Greater),
            ("1:1.1", "1.1", Greater),
            ("1.010", "1.10", Equal),
            ("1.14.0-1", "1.9.2-3", Greater),
        ];
        for (a, b, want) in cases {
            assert_eq!(vercmp(a, b), want, "{a} vs {b}");
            assert_eq!(vercmp(b, a), want.reverse(), "{b} vs {a}");
        }
    }

    #[test]
    fn splits_evr() {
        let cases = [
            ("1.0", ("0", "1.0", None)),
            ("1.0-2", ("0", "1.0", Some("2"))),
            ("3:1.0-2", ("3", "1.0", Some("2"))),
            (":1.0", ("0", "1.0", None)),
            ("1.0_beta-1-2", ("0", "1.0_beta-1", Some("2"))),
        ];
        for (input, want) in cases {
            assert_eq!(split_evr(input), want, "{input}");
        }
    }

    #[test]
    fn maps_debian_versions() {
        let cases = [
            ("1.14.0", 1, "1.14.0-1"),
            ("1.14.0-1", 3, "1.14.0-3"),
            ("0:1.0-5", 1, "1.0-1"),
            ("1:2.0-3", 1, "1:2.0-1"),
            ("2.0~rc1-1", 1, "2.0~rc1-1"),
            ("1.0+dfsg-2", 2, "1.0+dfsg-2"),
            ("0.9j-20080306-4", 1, "0.9j_20080306-1"),
            ("1:2:3-1", 1, "1:2_3-1"),
            ("1.0.3-beta-1ubuntu2", 1, "1.0.3_beta-1"),
        ];
        for (deb, pkgrel, want) in cases {
            let v = DebVersion::parse(deb).unwrap();
            let arch = ArchVersion::from_debian(&v, pkgrel);
            assert_eq!(arch.to_string(), want, "{deb}");
            assert!(!arch.pkgver.contains([':', '-', '/']), "{deb}: bad pkgver {}", arch.pkgver);
        }
    }

    /// Where dpkg and pacman agree or disagree on the order of two mapped versions.
    /// pacman treats "~" as a plain separator and a trailing letter run as older, so
    /// pre-releases and letter suffixes flip.
    #[test]
    fn mapped_order_against_dpkg() {
        let cases = [
            ("1.0", "1.1", true),
            ("1.0+dfsg", "1.0", true),
            ("1.0~rc1", "1.0~rc2", true),
            ("1:1.0", "2.0", true),
            ("1.0~rc1", "1.0", false),
            ("1.0a", "1.0", false),
            ("1.0.a", "1.0.1", false),
        ];
        for (a, b, agrees) in cases {
            let (da, db) = (DebVersion::parse(a).unwrap(), DebVersion::parse(b).unwrap());
            let (pa, pb) = (ArchVersion::from_debian(&da, 1), ArchVersion::from_debian(&db, 1));
            let same = da.cmp(&db) == vercmp(&pa.to_string(), &pb.to_string());
            assert_eq!(same, agrees, "{a} vs {b}");
        }
    }
}
