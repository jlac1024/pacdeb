// SPDX-License-Identifier: AGPL-3.0-or-later
//! The shell reading the analyzer needs: lines, words, quotes, expansions and heredocs.

use std::collections::HashMap;

use super::*;

/// Detects `name() {`, `name()` and `function name {`. Returns the name and, for a
/// one line function, its body text.
pub(super) fn function_start(line: &str) -> Option<(String, Option<String>)> {
    let t = line.trim();
    let (name, rest) = if let Some(r) = t.strip_prefix("function ") {
        let r = r.trim_start();
        let end = r.find(|c: char| c.is_whitespace() || c == '(' || c == '{').unwrap_or(r.len());
        let rest = r[end..].trim_start();
        (&r[..end], rest.strip_prefix("()").unwrap_or(rest))
    } else {
        let (name, rest) = t.split_once('(')?;
        let name = name.trim_end();
        (name, rest.trim_start().strip_prefix(')')?)
    };
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    let rest = rest.trim();
    match rest.strip_prefix('{') {
        Some(inner) if inner.trim_end().ends_with('}') => {
            let inner = inner.trim_end();
            Some((name.to_string(), Some(inner[..inner.len() - 1].trim().to_string())))
        }
        Some(inner) if inner.trim().is_empty() => Some((name.to_string(), None)),
        None if rest.is_empty() => Some((name.to_string(), None)),
        _ => None,
    }
}

/// Collects a multi line function body starting at `start`, skipping a `{` on its own
/// line. Returns the body and the index after the closing `}`.
pub(super) fn function_body(lines: &[Line], mut start: usize) -> (Vec<Line>, usize) {
    let mut depth = 1;
    if lines.get(start).is_some_and(|l| l.text.trim() == "{") && !lines[start - 1].text.trim_end().ends_with('{') {
        start += 1;
    }
    let mut body = Vec::new();
    let mut i = start;
    while i < lines.len() {
        let words: Vec<String> = split_commands(tokenize(&lines[i].text, &HashMap::new()))
            .into_iter()
            .flat_map(|c| c.words)
            .collect();
        depth += words.iter().filter(|w| *w == "{").count();
        depth -= words.iter().filter(|w| *w == "}").count().min(depth);
        i += 1;
        if depth == 0 {
            break;
        }
        body.push(lines[i - 1].clone());
    }
    (body, i)
}

/// Splits `configure|abort-upgrade) rest` into its patterns and the rest.
pub(super) fn split_case_pattern(line: &str) -> Option<(Vec<String>, &str)> {
    let i = line.find(')')?;
    let pattern = line[..i].trim().trim_start_matches('(');
    let ok = !pattern.is_empty() && pattern.chars().all(|c| c.is_ascii_alphanumeric() || "|*-_.\"' ".contains(c));
    if !ok {
        return None;
    }
    let patterns = pattern.split('|').map(|p| p.trim().trim_matches(['"', '\'']).to_string()).collect();
    Some((patterns, line[i + 1..].trim()))
}

