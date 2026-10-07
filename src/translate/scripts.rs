//! Reads Debian maintainer scripts without running them. Each command is sorted into
//! something Ferry turns into package contents, something a pacman hook already does,
//! something that needs nothing on Arch, or something a person has to look at.
//!
//! This is not a shell parser. It understands the plain command lines Debian scripts
//! are made of; anything it cannot follow is reported, never guessed at.

use std::collections::{HashMap, HashSet};

use super::fs::mentions_apt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Symlink { link: String, target: String },
    Chmod { path: String, mode: u32 },
    /// An `rm` in a removal script. Fine when the path belongs to the package.
    RemoveOwned { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Actions(Vec<Action>),
    /// A pacman hook does this already; the value names it.
    Hook(&'static str),
    /// Nothing to do on Arch; the value says why.
    Handled(&'static str),
    AptRepo,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub script: String,
    pub line: usize,
    pub text: String,
    pub outcome: Outcome,
}

const HOOKS: [(&str, &str); 6] = [
    ("update-desktop-database", "desktop database"),
    ("gtk-update-icon-cache", "icon cache"),
    ("update-icon-caches", "icon cache"),
    ("update-mime-database", "MIME database"),
    ("ldconfig", "linker cache"),
    ("glib-compile-schemas", "GSettings schemas"),
];

/// Shell builtins and test commands that do not change the system by themselves.
const NEUTRAL: [&str; 24] = [
    "[", "[[", "test", "true", "false", ":", "exit", "return", "set", "command", "which", "type",
    "hash", "local", "readonly", "shift", "break", "continue", "unset", "trap", "umask", "wait",
    "sleep", "for",
];

const REMOVAL_SCRIPTS: [&str; 2] = ["prerm", "postrm"];

pub fn analyze(script: &str, text: &str) -> Vec<Command> {
    let mut state = State::default();
    let mut out = Vec::new();
    for line in logical_lines(text) {
        let mut body = line.text.trim();
        if state.case_depth > 0 {
            body = strip_case_pattern(body);
        }
        let shown = if line.heredoc_lines > 0 {
            format!("{} (plus {} line heredoc)", line.text.trim(), line.heredoc_lines)
        } else {
            line.text.trim().to_string()
        };
        if mentions_apt(body) {
            // Track case/esac and functions even on apt lines so later lines parse right.
            for cmd in split_commands(&tokenize(body, &state.vars)) {
                state.note_structure(&cmd.words);
            }
            out.push(Command { script: script.into(), line: line.number, text: shown, outcome: Outcome::AptRepo });
            continue;
        }
        for cmd in split_commands(&tokenize(body, &state.vars)) {
            if let Some(outcome) = state.classify(script, &cmd) {
                let text = if line.heredoc_lines > 0 { shown.clone() } else { cmd.words.join(" ") };
                out.push(Command { script: script.into(), line: line.number, text, outcome });
            }
        }
    }
    out
}

#[derive(Default)]
struct State {
    /// Variables assigned a plain literal earlier in the script.
    vars: HashMap<String, String>,
    functions: HashSet<String>,
    case_depth: usize,
}

impl State {
    /// Updates case nesting and the function list. Returns true for words that are pure
    /// shell structure.
    fn note_structure(&mut self, words: &[String]) -> bool {
        let Some(first) = words.first() else {
            return true;
        };
        match first.as_str() {
            "case" => {
                self.case_depth += 1;
                return true;
            }
            "esac" => {
                self.case_depth = self.case_depth.saturating_sub(1);
                return true;
            }
            "fi" | "done" | "}" | ")" | ";;" | "in" | "function" => {
                if first == "function" {
                    if let Some(name) = words.get(1) {
                        self.functions.insert(name.trim_end_matches("()").to_string());
                    }
                }
                return true;
            }
            _ => {}
        }
        if let Some(name) = first.strip_suffix("()") {
            self.functions.insert(name.to_string());
            return true;
        }
        if words.get(1).is_some_and(|w| w == "()" || w.starts_with("()")) {
            self.functions.insert(first.clone());
            return true;
        }
        false
    }

    /// None for lines that are only structure and need no report.
    fn classify(&mut self, script: &str, cmd: &Cmd) -> Option<Outcome> {
        let mut words: &[String] = &cmd.words;
        while let Some(first) = words.first() {
            if matches!(first.as_str(), "if" | "then" | "else" | "elif" | "do" | "while" | "until" | "!" | "{" | "(") {
                words = &words[1..];
            } else {
                break;
            }
        }
        if self.note_structure(words) {
            return None;
        }

        // Leading assignments: remember literal ones, then look at the command after them.
        while let Some((name, value)) = words.first().and_then(|w| assignment(w)) {
            if !value.contains(['$', '`']) {
                self.vars.insert(name.to_string(), value.to_string());
            }
            words = &words[1..];
        }
        let first = words.first()?.as_str();
        let args = &words[1..];
        let writes_file = cmd.redirects.iter().any(|t| !is_harmless_redirect(t));

        let outcome = match first {
            "export" => {
                for w in args {
                    if let Some((name, value)) = assignment(w) {
                        if !value.contains(['$', '`']) {
                            self.vars.insert(name.to_string(), value.to_string());
                        }
                    }
                }
                return None;
            }
            "echo" | "printf" if !writes_file => return None,
            f if NEUTRAL.contains(&f) && !writes_file => return None,
            f if self.functions.contains(f) => return None,
            _ if writes_file => Outcome::Unknown(format!(
                "writes to {}",
                cmd.redirects.iter().find(|t| !is_harmless_redirect(t)).unwrap()
            )),
            "update-alternatives" => update_alternatives(args),
            "chmod" => chmod(args),
            "chown" | "chgrp" => chown(args),
            "ln" => ln(args),
            "rm" if REMOVAL_SCRIPTS.contains(&script) => rm(args),
            "xdg-icon-resource" if args.first().is_some_and(|a| a == "forceupdate") => Outcome::Hook("icon cache"),
            "xdg-desktop-menu" if args.first().is_some_and(|a| a == "forceupdate") => Outcome::Hook("desktop database"),
            "systemctl" if args.first().is_some_and(|a| a == "daemon-reload") => Outcome::Hook("systemd reload"),
            f => match HOOKS.iter().find(|(name, _)| *name == f) {
                Some((_, hook)) => Outcome::Hook(hook),
                None => Outcome::Unknown("no translation for this command".into()),
            },
        };
        Some(outcome)
    }
}

fn assignment(word: &str) -> Option<(&str, &str)> {
    let (name, value) = word.split_once('=')?;
    let valid = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then_some((name, value))
}

fn is_harmless_redirect(target: &str) -> bool {
    target == "/dev/null" || target == "-" || target.chars().all(|c| c.is_ascii_digit())
}

/// A literal absolute path: no expansions or globs left in it.
fn literal_path(s: &str) -> bool {
    s.starts_with('/') && !s.contains(['$', '`', '*', '?', '['])
}

fn update_alternatives(args: &[String]) -> Outcome {
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "--altdir" | "--admindir" | "--log" => i += 2,
            "--quiet" | "--verbose" | "--force" | "--skip-auto" | "--debug" => i += 1,
            _ => break,
        }
    }
    let Some(cmd) = args.get(i) else {
        return Outcome::Unknown("update-alternatives without a command".into());
    };
    let rest = &args[i + 1..];
    match cmd.as_str() {
        "--install" => {
            // --install <link> <name> <path> <priority> [--slave <link> <name> <path>]...
            let unreadable = || Outcome::Unknown("could not read the update-alternatives arguments".into());
            let [link, _name, path, _prio, slaves @ ..] = rest else {
                return unreadable();
            };
            let mut actions = vec![(link, path)];
            for chunk in slaves.chunks(4) {
                match chunk {
                    [flag, link, _name, path] if flag == "--slave" => actions.push((link, path)),
                    _ => return unreadable(),
                }
            }
            if actions.iter().any(|(l, p)| !literal_path(l) || !literal_path(p)) {
                return unreadable();
            }
            Outcome::Actions(
                actions
                    .into_iter()
                    .map(|(link, path)| Action::Symlink { link: link.clone(), target: path.clone() })
                    .collect(),
            )
        }
        "--remove" | "--remove-all" => Outcome::Handled("pacman removes the symlink with the package"),
        "--auto" | "--display" | "--query" | "--list" | "--get-selections" => {
            Outcome::Handled("only affects dpkg's alternatives database")
        }
        other => Outcome::Unknown(format!("update-alternatives {other} has no pacman equivalent")),
    }
}

