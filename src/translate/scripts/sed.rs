// SPDX-License-Identifier: AGPL-3.0-or-later
//! sed's `s` command, the one edit maintainer scripts make with sed: on a word in a
//! command substitution (`echo $icon | sed 's/[^0-9]//g'`) or on a file the package
//! ships (`sed -i 's|pkill|/usr/bin/pkill|g' unit.service`). Anything more is refused.

use regex::Regex;

#[derive(Debug, Clone)]
pub struct Substitution {
    re: Regex,
    replacement: String,
    global: bool,
}

impl PartialEq for Substitution {
    fn eq(&self, other: &Self) -> bool {
        self.re.as_str() == other.re.as_str() && self.replacement == other.replacement && self.global == other.global
    }
}

impl Eq for Substitution {}

impl Substitution {
    /// Reads `s<d>regex<d>replacement<d>flags`. `extended` is sed -E or -r.
    pub fn parse(expr: &str, extended: bool) -> Option<Substitution> {
        let mut chars = expr.strip_prefix('s')?.chars();
        let delim = chars.next().filter(|c| !c.is_alphanumeric() && *c != '\\' && *c != '\n')?;
        let rest: String = chars.collect();
        let parts = split_unescaped(&rest, delim)?;
        let [pattern, replacement, flags] = parts.as_slice() else {
            return None;
        };
        let mut global = false;
        let mut insensitive = false;
        for f in flags.chars() {
            match f {
                'g' => global = true,
                'I' | 'i' => insensitive = true,
                _ => return None,
            }
        }
        let mut rx = if extended { pattern.clone() } else { basic_to_extended(pattern)? };
        if insensitive {
            rx = format!("(?i){rx}");
        }
        Some(Substitution { re: Regex::new(&rx).ok()?, replacement: replacement_to_rust(replacement)?, global })
    }

    /// Applies the substitution to each line, as sed does.
    pub fn apply(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let changed = if self.global { self.re.replace_all(line, self.replacement.as_str()) } else { self.re.replace(line, self.replacement.as_str()) };
            out.push_str(&changed);
        }
        out
    }
}

/// Splits on `delim` where it is not escaped; an escaped delimiter becomes plain.
fn split_unescaped(s: &str, delim: char) -> Option<Vec<String>> {
    let mut parts = vec![String::new()];
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let n = chars.next()?;
            if n != delim {
                parts.last_mut()?.push('\\');
            }
            parts.last_mut()?.push(n);
        } else if c == delim {
            parts.push(String::new());
        } else {
            parts.last_mut()?.push(c);
        }
    }
    Some(parts)
}

/// POSIX basic regex to the extended form the regex crate reads: `\(`, `\)`, `\{`, `\}`
/// and `\|` are operators, while bare `( ) { } | + ?` are plain characters. Bracket
/// expressions pass through unchanged.
fn basic_to_extended(p: &str) -> Option<String> {
    let mut out = String::new();
    let chars: Vec<char> = p.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '[' => {
                // Copy the bracket expression whole; a ']' right after '[' or '[^' is literal.
                let start = i;
                i += 1;
                if chars.get(i) == Some(&'^') {
                    i += 1;
                }
                if chars.get(i) == Some(&']') {
                    i += 1;
                }
                while i < chars.len() && chars[i] != ']' {
                    if chars[i] == '[' && matches!(chars.get(i + 1), Some(':' | '.' | '=')) {
                        let close = chars[i + 2..].iter().position(|c| *c == ']')? + i + 2;
                        i = close + 1;
                    } else {
                        i += 1;
                    }
                }
                if i >= chars.len() {
                    return None;
                }
                out.extend(&chars[start..=i]);
                i += 1;
                continue;
            }
            '\\' => {
                let n = *chars.get(i + 1)?;
                match n {
                    '(' | ')' | '{' | '}' | '|' => out.push(n),
                    '+' | '?' => out.push(n),
                    '.' | '*' | '[' | ']' | '^' | '$' | '\\' | '/' => {
                        out.push('\\');
                        out.push(n);
                    }
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    _ if n.is_ascii_digit() => return None,
                    _ => out.push_str(&regex::escape(&n.to_string())),
                }
                i += 2;
                continue;
            }
            '(' | ')' | '{' | '}' | '|' | '+' | '?' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
        i += 1;
    }
    Some(out)
}

/// sed's replacement (`&`, `\1`..`\9`, escapes) in the regex crate's syntax.
fn replacement_to_rust(r: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = r.chars();
    while let Some(c) = chars.next() {
        match c {
            '&' => out.push_str("${0}"),
            '$' => out.push_str("$$"),
            '\\' => match chars.next()? {
                d if d.is_ascii_digit() => out.push_str(&format!("${{{d}}}")),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                '$' => out.push_str("$$"),
                other => out.push(other),
            },
            _ => out.push(c),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutes_like_sed() {
        let cases: &[(&str, bool, &str, Option<&str>)] = &[
            ("s/[^0-9]//g", false, "product_logo_256.png", Some("256")),
            ("s|pkill|/usr/bin/pkill|g", false, "ExecStop=pkill -f x; pkill y", Some("ExecStop=/usr/bin/pkill -f x; /usr/bin/pkill y")),
            ("s/a/b/", false, "aaa", Some("baa")),
            ("s/^/unix-user:/", false, "jeff", Some("unix-user:jeff")),
            ("s/\\(.*\\)\\.png/\\1/", false, "icon.png", Some("icon")),
            ("s/(x)+/[&]/g", false, "(x)+ xx", Some("[(x)+] xx")),
            ("s/(x)+/[&]/g", true, "(x)+ xx", Some("([x])+ [xx]")),
            ("s/A/b/gI", false, "aA", Some("bb")),
            ("s/a\\/b/c/", false, "a/b", Some("c")),
            ("s/x/$HOME/", false, "x", Some("$HOME")),
            ("s/x/y/w out", false, "x", None),
            ("s/x/y", false, "x", None),
            ("y/abc/xyz/", false, "abc", None),
        ];
        for (expr, ext, input, want) in cases {
            let got = Substitution::parse(expr, *ext).map(|s| s.apply(input));
            assert_eq!(got.as_deref(), *want, "{expr} on {input}");
        }
        // Each line is edited on its own.
        let s = Substitution::parse("s/^a/b/", false).unwrap();
        assert_eq!(s.apply("a1\na2"), "b1\nb2");
    }
}