/// Shell glob with only `*`, which is all case patterns in maintainer scripts use.
pub(super) fn glob_match(pattern: &str, word: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == word,
        Some((head, tail)) => {
            word.len() >= head.len() + tail.len() && word.starts_with(head) && {
                let rest = &word[head.len()..];
                (0..=rest.len()).any(|i| rest.is_char_boundary(i) && glob_match(tail, &rest[i..]))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Heredoc {
    pub(super) body: Vec<String>,
    /// False for a quoted delimiter, which turns off expansion.
    pub(super) expand: bool,
}

#[derive(Debug, Clone)]
pub(super) struct Line {
    pub(super) number: usize,
    pub(super) text: String,
    pub(super) heredoc: Option<Heredoc>,
}

/// Joins backslash continuations and attaches heredoc bodies to the line that opens them.
pub(super) fn logical_lines(text: &str) -> Vec<Line> {
    let raw: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let number = i + 1;
        let mut s = raw[i].to_string();
        i += 1;
        loop {
            if s.ends_with('\\') && i < raw.len() {
                s.pop();
                s.push(' ');
                s.push_str(raw[i].trim_start());
                i += 1;
            } else if unclosed_quote(&s) && i < raw.len() {
                // A quoted string that spans lines, like a multi line message.
                s.push('\n');
                s.push_str(raw[i]);
                i += 1;
            } else {
                break;
            }
        }
        let mut heredoc = None;
        if let Some((delim, quoted, strip_tabs)) = heredoc_delimiter(&s) {
            let mut body = Vec::new();
            while i < raw.len() {
                let l = raw[i];
                i += 1;
                let cmp = if strip_tabs { l.trim_start_matches('\t') } else { l };
                if cmp == delim {
                    break;
                }
                body.push(cmp.to_string());
            }
            heredoc = Some(Heredoc { body, expand: !quoted });
        }
        out.push(Line { number, text: s, heredoc });
    }
    out
}

/// Whether a line ends inside a single or double quoted string.
pub(super) fn unclosed_quote(s: &str) -> bool {
    let mut quote: Option<char> = None;
    let mut chars = s.chars().peekable();
    let mut word_start = true;
    while let Some(c) = chars.next() {
        match quote {
            Some('\'') if c == '\'' => quote = None,
            Some('"') if c == '\\' => {
                chars.next();
            }
            Some('"') if c == '"' => quote = None,
            Some(_) => {}
            None => match c {
                '\\' => {
                    chars.next();
                }
                '\'' | '"' => quote = Some(c),
                '#' if word_start => return false,
                _ => {}
            },
        }
        word_start = c.is_whitespace() || c == ';';
    }
    quote.is_some()
}

/// Finds `<<WORD`, `<<'WORD'` or `<<-WORD`: (delimiter, quoted, strip leading tabs).
pub(super) fn heredoc_delimiter(s: &str) -> Option<(String, bool, bool)> {
    let mut quote = None;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c == '#' && (i == 0 || chars[i - 1].is_whitespace()) => return None,
            None if c == '<'
                && chars.get(i + 1) == Some(&'<')
                && chars.get(i + 2) != Some(&'<')
                && (i == 0 || chars[i - 1] != '<') =>
            {
                let mut j = i + 2;
                let strip_tabs = chars.get(j) == Some(&'-');
                if strip_tabs {
                    j += 1;
                }
                while chars.get(j).is_some_and(|c| c.is_whitespace()) {
                    j += 1;
                }
                let raw: String = chars[j..].iter().take_while(|c| !c.is_whitespace() && !";&|<>".contains(**c)).collect();
                let quoted = raw.contains(['\'', '"', '\\']);
                let word: String = raw.chars().filter(|c| !"'\"\\".contains(*c)).collect();
                return (!word.is_empty()).then_some((word, quoted, strip_tabs));
            }
            None => {}
        }
        i += 1;
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sep {
    End,
    Semi,
    And,
    Or,
    Pipe,
    Background,
}

#[derive(Debug, PartialEq)]
pub(super) enum Tok {
    Word(String),
    Op(Sep),
    Redirect(String),
}

#[derive(Debug, Clone)]
pub(super) struct Cmd {
    pub(super) words: Vec<String>,
    pub(super) redirects: Vec<String>,
    /// The separator after this command.
    pub(super) sep: Sep,
}

impl Cmd {
    pub(super) fn text(&self) -> String {
        self.words.iter().map(|w| if w.is_empty() { "\"\"" } else { w.as_str() }).collect::<Vec<_>>().join(" ")
    }
}

/// Describes one test command in plain words, such as "/etc/foo exists".
pub(super) fn humanize(words: &[String]) -> String {
    let w: Vec<&str> = words.iter().map(String::as_str).collect();
    match w.as_slice() {
        ["!", ..] => format!("not ({})", humanize(&words[1..])),
        ["[" | "[[", inner @ .., "]" | "]]"] | ["test", inner @ ..] => bracket_words(inner),
        ["command", "-v", x] | ["which", x] | ["hash", x] | ["type", x] => format!("{x} is installed"),
        _ => format!("'{}' succeeds", w.join(" ")),
    }
}

pub(super) fn bracket_words(a: &[&str]) -> String {
    const FILE_OPS: [&str; 9] = ["-e", "-f", "-x", "-d", "-L", "-h", "-r", "-s", "-w"];
    match a {
        ["!", op, p] if FILE_OPS.contains(op) => file_test(op, p, true),
        [op, p] if FILE_OPS.contains(op) => file_test(op, p, false),
        ["-z", s] => format!("'{s}' is empty"),
        ["-n", s] => format!("'{s}' is not empty"),
        [x, "=" | "==", y] => match readlink_of(x) {
            Some(link) => format!("{link} points to {y}"),
            None => format!("{x} is '{y}'"),
        },
        [x, "!=", y] => format!("{x} is not '{y}'"),
        _ => format!("[ {} ]", a.join(" ")),
    }
}

pub(super) fn file_test(op: &str, path: &str, negated: bool) -> String {
    let (yes, no) = match op {
        "-d" => ("is a directory", "is not a directory"),
        "-L" | "-h" => ("is a symlink", "is not a symlink"),
        "-x" => ("is executable", "is not executable"),
        "-s" => ("is not empty", "is empty or missing"),
        "-r" => ("is readable", "is not readable"),
        "-w" => ("is writable", "is not writable"),
        _ => ("exists", "does not exist"),
    };
    format!("{path} {}", if negated { no } else { yes })
}

/// The path in `$(readlink <path>)`, as left by `expand` for display.
pub(super) fn readlink_of(word: &str) -> Option<&str> {
    Some(word.strip_prefix("$(readlink ")?.strip_suffix(')')?.trim())
}

pub(super) fn split_commands(toks: Vec<Tok>) -> Vec<Cmd> {
    let new = || Cmd { words: Vec::new(), redirects: Vec::new(), sep: Sep::End };
    let mut out = vec![new()];
    for t in toks {
        match t {
            Tok::Word(w) => out.last_mut().unwrap().words.push(w),
            Tok::Redirect(r) => out.last_mut().unwrap().redirects.push(r),
            Tok::Op(sep) => {
                out.last_mut().unwrap().sep = sep;
                out.push(new());
            }
        }
    }
    out.retain(|c| !c.words.is_empty() || !c.redirects.is_empty());
    out
}

/// Splits a line into words, separators and redirections. Quotes are removed, known
/// variables substituted, unknown expansions left as written.
pub(super) fn tokenize(s: &str, vars: &HashMap<String, String>) -> Vec<Tok> {
    let chars: Vec<char> = s.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    // Inside `[[ ... ]]`, && and || combine tests instead of separating commands.
    let mut double_bracket = false;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if double_bracket && ((c == '&' && next == Some('&')) || (c == '|' && next == Some('|'))) {
            toks.push(Tok::Word(format!("{c}{c}")));
            i += 2;
        } else if c.is_whitespace() {
            i += 1;
        } else if c == '#' {
            break;
        } else if c == ';' && next == Some(';') {
            // ";;" ends a case branch; keep it as its own command so the analyzer sees it.
            toks.push(Tok::Op(Sep::Semi));
            toks.push(Tok::Word(";;".into()));
            toks.push(Tok::Op(Sep::Semi));
            i += 2;
        } else if c == ';' {
            toks.push(Tok::Op(Sep::Semi));
            i += 1;
        } else if c == '&' && next == Some('&') {
            toks.push(Tok::Op(Sep::And));
            i += 2;
        } else if c == '|' && next == Some('|') {
            toks.push(Tok::Op(Sep::Or));
            i += 2;
        } else if c == '|' {
            toks.push(Tok::Op(Sep::Pipe));
            i += 1;
        } else if c == '&' && next != Some('>') {
            toks.push(Tok::Op(Sep::Background));
            i += 1;
        } else if c == '>' || c == '<' || c == '&' {
            i = redirect(&chars, i, vars, &mut toks);
        } else {
            let (word, next_i) = read_word(&chars, i, vars);
            i = next_i;
            if chars.get(i).is_some_and(|c| *c == '>' || *c == '<') && !word.is_empty() && word.chars().all(|c| c.is_ascii_digit()) {
                // "2>/dev/null": the digits name a file descriptor.
                i = redirect(&chars, i, vars, &mut toks);
            } else {
                match word.as_str() {
                    "[[" => double_bracket = true,
                    "]]" => double_bracket = false,
                    _ => {}
                }
                toks.push(Tok::Word(word));
            }
        }
    }
    toks
}

pub(super) fn redirect(chars: &[char], mut i: usize, vars: &HashMap<String, String>, toks: &mut Vec<Tok>) -> usize {
    let heredoc = chars.get(i) == Some(&'<') && chars.get(i + 1) == Some(&'<');
    while chars.get(i).is_some_and(|c| "<>&|-".contains(*c)) {
        i += 1;
    }
    while chars.get(i).is_some_and(|c| c.is_whitespace()) {
        i += 1;
    }
    let (target, next) = read_word(chars, i, vars);
    // A heredoc's body is input, not a file the command writes.
    if !heredoc {
        toks.push(Tok::Redirect(target));
    }
    next
}

pub(super) fn read_word(chars: &[char], mut i: usize, vars: &HashMap<String, String>) -> (String, usize) {
    let mut word = String::new();
    while let Some(&c) = chars.get(i) {
        match c {
            c if c.is_whitespace() || ";&|<>".contains(c) => break,
            '\'' => {
                i += 1;
                while let Some(&c) = chars.get(i) {
                    i += 1;
                    if c == '\'' {
                        break;
                    }
                    word.push(c);
                }
            }
            '"' => {
                i += 1;
                while let Some(&c) = chars.get(i) {
                    match c {
                        '"' => {
                            i += 1;
                            break;
                        }
                        '\\' if chars.get(i + 1).is_some_and(|n| "\"\\$`".contains(*n)) => {
                            word.push(chars[i + 1]);
                            i += 2;
                        }
                        '$' => i = expand(chars, i, vars, &mut word),
                        '`' => i = backticks(chars, i, vars, &mut word),
                        c => {
                            word.push(c);
                            i += 1;
                        }
                    }
                }
            }
            '\\' => {
                if let Some(&n) = chars.get(i + 1) {
                    word.push(n);
                }
                i += 2;
            }
            '$' => i = expand(chars, i, vars, &mut word),
            '`' => i = backticks(chars, i, vars, &mut word),
            c => {
                word.push(c);
                i += 1;
            }
        }
    }
    (word, i)
}

/// Handles `$NAME`, `$1`, `${NAME}`, `${NAME:-default}`, `$(dirname <path>)` and other
/// `$(...)` at `chars[i]`. Known values are substituted; everything else is copied as
/// written so later checks see an unresolved expansion.
pub(super) fn expand(chars: &[char], i: usize, vars: &HashMap<String, String>, word: &mut String) -> usize {
    match chars.get(i + 1) {
        Some('(') => {
            let mut depth = 0;
            let mut j = i + 1;
            while let Some(&c) = chars.get(j) {
                j += 1;
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let inner: String = chars[(i + 2).min(j)..j.saturating_sub(1).max(i + 2)].iter().collect();
            match substitute(&inner, vars) {
                Some(value) => word.push_str(&value),
                None => {
                    // Still unresolved (it keeps the "$("), but with known variables
                    // filled in so messages show real paths.
                    let shown: Vec<String> = tokenize(&inner, vars)
                        .into_iter()
                        .filter_map(|t| match t {
                            Tok::Word(w) => Some(w),
                            _ => None,
                        })
                        .collect();
                    word.push_str(&format!("$({})", shown.join(" ")));
                }
            }
            j
        }
        Some('{') => {
            let Some(close) = chars[i..].iter().position(|c| *c == '}') else {
                word.extend(&chars[i..]);
                return chars.len();
            };
            let inner: String = chars[i + 2..i + close].iter().collect();
            match parameter(&inner, vars) {
                Some(v) => word.push_str(&v),
                None => word.extend(&chars[i..=i + close]),
            }
            i + close + 1
        }
        Some(c) if c.is_ascii_alphabetic() || *c == '_' => {
            let len = chars[i + 1..].iter().take_while(|c| c.is_ascii_alphanumeric() || **c == '_').count();
            let name: String = chars[i + 1..i + 1 + len].iter().collect();
            match vars.get(&name) {
                Some(v) => word.push_str(v),
                None => word.extend(&chars[i..i + 1 + len]),
            }
            i + 1 + len
        }
        Some(c) if c.is_ascii_digit() => {
            match vars.get(&c.to_string()) {
                Some(v) => word.push_str(v),
                None => word.extend(&chars[i..i + 2]),
            }
            i + 2
        }
        // $@ and $* are the script's (or function's) arguments, which dpkg sets.
        Some('@' | '*') if vars.contains_key("1") => {
            let args: Vec<&str> = (1..=9).filter_map(|n| vars.get(&n.to_string())).map(String::as_str).filter(|a| !a.is_empty()).collect();
            word.push_str(&args.join(" "));
            i + 2
        }
        // Other special parameters such as $? and $$ stay as written, unresolved.
        Some(c) if "@*#?!$-".contains(*c) => {
            word.push('$');
            word.push(*c);
            i + 2
        }
        // Any other `$` is a plain character.
        Some(_) => {
            word.push('$');
            i + 1
        }
        None => {
            word.push('$');
            i + 1
        }
    }
}

/// `${NAME}`, `${NAME:-default}`, `${NAME-default}` and the pattern removals
/// `${NAME%pat}`, `${NAME%%pat}`, `${NAME#pat}` and `${NAME##pat}`. None when the value
/// is not known, so the expansion stays as written.
fn parameter(inner: &str, vars: &HashMap<String, String>) -> Option<String> {
    let len = inner.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').count();
    let (name, op) = inner.split_at(len);
    let value = vars.get(name)?;
    if op.is_empty() {
        return Some(value.clone());
    }
    if let Some(default) = op.strip_prefix(":-").or_else(|| op.strip_prefix('-')) {
        // Unknown variables never get here, so only the empty case needs the default.
        return Some(if value.is_empty() && op.starts_with(':') && !unresolved(default) { default.to_string() } else { value.clone() });
    }
    let (strip, pattern) = ["%%", "##", "%", "#"].iter().find_map(|o| op.strip_prefix(o).map(|p| (*o, p)))?;
    if unresolved(pattern) || unresolved(value) {
        return None;
    }
    let cuts: Vec<usize> = value.char_indices().map(|(i, _)| i).chain([value.len()]).collect();
    let found = match strip {
        "#" => cuts.iter().find(|&&c| pattern_match(pattern, &value[..c])).map(|&c| value[c..].to_string()),
        "##" => cuts.iter().rev().find(|&&c| pattern_match(pattern, &value[..c])).map(|&c| value[c..].to_string()),
        "%" => cuts.iter().rev().find(|&&c| pattern_match(pattern, &value[c..])).map(|&c| value[..c].to_string()),
        _ => cuts.iter().find(|&&c| pattern_match(pattern, &value[c..])).map(|&c| value[..c].to_string()),
    };
    Some(found.unwrap_or_else(|| value.clone()))
}

/// Shell pattern matching with `*` and `?`, as in `${var%.png}` and file globs.
pub(super) fn pattern_match(pattern: &str, s: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), s.chars().collect());
    // Classic two pointer match with backtracking to the last '*'.
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// A backquoted command substitution starting at `chars[i]`; returns the index after it.
fn backticks(chars: &[char], i: usize, vars: &HashMap<String, String>, word: &mut String) -> usize {
    let Some(len) = chars[i + 1..].iter().position(|c| *c == '`') else {
        word.extend(&chars[i..]);
        return chars.len();
    };
    let inner: String = chars[i + 1..i + 1 + len].iter().collect();
    match substitute(&inner, vars) {
        Some(value) => word.push_str(&value),
        None => word.extend(&chars[i..i + 2 + len]),
    }
    i + 2 + len
}

/// Evaluates a command substitution the analyzer knows the answer to on Arch: where a
/// tool is (`command -v`, `which`), who runs the script (`id -u`: root), the folder
/// (`pwd`), a user or group lookup (`getent`: not there yet on a first install), the
/// init system (systemd), `dirname`/`basename` of a path, and `echo WORD | sed s/...`.
/// None for anything else, which then stays unresolved.
pub(super) fn substitute(inner: &str, vars: &HashMap<String, String>) -> Option<String> {
    let text = inner.trim();
    if system::probes_init_system(text) {
        return Some("systemd".into());
    }
    let cmds = split_commands(tokenize(text, vars));
    let first = cmds.first()?;
    let words: Vec<&str> = first.words.iter().map(String::as_str).collect();
    // `cmd || true` and `cmd || :` only matter when cmd fails, which a known answer rules out.
    let alone = cmds.len() == 1
        || (cmds.len() == 2 && first.sep == Sep::Or && matches!(cmds[1].words.as_slice(), [w] if w == "true" || w == ":"));
    if cmds.len() == 2 && first.sep == Sep::Pipe {
        return echo_sed(&words, &cmds[1].words);
    }
    if !alone {
        return None;
    }
    match words.as_slice() {
        ["command", "-v", tool] | ["which", tool] | ["type", "-p" | "-P", tool] if !unresolved(tool) => system::tool_path(tool),
        ["id", "-u" | "-g" | "-ru" | "-rg"] => Some("0".into()),
        ["whoami"] | ["id", "-un"] => Some("root".into()),
        ["pwd"] => vars.get("PWD").cloned(),
        ["getent", "group" | "passwd", name] if !unresolved(name) => Some(String::new()),
        ["dirname", p] if p.starts_with('/') && !unresolved(p) => Some(parent(&resolve("/", p)).to_string()),
        ["basename", p] if !unresolved(p) => Some(basename(p.trim_end_matches('/')).to_string()),
        _ => None,
    }
}

/// `echo WORDS | sed [-E] [-e] 's/.../.../'` on literal words.
fn echo_sed(echo: &[&str], sed: &[String]) -> Option<String> {
    let ["echo", input @ ..] = echo else {
        return None;
    };
    if input.iter().any(|w| unresolved(w) || w.starts_with('-')) {
        return None;
    }
    let mut extended = false;
    let mut expr = None;
    for w in sed.iter().skip(1) {
        match w.as_str() {
            "-E" | "-r" => extended = true,
            "-e" => {}
            e if expr.is_none() && !e.starts_with('-') => expr = Some(e),
            _ => return None,
        }
    }
    (sed.first()? == "sed").then_some(())?;
    Some(super::sed::Substitution::parse(expr?, extended)?.apply(&input.join(" ")))
}