fn chmod(args: &[String]) -> Outcome {
    let args: Vec<&String> = args.iter().filter(|a| *a != "--").collect();
    if args.iter().any(|a| a.starts_with('-')) {
        return Outcome::Unknown("chmod options are not translated".into());
    }
    let [mode, paths @ ..] = args.as_slice() else {
        return Outcome::Unknown("chmod without arguments".into());
    };
    let mode = match u32::from_str_radix(mode, 8) {
        Ok(m) if (3..=4).contains(&mode.len()) && m <= 0o7777 => m,
        _ => return Outcome::Unknown("only numeric chmod modes are translated".into()),
    };
    if paths.is_empty() || !paths.iter().all(|p| literal_path(p)) {
        return Outcome::Unknown("chmod on a path Ferry cannot resolve".into());
    }
    Outcome::Actions(paths.iter().map(|p| Action::Chmod { path: p.to_string(), mode }).collect())
}

fn chown(args: &[String]) -> Outcome {
    let owner = args.iter().find(|a| !a.starts_with('-'));
    let to_root = owner.is_some_and(|o| {
        o.split([':', '.']).all(|part| part.is_empty() || part == "root" || part == "0")
    });
    if to_root {
        Outcome::Handled("every file in the package is owned by root")
    } else {
        Outcome::Unknown("pacman packages hold root owned files; other owners need a manual step".into())
    }
}

