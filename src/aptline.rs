//! Reading the repository lines vendors publish for apt: one-line `deb ...` entries
//! (sources.list style) and deb822 `.sources` paragraphs.

use crate::error::{Result, bail};

/// One repository described by a vendor's apt line or .sources file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Described {
    pub url: String,
    pub suite: String,
    pub components: Vec<String>,
    pub arch: Option<String>,
    pub signed_by: Option<SignedBy>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignedBy {
    /// A key file path on a Debian system; usually not present here.
    Path(String),
    /// A key pasted into the .sources file itself.
    Inline(String),
}

/// Reads apt lines or a .sources file. Comments, empty lines and deb-src entries are
/// skipped; deb822 entries with several URIs or suites give one repository each.
pub fn parse(text: &str) -> Result<Vec<Described>> {
    let is_deb822 = text.lines().any(|l| l.trim_start().to_ascii_lowercase().starts_with("uris:"));
    let found = if is_deb822 { parse_deb822(text)? } else { parse_lines(text)? };
    if found.is_empty() {
        bail!("no 'deb' repository found; expected a line like 'deb https://example.com/apt stable main'");
    }
    Ok(found)
}

fn parse_lines(text: &str) -> Result<Vec<Described>> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some(rest) = line.strip_prefix("deb ").or_else(|| line.strip_prefix("deb\t")) else {
            if line.starts_with("deb-src") {
                continue;
            }
            bail!("not an apt line: {line}");
        };
        let rest = rest.trim();
        let (options, rest) = match rest.strip_prefix('[') {
            Some(r) => match r.split_once(']') {
                Some((opts, after)) => (opts, after.trim()),
                None => bail!("unclosed '[' in: {line}"),
            },
            None => ("", rest),
        };
        let mut words = rest.split_whitespace();
        let (Some(url), Some(suite)) = (words.next(), words.next()) else {
            bail!("an apt line needs a URL and a suite: {line}");
        };
        let mut d = Described { url: url.to_string(), suite: suite.to_string(), components: words.map(String::from).collect(), arch: None, signed_by: None };
        for opt in options.split_whitespace() {
            match opt.split_once('=') {
                Some(("arch", v)) => d.arch = v.split(',').next().map(String::from),
                Some(("signed-by", v)) => d.signed_by = Some(SignedBy::Path(v.to_string())),
                _ => {}
            }
        }
        check(&d, line)?;
        out.push(d);
    }
    Ok(out)
}

fn parse_deb822(text: &str) -> Result<Vec<Described>> {
    let mut out = Vec::new();
    for para in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        // Fields with their continuation lines; a line holding only "." is an empty line.
        let mut fields: Vec<(String, String)> = Vec::new();
        for line in para.lines() {
            if line.trim_start().starts_with('#') {
                continue;
            }
            if line.starts_with([' ', '\t']) {
                if let Some((_, v)) = fields.last_mut() {
                    let l = line.trim();
                    v.push('\n');
                    v.push_str(if l == "." { "" } else { l });
                }
            } else if let Some((k, v)) = line.split_once(':') {
                fields.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
            }
        }
        let get = |k: &str| fields.iter().find(|(f, _)| f == k).map(|(_, v)| v.clone());
        let words = |k: &str| get(k).map(|v| v.split_whitespace().map(String::from).collect::<Vec<_>>()).unwrap_or_default();
        if get("enabled").is_some_and(|v| v.eq_ignore_ascii_case("no")) || !words("types").iter().any(|t| t == "deb") {
            continue;
        }
        let signed_by = get("signed-by").map(|v| {
            let v = v.trim().to_string();
            if v.contains("BEGIN PGP PUBLIC KEY BLOCK") { SignedBy::Inline(v) } else { SignedBy::Path(v) }
        });
        for url in words("uris") {
            for suite in words("suites") {
                let d = Described { url: url.clone(), suite, components: words("components"), arch: words("architectures").into_iter().next(), signed_by: signed_by.clone() };
                check(&d, &format!("the entry for {url}"))?;
                out.push(d);
            }
        }
    }
    Ok(out)
}

fn check(d: &Described, what: &str) -> Result<()> {
    if !d.url.starts_with("https://") && !d.url.starts_with("http://") {
        bail!("{what}: pacdeb reads http and https repositories only");
    }
    let flat = d.suite.ends_with('/');
    if flat && !d.components.is_empty() {
        bail!("{what}: a flat repository (suite ending in /) has no components");
    }
    if !flat && d.components.is_empty() {
        bail!("{what}: no components after the suite (often 'main')");
    }
    Ok(())
}

