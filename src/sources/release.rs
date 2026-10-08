//! What an apt repository's signed Release file says about the repository: who
//! publishes it, when it was last updated, and until when it may be trusted.

use crate::control::Control;
use crate::error::{Result, bail};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub origin: Option<String>,
    pub label: Option<String>,
    pub suite: Option<String>,
    pub codename: Option<String>,
    pub date: Option<String>,
    pub valid_until: Option<String>,
    pub architectures: Vec<String>,
    pub components: Vec<String>,
}

pub fn info(release: &str) -> ReleaseInfo {
    let Ok(c) = Control::parse(release) else {
        return ReleaseInfo::default();
    };
    let one = |k: &str| c.get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let list = |k: &str| c.get(k).map(|v| v.split_whitespace().map(String::from).collect()).unwrap_or_default();
    ReleaseInfo {
        origin: one("Origin"),
        label: one("Label"),
        suite: one("Suite"),
        codename: one("Codename"),
        date: one("Date"),
        valid_until: one("Valid-Until"),
        architectures: list("Architectures"),
        components: list("Components"),
    }
}

/// Refuses a Release file whose Valid-Until has passed, as apt does: an old signed
/// Release could be served again to hide newer versions.
pub fn check_fresh(info: &ReleaseInfo, now: i64) -> Result<()> {
    let Some(until) = &info.valid_until else {
        return Ok(());
    };
    match parse_date(until) {
        Some(t) if t < now => bail!("the repository's Release file expired on {until} (Valid-Until); it is out of date, or an old copy is being served"),
        Some(_) => Ok(()),
        None => bail!("the repository's Release file has a Valid-Until pacdeb cannot read: {until}"),
    }
}

/// Seconds since 1970 for a Release date like "Sat, 04 Oct 2026 12:34:56 UTC" (the
/// weekday is optional; the zone is UTC, GMT, Z or +hhmm/-hhmm).
pub fn parse_date(s: &str) -> Option<i64> {
    let s = s.split_once(',').map_or(s, |(_, rest)| rest).trim();
    let mut parts = s.split_whitespace();
    let day: i64 = parts.next()?.parse().ok()?;
    let mon = parts.next()?;
    let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].iter().position(|m| *m == mon)? as i64 + 1;
    let year: i64 = parts.next()?.parse().ok()?;
    let mut hms = parts.next()?.split(':').map(|p| p.parse::<i64>().ok());
    let (h, m, sec) = (hms.next()??, hms.next()??, hms.next().flatten().unwrap_or(0));
    let offset = match parts.next().unwrap_or("UTC") {
        "UTC" | "GMT" | "Z" => 0,
        z if z.len() == 5 && (z.starts_with('+') || z.starts_with('-')) => {
            let n: i64 = z[1..].parse().ok()?;
            let secs = (n / 100) * 3600 + (n % 100) * 60;
            if z.starts_with('-') { -secs } else { secs }
        }
        _ => return None,
    };
    if !(1..=31).contains(&day) || h > 23 || m > 59 || sec > 60 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86400 + h * 3600 + m * 60 + sec - offset)
}

/// Days since 1970-01-01 for a date in the proleptic Gregorian calendar.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// "3 days ago", "in 2 hours", for showing dates relative to now.
pub fn relative(t: i64, now: i64) -> String {
    let d = (now - t).abs();
    let amount = match d {
        0..=119 => "a moment".to_string(),
        120..=7199 => format!("{} minutes", d / 60),
        7200..=172_799 => format!("{} hours", d / 3600),
        _ => format!("{} days", d / 86400),
    };
    if t <= now { format!("{amount} ago") } else { format!("in {amount}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_dates() {
        let cases = [
            ("Thu, 01 Jan 1970 00:00:00 UTC", Some(0)),
            ("Sat, 03 Oct 2026 12:34:56 UTC", Some(1_791_030_896)),
            ("03 Oct 2026 12:34:56 +0000", Some(1_791_030_896)),
            ("Sat, 03 Oct 2026 14:34:56 +0200", Some(1_791_030_896)),
            ("Sat, 03 Oct 2026 07:34:56 -0500", Some(1_791_030_896)),
            ("Tue, 29 Feb 2028 00:00:00 GMT", Some(1_835_395_200)),
            ("Sat, 03 Oct 2026 12:34 UTC", Some(1_791_030_840)),
            ("Sat, 03 Foo 2026 12:34:56 UTC", None),
            ("Sat, 03 Oct 2026 12:34:56 CEST", None),
            ("", None),
        ];
        for (input, want) in cases {
            assert_eq!(parse_date(input), want, "{input}");
        }
    }

    #[test]
    fn reads_release_fields() {
        let text = "Origin: Example\nLabel: Example\nSuite: stable\nCodename: stable\nDate: Sat, 03 Oct 2026 12:34:56 UTC\n\
                    Valid-Until: Sat, 10 Oct 2026 12:34:56 UTC\nArchitectures: amd64 arm64\nComponents: main\nSHA256:\n abc 1 main/x\n";
        let i = info(text);
        assert_eq!(i.origin.as_deref(), Some("Example"));
        assert_eq!(i.architectures, ["amd64", "arm64"]);
        assert_eq!(i.components, ["main"]);
        let until = parse_date(i.valid_until.as_deref().unwrap()).unwrap();
        assert!(check_fresh(&i, until - 1).is_ok());
        assert!(check_fresh(&i, until + 1).unwrap_err().to_string().contains("expired"));
        assert!(check_fresh(&ReleaseInfo::default(), 0).is_ok());
    }

    #[test]
    fn says_how_long_ago() {
        let cases = [(1000, 1000, "a moment ago"), (0, 600, "10 minutes ago"), (0, 3 * 86400, "3 days ago"), (7200, 0, "in 2 hours")];
        for (t, now, want) in cases {
            assert_eq!(relative(t, now), want);
        }
    }
}