fn ln(args: &[String]) -> Outcome {
    let mut symbolic = false;
    let mut rest = Vec::new();
    for a in args {
        match a.as_str() {
            "--symbolic" => symbolic = true,
            "--force" | "--no-dereference" | "--no-target-directory" | "--" => {}
            s if s.starts_with('-') && !s.starts_with("--") => {
                if !s[1..].chars().all(|c| "sfnT".contains(c)) {
                    return Outcome::Unknown(format!("ln option {s} is not translated"));
                }
                symbolic |= s.contains('s');
            }
            s if s.starts_with("--") => return Outcome::Unknown(format!("ln option {s} is not translated")),
            _ => rest.push(a),
        }
    }
    match rest.as_slice() {
        [target, link] if symbolic && literal_path(link) && !link.ends_with('/') && !target.contains(['$', '`']) => {
            Outcome::Actions(vec![Action::Symlink { link: link.to_string(), target: target.to_string() }])
        }
        _ => Outcome::Unknown("only 'ln -s <target> <absolute link>' is translated".into()),
    }
}

fn rm(args: &[String]) -> Outcome {
    let paths: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    if paths.is_empty() || !paths.iter().all(|p| literal_path(p)) {
        return Outcome::Unknown("rm on a path Ferry cannot resolve".into());
    }
    Outcome::Actions(paths.into_iter().map(|p| Action::RemoveOwned { path: p.clone() }).collect())
}

/// Drops a leading case pattern such as `configure)` or `abort-upgrade|abort-remove)`.
fn strip_case_pattern(line: &str) -> &str {
    let Some(i) = line.find(')') else {
        return line;
    };
    let pattern = &line[..i];
    let looks_like_pattern = !pattern.trim().is_empty()
        && pattern.chars().all(|c| c.is_ascii_alphanumeric() || "|*-_.\"' ".contains(c));
    if looks_like_pattern { line[i + 1..].trim() } else { line }
}

struct Line {
    number: usize,
    text: String,
    heredoc_lines: usize,
}

/// Joins backslash continuations and folds heredoc bodies into the line that opens them.
fn logical_lines(text: &str) -> Vec<Line> {
    let raw: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let number = i + 1;
        let mut s = raw[i].to_string();
        i += 1;
        while s.ends_with('\\') && i < raw.len() {
            s.pop();
            s.push(' ');
            s.push_str(raw[i].trim_start());
            i += 1;
        }
        let mut heredoc_lines = 0;
        if let Some(delim) = heredoc_delimiter(&s) {
            while i < raw.len() {
                let l = raw[i];
                i += 1;
                if l.trim() == delim {
                    break;
                }
                heredoc_lines += 1;
            }
        }
        out.push(Line { number, text: s, heredoc_lines });
    }
    out
}