/// A short name for a repository, from its URL: the host without "www.", "apt.",
/// "repo." or "packages." in front, and the TLD dropped.
pub fn suggest_name(url: &str) -> String {
    let host = url.split("://").nth(1).unwrap_or(url).split(['/', ':']).next().unwrap_or("repo");
    let mut parts: Vec<&str> = host.split('.').filter(|p| !p.is_empty()).collect();
    while parts.len() > 2 && ["www", "apt", "repo", "repos", "packages", "pkg", "download", "downloads", "deb"].contains(&parts[0]) {
        parts.remove(0);
    }
    if parts.len() > 1 {
        parts.pop();
    }
    let name: String = parts.join("-").to_ascii_lowercase().chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    if name.is_empty() { "repo".into() } else { name }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(url: &str, suite: &str, comps: &[&str], arch: Option<&str>, signed: Option<SignedBy>) -> Described {
        Described { url: url.into(), suite: suite.into(), components: comps.iter().map(|s| s.to_string()).collect(), arch: arch.map(String::from), signed_by: signed }
    }

    #[test]
    fn reads_one_line_entries() {
        let cases: &[(&str, Described)] = &[
            ("deb https://apt.example.com/app/stable stable main", d("https://apt.example.com/app/stable", "stable", &["main"], None, None)),
            (
                "deb [arch=amd64,arm64 signed-by=/usr/share/keyrings/x.gpg] https://x.example/apt noble main contrib # comment",
                d("https://x.example/apt", "noble", &["main", "contrib"], Some("amd64"), Some(SignedBy::Path("/usr/share/keyrings/x.gpg".into()))),
            ),
            ("deb https://x.example/flat ./", d("https://x.example/flat", "./", &[], None, None)),
        ];
        for (line, want) in cases {
            assert_eq!(parse(line).unwrap(), [want.clone()], "{line}");
        }
        let two = parse("# vendor repo\ndeb-src https://x.example/apt stable main\ndeb https://x.example/apt stable main\n\ndeb https://y.example/apt stable main\n").unwrap();
        assert_eq!(two.len(), 2);
    }

    #[test]
    fn refuses_what_is_not_a_repository() {
        let cases = [
            ("", "no 'deb' repository"),
            ("deb https://x.example/apt", "URL and a suite"),
            ("deb https://x.example/apt stable", "no components"),
            ("deb https://x.example/flat ./ main", "flat repository"),
            ("deb ftp://x.example/apt stable main", "http and https"),
            ("deb [arch=amd64 https://x.example stable main", "unclosed"),
            ("hello", "not an apt line"),
        ];
        for (text, want) in cases {
            let err = parse(text).unwrap_err().to_string();
            assert!(err.contains(want), "{text}: {err}");
        }
    }

    #[test]
    fn reads_deb822_sources() {
        let text = "Types: deb deb-src\nURIs: https://x.example/apt\nSuites: stable beta\nComponents: main\nArchitectures: amd64\n\
                    Signed-By:\n -----BEGIN PGP PUBLIC KEY BLOCK-----\n .\n mQINBGH\n -----END PGP PUBLIC KEY BLOCK-----\n\n\
                    Types: deb\nURIs: https://off.example/apt\nSuites: stable\nComponents: main\nEnabled: no\n";
        let got = parse(text).unwrap();
        assert_eq!(got.len(), 2, "two suites, the disabled entry skipped");
        assert_eq!((got[0].suite.as_str(), got[1].suite.as_str()), ("stable", "beta"));
        assert_eq!(got[0].arch.as_deref(), Some("amd64"));
        let Some(SignedBy::Inline(key)) = &got[0].signed_by else { panic!("inline key expected") };
        assert_eq!(key, "-----BEGIN PGP PUBLIC KEY BLOCK-----\n\nmQINBGH\n-----END PGP PUBLIC KEY BLOCK-----");
    }

    #[test]
    fn suggests_names() {
        let cases = [
            ("https://apt.example.com/app/stable", "example"),
            ("https://packages.microsoft.com/repos/code", "microsoft"),
            ("https://apt.example.org/", "example"),
            ("https://repo.steampowered.com/steam/", "steampowered"),
            ("http://localhost:8080/apt", "localhost"),
            ("https://dl.google.com/linux/chrome/deb/", "dl-google"),
        ];
        for (url, want) in cases {
            assert_eq!(suggest_name(url), want, "{url}");
        }
    }
}