fn heredoc_delimiter(s: &str) -> Option<String> {
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
                if chars.get(j) == Some(&'-') {
                    j += 1;
                }
                while chars.get(j).is_some_and(|c| c.is_whitespace()) {
                    j += 1;
                }
                let word: String = chars[j..]
                    .iter()
                    .take_while(|c| !c.is_whitespace() && !";&|<>".contains(**c))
                    .filter(|c| !"'\"\\".contains(**c))
                    .collect();
                return (!word.is_empty()).then_some(word);
            }
            None => {}
        }
        i += 1;
    }
    None
}

#[derive(Debug, PartialEq)]
enum Tok {
    Word(String),
    Op,
    Redirect(String),
}

#[derive(Debug, Default)]
struct Cmd {
    words: Vec<String>,
    redirects: Vec<String>,
}

fn split_commands(toks: &[Tok]) -> Vec<Cmd> {
    let mut out = vec![Cmd::default()];
    for t in toks {
        match t {
            Tok::Word(w) => out.last_mut().unwrap().words.push(w.clone()),
            Tok::Redirect(r) => out.last_mut().unwrap().redirects.push(r.clone()),
            Tok::Op => out.push(Cmd::default()),
        }
    }
    out.retain(|c| !c.words.is_empty() || !c.redirects.is_empty());
    out
}

/// Splits a line into words, command separators and redirections. Quotes are removed,
/// known variables are substituted, unknown expansions stay as written.
fn tokenize(s: &str, vars: &HashMap<String, String>) -> Vec<Tok> {
    let chars: Vec<char> = s.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '#' {
            break;
        } else if c == ';' || c == '|' || (c == '&' && chars.get(i + 1) != Some(&'>')) {
            // ";", ";;", "|", "||", "&", "&&" all end a command here.
            i += 1;
            if chars.get(i) == Some(&c) {
                i += 1;
            }
            toks.push(Tok::Op);
        } else if c == '>' || c == '<' || c == '&' {
            i = redirect(&chars, i, vars, &mut toks);
        } else {
            let (word, next) = read_word(&chars, i, vars);
            i = next;
            if chars.get(i).is_some_and(|c| *c == '>' || *c == '<') && word.chars().all(|c| c.is_ascii_digit()) {
                // "2>/dev/null": the digits name a file descriptor.
                i = redirect(&chars, i, vars, &mut toks);
            } else {
                toks.push(Tok::Word(word));
            }
        }
    }
    toks
}

fn redirect(chars: &[char], mut i: usize, vars: &HashMap<String, String>, toks: &mut Vec<Tok>) -> usize {
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

fn read_word(chars: &[char], mut i: usize, vars: &HashMap<String, String>) -> (String, usize) {
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
            '`' => {
                let end = chars[i + 1..].iter().position(|c| *c == '`').map_or(chars.len(), |p| i + 2 + p);
                word.extend(&chars[i..end]);
                i = end;
            }
            c => {
                word.push(c);
                i += 1;
            }
        }
    }
    (word, i)
}

/// Handles `$NAME`, `${NAME}` and `$(...)` at `chars[i]`. Known variables are replaced;
/// everything else is copied as is so later checks see an unresolved expansion.
fn expand(chars: &[char], i: usize, vars: &HashMap<String, String>, word: &mut String) -> usize {
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
            word.extend(&chars[i..j]);
            j
        }
        Some('{') => {
            let Some(close) = chars[i..].iter().position(|c| *c == '}') else {
                word.extend(&chars[i..]);
                return chars.len();
            };
            let name: String = chars[i + 2..i + close].iter().collect();
            match vars.get(&name) {
                Some(v) => word.push_str(v),
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
        Some(_) => {
            word.push('$');
            word.push(chars[i + 1]);
            i + 2
        }
        None => {
            word.push('$');
            i + 1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcomes(script: &str, text: &str) -> Vec<(usize, String, Outcome)> {
        analyze(script, text).into_iter().map(|c| (c.line, c.text, c.outcome)).collect()
    }

    fn unknown() -> Outcome {
        Outcome::Unknown(String::new())
    }

    /// Compares outcomes, ignoring the reason text of Unknown.
    fn same_kind(a: &Outcome, b: &Outcome) -> bool {
        match (a, b) {
            (Outcome::Unknown(_), Outcome::Unknown(_)) => true,
            _ => a == b,
        }
    }

    fn symlink(link: &str, target: &str) -> Action {
        Action::Symlink { link: link.into(), target: target.into() }
    }

    #[test]
    fn classifies_single_commands() {
        let cases: Vec<(&str, &str, Option<Outcome>)> = vec![
            ("postinst", "set -e", None),
            ("postinst", "exit 0", None),
            ("postinst", "# comment", None),
            ("postinst", "#DEBHELPER#", None),
            ("postinst", "echo done", None),
            ("postinst", "if [ \"$1\" = configure ]; then", None),
            ("postinst", "fi", None),
            ("postinst", "update-alternatives --install /usr/bin/app app /opt/App/app 100",
                Some(Outcome::Actions(vec![symlink("/usr/bin/app", "/opt/App/app")]))),
            ("postinst", "update-alternatives --quiet --install '/usr/bin/app' 'app' '/opt/App Name/app' 100 --slave /usr/share/man/man1/app.1.gz app.1.gz /opt/App/app.1.gz",
                Some(Outcome::Actions(vec![symlink("/usr/bin/app", "/opt/App Name/app"), symlink("/usr/share/man/man1/app.1.gz", "/opt/App/app.1.gz")]))),
            ("postinst", "update-alternatives --install /usr/bin/app app $DIR/app 100", Some(unknown())),
            ("postinst", "update-alternatives --install /usr/bin/app app", Some(unknown())),
            ("prerm", "update-alternatives --remove app /opt/App/app", Some(Outcome::Handled("pacman removes the symlink with the package"))),
            ("postinst", "update-alternatives --set app /opt/App/app", Some(unknown())),
            ("postinst", "chmod 4755 '/opt/App/chrome-sandbox' || true",
                Some(Outcome::Actions(vec![Action::Chmod { path: "/opt/App/chrome-sandbox".into(), mode: 0o4755 }]))),
            ("postinst", "chmod u+s /opt/App/chrome-sandbox", Some(unknown())),
            ("postinst", "chmod -R 755 /opt/App", Some(unknown())),
            ("postinst", "chmod 755 relative/path", Some(unknown())),
            ("postinst", "chown root:root /opt/App/chrome-sandbox", Some(Outcome::Handled("every file in the package is owned by root"))),
            ("postinst", "chown -R root /opt/App", Some(Outcome::Handled("every file in the package is owned by root"))),
            ("postinst", "chown nobody:nogroup /var/lib/app", Some(unknown())),
            ("postinst", "ln -sf /opt/App/app /usr/bin/app", Some(Outcome::Actions(vec![symlink("/usr/bin/app", "/opt/App/app")]))),
            ("postinst", "ln -s ../lib/app/run /usr/bin/run", Some(Outcome::Actions(vec![symlink("/usr/bin/run", "../lib/app/run")]))),
            ("postinst", "ln /opt/App/app /usr/bin/app", Some(unknown())),
            ("postinst", "ln -sr /opt/App/app /usr/bin/app", Some(unknown())),
            ("postrm", "rm -f /usr/bin/app", Some(Outcome::Actions(vec![Action::RemoveOwned { path: "/usr/bin/app".into() }]))),
            ("postinst", "rm -f /usr/bin/app", Some(unknown())),
            ("postrm", "rm -rf \"$HOME/.config/app\"", Some(unknown())),
            ("postinst", "update-desktop-database -q > /dev/null 2>&1", Some(Outcome::Hook("desktop database"))),
            ("postinst", "gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true", Some(Outcome::Hook("icon cache"))),
            ("postinst", "update-mime-database /usr/share/mime", Some(Outcome::Hook("MIME database"))),
            ("postinst", "ldconfig", Some(Outcome::Hook("linker cache"))),
            ("postinst", "xdg-icon-resource forceupdate --theme hicolor", Some(Outcome::Hook("icon cache"))),
            ("postinst", "xdg-desktop-menu install /opt/App/app.desktop", Some(unknown())),
            ("postinst", "systemctl daemon-reload", Some(Outcome::Hook("systemd reload"))),
            ("postinst", "systemctl enable app.service", Some(unknown())),
            ("postinst", "deb-systemd-helper enable app.service", Some(unknown())),
            ("postinst", "echo 'x' > /etc/app.conf", Some(unknown())),
            ("postinst", "mkdir -p /var/lib/app", Some(unknown())),
            ("postinst", "apt-get update", Some(Outcome::AptRepo)),
            ("postinst", "echo deb http://x stable main > /etc/apt/sources.list.d/x.list", Some(Outcome::AptRepo)),
        ];
        for (script, line, want) in cases {
            let got = outcomes(script, line);
            match &want {
                None => assert!(got.is_empty(), "{line:?}: expected nothing, got {got:?}"),
                Some(want) => {
                    assert_eq!(got.len(), 1, "{line:?}: {got:?}");
                    assert!(same_kind(&got[0].2, want), "{line:?}: expected {want:?}, got {:?}", got[0].2);
                }
            }
        }
    }

    #[test]
    fn follows_a_whole_script() {
        let script = r#"#!/bin/sh
set -e

APP_DIR=/opt/Demo
SANDBOX="${APP_DIR}/chrome-sandbox"

case "$1" in
    configure|abort-upgrade)
        update-alternatives --install /usr/bin/demo demo \
            "$APP_DIR/demo" 100
        chmod 4755 "$SANDBOX" || true
        if hash update-desktop-database 2>/dev/null; then update-desktop-database; fi
        ;;
    abort-remove|abort-deconfigure) ;;
    *)
        echo "postinst called with unknown argument" >&2
        exit 1
        ;;
esac

setup_repo() {
    cat > /etc/apt/sources.list.d/demo.list <<EOF
deb [arch=amd64] https://example.com/apt stable main
EOF
}
setup_repo
weird-tool --do-something

exit 0
"#;
        let got = outcomes("postinst", script);
        let summary: Vec<(usize, Outcome)> = got.iter().map(|(n, _, o)| (*n, o.clone())).collect();
        assert_eq!(summary.len(), 5, "{got:#?}");
        assert_eq!(summary[0], (9, Outcome::Actions(vec![symlink("/usr/bin/demo", "/opt/Demo/demo")])));
        assert_eq!(summary[1], (11, Outcome::Actions(vec![Action::Chmod { path: "/opt/Demo/chrome-sandbox".into(), mode: 0o4755 }])));
        assert_eq!(summary[2], (12, Outcome::Hook("desktop database")));
        assert_eq!(summary[3].0, 22);
        assert_eq!(summary[3].1, Outcome::AptRepo);
        assert!(got[3].1.contains("plus 1 line heredoc"), "{}", got[3].1);
        assert_eq!(got[4].0, 27);
        assert!(matches!(got[4].2, Outcome::Unknown(_)));
        assert_eq!(got[4].1, "weird-tool --do-something");
    }

    #[test]
    fn tokenizes() {
        let vars = HashMap::from([("D".to_string(), "/opt/x".to_string())]);
        let words = |s: &str| -> Vec<Vec<String>> {
            split_commands(&tokenize(s, &vars)).into_iter().map(|c| c.words).collect()
        };
        let cases: Vec<(&str, Vec<Vec<&str>>)> = vec![
            ("a b c", vec![vec!["a", "b", "c"]]),
            ("a 'b c' \"d e\"", vec![vec!["a", "b c", "d e"]]),
            ("a; b && c || d | e", vec![vec!["a"], vec!["b"], vec!["c"], vec!["d"], vec!["e"]]),
            ("ls $D ${D}/y \"$D/z\" '$D'", vec![vec!["ls", "/opt/x", "/opt/x/y", "/opt/x/z", "$D"]]),
            ("ls $UNKNOWN $(pwd) `pwd`", vec![vec!["ls", "$UNKNOWN", "$(pwd)", "`pwd`"]]),
            ("cmd >/dev/null 2>&1 # note", vec![vec!["cmd"]]),
            ("a\\ b", vec![vec!["a b"]]),
        ];
        for (input, want) in cases {
            assert_eq!(words(input), want, "{input:?}");
        }
        let redirects: Vec<String> = split_commands(&tokenize("x > /etc/f 2>>/var/log/y <in &>/dev/null", &vars))
            .remove(0)
            .redirects;
        assert_eq!(redirects, ["/etc/f", "/var/log/y", "in", "/dev/null"]);
    }

    #[test]
    fn finds_heredoc_delimiters() {
        let cases = [
            ("cat <<EOF", Some("EOF")),
            ("cat > f << 'END'", Some("END")),
            ("cat <<-\"X\"", Some("X")),
            ("cat <<< word", None),
            ("echo '<<EOF'", None),
            ("# <<EOF", None),
        ];
        for (input, want) in cases {
            assert_eq!(heredoc_delimiter(input).as_deref(), want, "{input:?}");
        }
    }
}
