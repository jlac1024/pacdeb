// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reads Debian maintainer scripts without running them. Each command is sorted into
//! something pacdeb turns into package contents, something a pacman hook already does,
//! something that needs nothing on Arch, or something a person has to look at.
//!
//! This is not a shell. It follows plain command lines, literal variables, `if`/`case`
//! branches whose conditions it can decide from the package itself, and functions at
//! the place they are called. Whatever it cannot follow is reported, never guessed at.

use std::collections::{HashMap, HashSet};

mod commands;
pub(crate) mod sed;
mod system;
mod shell;
#[cfg(test)]
mod tests;

use super::fs::mentions_apt;
use super::pathutil::{basename, parent, resolve};
use commands::*;
pub(crate) use commands::symbolic_mode;
use shell::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Symlink { link: String, target: String },
    Chmod { path: String, mode: u32 },
    /// Copy a file the package ships to another path.
    Copy { from: String, to: String },
    /// Write literal content, from a heredoc.
    Write { path: String, content: Vec<u8> },
    Mkdir { path: String },
    /// Fine when the path ends up in the package; pacman removes it then.
    Remove { path: String },
    /// A system user, created on Arch from a sysusers.d file.
    SystemUser { name: String, home: Option<String>, comment: Option<String> },
    SystemGroup { name: String },
    GroupMember { user: String, group: String },
    /// A symbolic chmod such as `u+s` or `g+s`, applied to the file's mode in the package.
    ModeChange { path: String, spec: String },
    /// A file owned by someone other than root. Packages hold root owned files, so this
    /// becomes a tmpfiles.d line that sets the owner once the package is installed.
    Owner { path: String, user: Option<String>, group: Option<String> },
    /// `sed -i s/.../.../` on a file the package ships.
    Edit { path: String, sed: sed::Substitution },
    /// `touch`: an empty file when the package has none there.
    Touch { path: String },
}

/// What a script does to a service. Arch packages never enable or start services
/// themselves, so these become a note telling the person what to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStep {
    pub verb: String,
    pub unit: String,
    /// A user service (`systemctl --user`) rather than a system one.
    pub user: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Actions(Vec<Action>),
    /// A pacman hook does this already; the value names it.
    Hook(&'static str),
    /// Nothing to do on Arch; the value says why.
    Handled(&'static str),
    /// Enabling or starting services, left to the person on Arch.
    Service(Vec<ServiceStep>),
    AptRepo,
    /// The command sits in a branch that never runs for this package.
    Skipped(String),
    /// Whether the command runs depends on the system, not the package, so it is left
    /// out. `condition` is in plain words; `would` says what it would have done.
    Conditional { condition: String, would: String },
    Unknown(String),
}

/// What a path is once the package is installed, as far as the package can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathFact {
    Missing,
    File { exec: bool, empty: bool },
    Dir,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub script: String,
    pub line: usize,
    pub text: String,
    pub outcome: Outcome,
}

pub fn describe_action(a: &Action) -> String {
    match a {
        Action::Symlink { link, target } => format!("symlink {link} -> {target}"),
        Action::Chmod { path, mode } => format!("mode {mode:04o} on {path}"),
        Action::Copy { from, to } => format!("{to} as a copy of {from}"),
        Action::Write { path, content } => format!("{path} with the script's {} bytes", content.len()),
        Action::Mkdir { path } => format!("directory {path}"),
        Action::Remove { path } => format!("removal of {path}"),
        Action::SystemUser { name, .. } => format!("system user {name}"),
        Action::SystemGroup { name } => format!("system group {name}"),
        Action::GroupMember { user, group } => format!("{user} in group {group}"),
        Action::ModeChange { path, spec } => format!("mode change {spec} on {path}"),
        Action::Owner { path, user, group } => {
            format!("owner {}:{} on {path}", user.as_deref().unwrap_or("root"), group.as_deref().unwrap_or("root"))
        }
        Action::Edit { path, .. } => format!("an edit of {path}"),
        Action::Touch { path } => format!("empty file {path}"),
    }
}

const HOOKS: [(&str, &str); 6] = [
    ("update-desktop-database", "desktop database"),
    ("gtk-update-icon-cache", "icon cache"),
    ("update-icon-caches", "icon cache"),
    ("update-mime-database", "MIME database"),
    ("ldconfig", "linker cache"),
    ("glib-compile-schemas", "GSettings schemas"),
];

/// Builtins and queries that do not change the system by themselves.
const NEUTRAL: [&str; 25] = [
    "[", "[[", "test", "true", "false", ":", "set", "command", "which", "type", "hash", "shift",
    "break", "continue", "unset", "trap", "umask", "wait", "sleep", "readlink", "head", "tail",
    "dirname", "getent", "id",
];

const MAX_CALL_DEPTH: usize = 8;

/// `facts` says what a path is once the package is installed: Some when the package
/// decides it, None when it depends on the rest of the system. `glob` lists the package
/// files a shell pattern like `/usr/share/app/icons/*.png` matches.
pub fn analyze(script: &str, text: &str, facts: &dyn Fn(&str) -> Option<PathFact>, glob: &dyn Fn(&str) -> Vec<String>) -> Vec<Command> {
    // What dpkg passes as $1 on a fresh install or a removal. DPKG_ROOT is only set for
    // installs into another root.
    let action = match script {
        "preinst" => "install",
        "postinst" => "configure",
        _ => "remove",
    };
    let lines = logical_lines(text);
    let first = run_pass(script, action, &lines, facts, glob, HashSet::new());
    // Variables the apt setup commands use (a temporary folder for the key, say) mark
    // the earlier commands that fill them as apt setup too.
    let learned: HashSet<String> = first.apt_vars.iter().cloned().chain(first.out.iter().filter(|c| c.outcome == Outcome::AptRepo).flat_map(|c| variables_in(&c.text))).collect();
    if learned == first.apt_vars {
        return first.out;
    }
    run_pass(script, action, &lines, facts, glob, learned).out
}

fn run_pass<'a>(
    script: &str,
    action: &str,
    lines: &[Line],
    facts: &'a dyn Fn(&str) -> Option<PathFact>,
    glob: &'a dyn Fn(&str) -> Vec<String>,
    apt_vars: HashSet<String>,
) -> Analyzer<'a> {
    let mut a = Analyzer {
        script: script.to_string(),
        // dpkg runs maintainer scripts from the root folder. $2 (the version installed
        // before) is empty on a first install and a plain removal.
        vars: HashMap::from([
            ("DPKG_ROOT".into(), String::new()),
            ("1".into(), action.into()),
            ("2".into(), String::new()),
            ("PWD".into(), "/".into()),
        ]),
        functions: HashMap::new(),
        frames: Vec::new(),
        guards: Vec::new(),
        stopped: false,
        returned: false,
        unrolled: 0,
        broke: false,
        continued: false,
        apt_vars,
        in_apt_pipeline: false,
        depth: 0,
        facts,
        glob,
        out: Vec::new(),
    };
    a.run_lines(lines);
    a
}

/// Shell variables a command still names, unexpanded. Positional parameters and the
/// environment every script has are left out: they say nothing about the apt setup.
fn variables_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find('$') {
        rest = &rest[i + 1..];
        let name: String = rest.trim_start_matches('{').chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if !name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit()) && !["HOME", "USER", "PWD", "PATH", "SHELL"].contains(&name.as_str()) {
            out.push(name);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tri {
    Yes,
    No,
    Maybe,
}

impl Tri {
    fn of(b: bool) -> Tri {
        if b { Tri::Yes } else { Tri::No }
    }
    fn not(self) -> Tri {
        match self {
            Tri::Yes => Tri::No,
            Tri::No => Tri::Yes,
            Tri::Maybe => Tri::Maybe,
        }
    }
    fn and(self, o: Tri) -> Tri {
        match (self, o) {
            (Tri::No, _) | (_, Tri::No) => Tri::No,
            (Tri::Yes, Tri::Yes) => Tri::Yes,
            _ => Tri::Maybe,
        }
    }
    fn or(self, o: Tri) -> Tri {
        self.not().and(o.not()).not()
    }
}

/// A condition in plain words: all of it, and only the parts the package cannot decide.
#[derive(Debug, Clone, Default)]
struct CondText {
    all: String,
    unknown: String,
}

impl CondText {
    fn negated(&self) -> CondText {
        let not = |s: &str| if s.is_empty() { String::new() } else { format!("not ({s})") };
        CondText { all: not(&self.all), unknown: not(&self.unknown) }
    }
}

#[derive(Debug)]
enum Frame {
    If { now: Tri, any: Tri, text: CondText },
    Case { word: Option<String>, now: Tri, any: Tri },
    Loop { text: String },
    /// A command that only runs after `&&` or `||`.
    Gate { now: Tri, text: CondText },
}

impl Frame {
    fn now(&self) -> Tri {
        match self {
            Frame::If { now, .. } | Frame::Case { now, .. } | Frame::Gate { now, .. } => *now,
            Frame::Loop { .. } => Tri::Maybe,
        }
    }
    /// The condition as it matters at `want`: only the undecided parts for Maybe.
    fn text(&self, want: Tri) -> String {
        match self {
            Frame::If { text, .. } | Frame::Gate { text, .. } => {
                if want == Tri::Maybe { text.unknown.clone() } else { text.all.clone() }
            }
            Frame::Case { word, .. } => format!("the case on '{}' matches", word.as_deref().unwrap_or("?")),
            Frame::Loop { text } => format!("inside the loop '{text}'"),
        }
    }
}

struct Analyzer<'a> {
    script: String,
    /// Variables with a literal value. Anything else is left out, so it stays unresolved.
    vars: HashMap<String, String>,
    functions: HashMap<String, Vec<Line>>,
    frames: Vec<Frame>,
    /// Earlier `exit` or `return` lines that may or may not have run.
    guards: Vec<String>,
    stopped: bool,
    returned: bool,
    /// How many unrolled loops the analyzer is inside; `break` and `continue` act on them.
    unrolled: usize,
    broke: bool,
    continued: bool,
    /// Variables holding apt locations (sources lists, keyrings), so commands using them
    /// count as apt repository setup.
    apt_vars: HashSet<String>,
    /// Inside a pipeline any part of which sets up apt (`curl KEY | gpg --dearmor`).
    in_apt_pipeline: bool,
    depth: usize,
    facts: &'a dyn Fn(&str) -> Option<PathFact>,
    glob: &'a dyn Fn(&str) -> Vec<String>,
    out: Vec<Command>,
}

impl Analyzer<'_> {
    fn active(&self) -> Tri {
        let guard = if self.guards.is_empty() { Tri::Yes } else { Tri::Maybe };
        self.frames.iter().fold(guard, |acc, f| acc.and(f.now()))
    }

    /// The conditions that hold the current command at `want` (Maybe or No).
    fn conditions(&self, want: Tri) -> String {
        let mut parts: Vec<String> = self
            .frames
            .iter()
            .filter(|f| f.now() == want)
            .map(|f| f.text(want))
            .filter(|t| !t.is_empty())
            .collect();
        if want == Tri::Maybe {
            parts.extend(self.guards.iter().cloned());
        }
        parts.join(" and ")
    }

    fn halted(&self) -> bool {
        self.stopped || self.returned || self.broke || self.continued
    }

    fn run_lines(&mut self, lines: &[Line]) {
        let mut i = 0;
        while i < lines.len() && !self.halted() {
            let line = &lines[i];
            i += 1;
            if let Some((var, items, body, next)) = self.unrollable_loop(lines, i - 1) {
                i = next;
                self.unroll(&var, &items, &body);
                continue;
            }
            if let Some((name, inline)) = function_start(&line.text) {
                let body = match inline {
                    Some(body) => vec![Line { number: line.number, text: body, heredoc: None }],
                    None => {
                        let (body, next) = function_body(lines, i);
                        i = next;
                        body
                    }
                };
                self.functions.insert(name, body);
                continue;
            }
            let mut body = line.text.trim().to_string();
            if let Some(Frame::Case { word, now, any }) = self.frames.last_mut() {
                if let Some((patterns, rest)) = split_case_pattern(&body) {
                    let matched = match word {
                        Some(w) => Tri::of(patterns.iter().any(|p| glob_match(p, w))),
                        None => Tri::Maybe,
                    };
                    *now = any.not().and(matched);
                    *any = any.or(matched);
                    body = rest.to_string();
                }
            }
            let cmds = split_commands(tokenize(&body, &self.vars));
            self.run_cmds(line, cmds);
        }
    }

    fn run_cmds(&mut self, line: &Line, mut cmds: Vec<Cmd>) {
        let mut k = 0;
        while k < cmds.len() && !self.halted() {
            // A pipeline is one unit for apt setup: decide it at its first command.
            if k == 0 || cmds[k - 1].sep != Sep::Pipe {
                let end = (k..cmds.len()).find(|&j| cmds[j].sep != Sep::Pipe).unwrap_or(cmds.len() - 1);
                self.in_apt_pipeline = end > k && cmds[k..=end].iter().any(|c| {
                    let text = format!("{} {}", c.words.join(" "), c.redirects.join(" "));
                    apt_setup_text(&text) || self.apt_vars.iter().any(|v| mentions_var(&text, v))
                });
            }
            let first = cmds[k].words.first().cloned().unwrap_or_default();
            match first.as_str() {
                "if" | "elif" => {
                    cmds[k].words.remove(0);
                    let start = k;
                    while k < cmds.len() && cmds[k].words.first().is_none_or(|w| w != "then") {
                        k += 1;
                    }
                    let cond = &cmds[start..k];
                    let (value, text) = self.eval_list(cond);
                    if first == "if" {
                        self.frames.push(Frame::If { now: value, any: value, text });
                    } else if let Some(Frame::If { now, any, text: t }) = self.frames.last_mut() {
                        *now = any.not().and(value);
                        *any = any.or(value);
                        *t = text;
                    }
                    if k < cmds.len() {
                        cmds[k].words.remove(0);
                        continue;
                    }
                }
                "then" | "do" | "{" | "(" => {
                    cmds[k].words.remove(0);
                    continue;
                }
                "else" => {
                    if let Some(Frame::If { now, any, text }) = self.frames.last_mut() {
                        *now = any.not();
                        *text = text.negated();
                    }
                    cmds[k].words.remove(0);
                    continue;
                }
                "fi" | "esac" | "done" => {
                    self.frames.pop();
                }
                ";;" => {
                    if let Some(Frame::Case { now, .. }) = self.frames.last_mut() {
                        *now = Tri::No;
                    }
                }
                "case" => {
                    let word = cmds[k].words.get(1).filter(|w| !unresolved(w)).cloned();
                    self.frames.push(Frame::Case { word, now: Tri::No, any: Tri::No });
                }
                "for" | "while" | "until" => {
                    let text = cmds[k].text();
                    self.frames.push(Frame::Loop { text });
                    while k < cmds.len() && cmds[k].words.first().is_none_or(|w| w != "do") {
                        k += 1;
                    }
                    continue;
                }
                "" | "}" | ")" => {}
                _ if is_test(&cmds[k].words) && matches!(cmds[k].sep, Sep::And | Sep::Or) && k + 1 < cmds.len() => {
                    // `[ -x x ] && cmd` and `[ -x x ] || cmd`: the next command is gated.
                    let (value, text) = self.eval_list(&cmds[k..=k]);
                    let (now, text) = match cmds[k].sep {
                        Sep::And => (value, text),
                        _ => (value.not(), text.negated()),
                    };
                    self.frames.push(Frame::Gate { now, text });
                    let gated = cmds[k + 1].clone();
                    self.command(line, &gated);
                    self.frames.pop();
                    k += 1;
                }
                _ => {
                    let cmd = cmds[k].clone();
                    self.command(line, &cmd);
                }
            }
            k += 1;
        }
    }

    /// A `for NAME in WORDS` loop starting at `lines[at]` whose words are all known, with
    /// globs expanded over the package's files: (name, items, body, index after `done`).
    /// Unknown words leave the loop to the generic handling, which treats it as maybe.
    fn unrollable_loop(&self, lines: &[Line], at: usize) -> Option<(String, Vec<String>, Vec<Line>, usize)> {
        let text = lines[at].text.trim();
        let rest = text.strip_prefix("for ")?;
        let (name, rest) = rest.trim_start().split_once(char::is_whitespace)?;
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        let rest = rest.trim_start().strip_prefix("in")?;
        // One line loops: `for x in a b; do cmd; done`.
        let (words_text, inline_body) = match rest.split_once("; do ").or_else(|| rest.split_once(";do ")) {
            Some((w, b)) => {
                let body = b.trim().strip_suffix("done")?.trim().trim_end_matches(';').to_string();
                (w, Some(body))
            }
            None => (rest.trim().trim_end_matches("; do").trim_end_matches(";do").trim_end_matches(';'), None),
        };
        let mut items = Vec::new();
        for w in split_commands(tokenize(words_text, &self.vars)).into_iter().flat_map(|c| c.words) {
            if unresolved(&w) {
                return None;
            }
            if w.contains(['*', '?']) {
                // Only globs over the package's own files are known; others (like
                // /home/*) depend on the system.
                let found = if w.starts_with('/') { (self.glob)(&w) } else { Vec::new() };
                if found.is_empty() {
                    return None;
                }
                items.extend(found);
            } else {
                items.push(w);
            }
        }
        if items.len() > 64 {
            return None;
        }
        if let Some(body) = inline_body {
            return Some((name.to_string(), items, vec![Line { number: lines[at].number, text: body, heredoc: None }], at + 1));
        }
        // The body runs to the matching `done`; nested loops have their own.
        let mut i = at + 1;
        if lines.get(i).is_some_and(|l| l.text.trim() == "do") && !text.ends_with("do") {
            i += 1;
        } else if !text.ends_with("do") {
            return None;
        }
        let start = i;
        let mut depth = 1;
        while i < lines.len() {
            let t = lines[i].text.trim();
            let first = t.split(|c: char| c.is_whitespace() || c == ';').next().unwrap_or("");
            if matches!(first, "for" | "while" | "until") && (t.ends_with("do") || t.contains("; do")) {
                depth += 1;
            }
            if first == "done" || t.ends_with("; done") || t.ends_with(";done") {
                depth -= 1;
                if depth == 0 {
                    return Some((name.to_string(), items, lines[start..i].to_vec(), i + 1));
                }
            }
            i += 1;
        }
        None
    }

    fn unroll(&mut self, var: &str, items: &[String], body: &[Line]) {
        self.unrolled += 1;
        let active = self.active();
        for item in items {
            self.assign(var, item, active);
            self.run_lines(body);
            self.continued = false;
            if self.broke {
                self.broke = false;
                break;
            }
            if self.stopped || self.returned {
                break;
            }
        }
        self.unrolled -= 1;
    }

    /// Decides a condition list such as `[ -f x ] && command -v y`, and describes it.
    fn eval_list(&self, cmds: &[Cmd]) -> (Tri, CondText) {
        let mut value = Tri::Yes;
        let mut sep = Sep::And;
        let mut all = String::new();
        let mut unknown = String::new();
        for (i, c) in cmds.iter().enumerate() {
            let v = self.eval_test(&c.words);
            let joiner = if sep == Sep::Or { " or " } else { " and " };
            value = if i == 0 {
                v
            } else {
                match sep {
                    Sep::And => value.and(v),
                    Sep::Or => value.or(v),
                    _ => Tri::Maybe,
                }
            };
            let words = humanize(&c.words);
            if i > 0 {
                all.push_str(joiner);
            }
            all.push_str(&words);
            if v == Tri::Maybe {
                if !unknown.is_empty() {
                    unknown.push_str(joiner);
                }
                unknown.push_str(&words);
            }
            sep = c.sep;
        }
        (value, CondText { all, unknown })
    }

    fn eval_test(&self, words: &[String]) -> Tri {
        // debhelper asks dpkg's own service state. A pacdeb package is always a first
        // install as far as that state goes: nothing was installed before, and
        // was-enabled defaults to true for new installs (debhelper's own comment says so).
        if words.first().is_some_and(|w| w == "deb-systemd-helper") {
            let verb = words.iter().skip(1).find(|w| !w.starts_with('-')).map(String::as_str);
            match verb {
                Some("debian-installed") => return Tri::No,
                Some("was-enabled") => return Tri::Yes,
                _ => {}
            }
        }
        let w: Vec<&str> = words.iter().map(String::as_str).collect();
        match w.as_slice() {
            // Whether a tool is installed: known for the base system.
            ["command", "-v", tool] | ["which", tool] | ["type", tool] | ["type", "-p" | "-P", tool] | ["hash", tool] => {
                return system::has_tool(tool).map_or(Tri::Maybe, Tri::of);
            }
            // Users and groups a package makes do not exist yet on a first install.
            ["getent", "group" | "passwd", name] if !unresolved(name) => return Tri::No,
            _ => {}
        }
        match words.first().map(String::as_str) {
            Some("!") => self.eval_test(&words[1..]).not(),
            Some("[") | Some("[[") => {
                let end = words.len() - usize::from(matches!(words.last().map(String::as_str), Some("]" | "]]")));
                self.eval_bracket(&words[1..end])
            }
            Some("test") => self.eval_bracket(&words[1..]),
            Some("true") | Some(":") => Tri::Yes,
            Some("false") => Tri::No,
            _ => Tri::Maybe,
        }
    }

    fn eval_bracket(&self, args: &[String]) -> Tri {
        // `a || b` and `a -o b` bind looser than `a && b` and `a -a b`.
        for or in ["||", "-o"] {
            if let Some(i) = args.iter().position(|w| w == or) {
                return self.eval_bracket(&args[..i]).or(self.eval_bracket(&args[i + 1..]));
            }
        }
        for and in ["&&", "-a"] {
            if let Some(i) = args.iter().position(|w| w == and) {
                // `-a FILE` alone is the old spelling of `-e FILE`, not "and".
                if i > 0 {
                    return self.eval_bracket(&args[..i]).and(self.eval_bracket(&args[i + 1..]));
                }
            }
        }
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let number = |s: &str| s.trim().parse::<i64>().ok();
        match a.as_slice() {
            ["!", ..] => self.eval_bracket(&args[1..]).not(),
            [x, "=~", re] if !unresolved(x) && !unresolved(re) => match regex::Regex::new(re) {
                Ok(re) => Tri::of(re.is_match(x)),
                Err(_) => Tri::Maybe,
            },
            // A word that is not a number makes the test fail in the shell.
            [x, op, y] if matches!(*op, "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge")
                && ((!unresolved(x) && number(x).is_none()) || (!unresolved(y) && number(y).is_none())) =>
            {
                Tri::No
            }
            [x, op, y] if matches!(*op, "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge") => match (number(x), number(y)) {
                (Some(x), Some(y)) => Tri::of(match *op {
                    "-eq" => x == y,
                    "-ne" => x != y,
                    "-lt" => x < y,
                    "-le" => x <= y,
                    "-gt" => x > y,
                    _ => x >= y,
                }),
                _ => Tri::Maybe,
            },
            [op, path] if ["-e", "-a", "-f", "-x", "-d", "-L", "-h", "-r", "-s", "-w"].contains(op) => {
                // An empty word names no file.
                if path.is_empty() {
                    return Tri::No;
                }
                let Some(path) = self.absolute(path) else {
                    return Tri::Maybe;
                };
                let Some(fact) = (self.facts)(&path).or_else(|| system::system_fact(&path)) else {
                    return Tri::Maybe;
                };
                use PathFact::*;
                // A symlink's target type is not known here, so type tests on one stay open.
                match (*op, fact) {
                    (_, Missing) => Tri::No,
                    ("-L" | "-h", f) => Tri::of(f == Symlink),
                    (_, Symlink) if *op != "-e" && *op != "-r" && *op != "-w" => Tri::Maybe,
                    ("-f", f) => Tri::of(matches!(f, File { .. })),
                    ("-d", f) => Tri::of(f == Dir),
                    ("-x", File { exec, .. }) => Tri::of(exec),
                    ("-s", File { empty, .. }) => Tri::of(!empty),
                    _ => Tri::Yes,
                }
            }
            ["-z", s] if !unresolved(s) => Tri::of(s.is_empty()),
            ["-n", s] if !unresolved(s) => Tri::of(!s.is_empty()),
            [x, "=" | "==", y] if !unresolved(x) && !unresolved(y) => Tri::of(x == y),
            [x, "!=", y] if !unresolved(x) && !unresolved(y) => Tri::of(x != y),
            [s] if !unresolved(s) => Tri::of(!s.is_empty()),
            _ => Tri::Maybe,
        }
    }

    /// A literal path made absolute: relative ones resolve against the working folder
    /// when it is known.
    fn absolute(&self, path: &str) -> Option<String> {
        if unresolved(path) {
            return None;
        }
        if path.starts_with('/') {
            return Some(resolve("/", path));
        }
        let pwd = self.vars.get("PWD")?;
        Some(resolve(pwd, path))
    }

    fn assign(&mut self, name: &str, value: &str, active: Tri) {
        if apt_setup_text(value) || self.apt_vars.iter().any(|v| mentions_var(value, v)) {
            self.apt_vars.insert(name.to_string());
        }
        match active {
            Tri::Yes if !unresolved(value) => {
                self.vars.insert(name.to_string(), value.to_string());
            }
            Tri::No => {}
            // Maybe set, or set to something unknown: the value can no longer be trusted.
            _ => {
                if self.vars.get(name).map(String::as_str) != Some(value) {
                    self.vars.remove(name);
                }
            }
        }
    }

    fn command(&mut self, line: &Line, cmd: &Cmd) {
        let active = self.active();
        let mut words: &[String] = &cmd.words;
        while let Some((name, value)) = words.first().and_then(|w| assignment(w)) {
            self.assign(name, value, active);
            words = &words[1..];
        }
        let Some(first) = words.first().map(String::as_str) else {
            return;
        };
        // `/usr/bin/xdg-icon-resource` is the same command as `xdg-icon-resource`.
        let first = match first.rsplit_once('/') {
            Some((dir, name)) if ["/usr/bin", "/bin", "/usr/sbin", "/sbin"].contains(&dir) => name,
            _ => first,
        };
        let args = &words[1..];
        match first {
            "export" | "local" | "readonly" => {
                for w in args {
                    if let Some((name, value)) = assignment(w) {
                        self.assign(name, value, active);
                    }
                }
                return;
            }
            "cd" => {
                let target = args.first().map(String::as_str).unwrap_or("/");
                match (active, self.absolute(target)) {
                    (Tri::Yes, Some(dir)) => {
                        self.vars.insert("PWD".into(), dir);
                    }
                    (Tri::No, _) => {}
                    _ => {
                        self.vars.remove("PWD");
                    }
                }
                return;
            }
            "break" | "continue" if self.unrolled > 0 => {
                match active {
                    Tri::Yes if first == "break" => self.broke = true,
                    Tri::Yes => self.continued = true,
                    Tri::Maybe => self.guards.push(format!("the {first} on line {} did not leave the loop first", line.number)),
                    Tri::No => {}
                }
                return;
            }
            "exit" | "return" => {
                match active {
                    Tri::Yes if first == "exit" => self.stopped = true,
                    Tri::Yes => self.returned = true,
                    Tri::Maybe => self.guards.push(format!("the {first} on line {} did not stop the script first", line.number)),
                    Tri::No => {}
                }
                return;
            }
            f if self.functions.contains_key(f) => {
                if active != Tri::No && self.depth < MAX_CALL_DEPTH {
                    self.call(f, args);
                }
                return;
            }
            _ => {}
        }

        // `./file` after a `cd` names a file in that folder.
        let args: Vec<String> = args
            .iter()
            .map(|a| if a.starts_with("./") || a.starts_with("../") { self.absolute(a).unwrap_or_else(|| a.clone()) } else { a.clone() })
            .collect();
        let args = args.as_slice();
        let Some(outcome) = self.classify(first, args, cmd, line) else {
            return;
        };
        let outcome = match active {
            Tri::Yes => outcome,
            Tri::Maybe => match outcome {
                Outcome::Actions(actions) => Outcome::Conditional {
                    condition: self.conditions(Tri::Maybe),
                    would: format!("add {}", actions.iter().map(describe_action).collect::<Vec<_>>().join("; ")),
                },
                Outcome::Unknown(why) => Outcome::Conditional { condition: self.conditions(Tri::Maybe), would: why },
                other => other,
            },
            Tri::No => match outcome {
                Outcome::Actions(_) | Outcome::Unknown(_) => {
                    Outcome::Skipped(format!("does not run; this is false for the package: {}", self.conditions(Tri::No)))
                }
                _ => return,
            },
        };
        let mut text = std::iter::once(first.to_string()).chain(args.iter().cloned()).collect::<Vec<_>>().join(" ");
        if let Some((head, rest)) = text.split_once('\n') {
            // A quoted multi line argument: the first line is enough to recognize it.
            text = format!("{} ... ({} more lines)", head.trim_end(), rest.lines().count());
        }
        for r in cmd.redirects.iter().filter(|r| !is_harmless_redirect(r)) {
            text.push_str(&format!(" > {r}"));
        }
        if let Some(h) = &line.heredoc {
            text.push_str(&format!(" (heredoc, {} lines)", h.body.len()));
        }
        self.out.push(Command { script: self.script.clone(), line: line.number, text, outcome });
    }

    fn call(&mut self, name: &str, args: &[String]) {
        let body = self.functions[name].clone();
        let saved: Vec<(String, Option<String>)> = (1..=9)
            .map(|i| i.to_string())
            .map(|k| {
                let old = self.vars.remove(&k);
                (k, old)
            })
            .collect();
        for (i, a) in args.iter().enumerate().take(9) {
            if !unresolved(a) {
                self.vars.insert((i + 1).to_string(), a.clone());
            }
        }
        let (frames, guards) = (self.frames.len(), self.guards.len());
        // The body's own pipelines decide for themselves.
        let in_pipeline = std::mem::take(&mut self.in_apt_pipeline);
        self.depth += 1;
        self.run_lines(&body);
        self.depth -= 1;
        self.in_apt_pipeline = in_pipeline;
        self.frames.truncate(frames);
        self.guards.truncate(guards);
        self.returned = false;
        for (k, old) in saved {
            match old {
                Some(v) => self.vars.insert(k, v),
                None => self.vars.remove(&k),
            };
        }
    }

    /// None for commands that need no report.
    fn classify(&self, first: &str, args: &[String], cmd: &Cmd, line: &Line) -> Option<Outcome> {
        let writes: Vec<&String> = cmd.redirects.iter().filter(|t| !is_harmless_redirect(t)).collect();
        let heredoc_text = line.heredoc.as_ref().map(|h| h.body.join("\n")).unwrap_or_default();
        let all = format!("{first} {} {} {heredoc_text}", args.join(" "), cmd.redirects.join(" "));
        let uses_apt_var = self.apt_vars.iter().any(|v| mentions_var(&all, v));
        if (apt_setup_text(&all) || uses_apt_var || self.in_apt_pipeline) && first != "update-mime-database" {
            return Some(Outcome::AptRepo);
        }
        if matches!(first, "echo" | "printf") && writes.is_empty() {
            return None;
        }
        if NEUTRAL.contains(&first) && writes.is_empty() {
            return None;
        }
        if first == "cat" && args.is_empty() && writes.len() == 1 {
            if let Some(h) = &line.heredoc {
                return Some(write_heredoc(writes[0], h, &self.vars));
            }
        }
        if let Some(target) = writes.first() {
            return Some(Outcome::Unknown(format!("writes to {target}")));
        }
        Some(match first {
            "update-alternatives" => update_alternatives(args),
            "chmod" => chmod(args),
            "chown" | "chgrp" => chown(first, args),
            "ln" => ln(args),
            "rm" => rm(args),
            "cp" => cp(args),
            "mkdir" => mkdir(args),
            "install" => install(args),
            "xdg-icon-resource" => xdg_icon_resource(args),
            "xdg-desktop-menu" => xdg_desktop_menu(args),
            "sed" => sed_in_place(args),
            "touch" => touch(args),
            "adduser" | "useradd" => add_user(first, args),
            "addgroup" | "groupadd" => add_group(args),
            "usermod" => usermod(args),
            "gpasswd" => gpasswd(args),
            "pkill" | "killall" => Outcome::Handled("stops a running copy of the app; pacman does not, so restart it after an update"),
            "deluser" | "delgroup" | "userdel" | "groupdel" => {
                Outcome::Handled("Arch keeps system users and groups when a package is removed")
            }
            "systemctl" => systemctl(args),
            "deb-systemd-helper" => deb_systemd_helper(args),
            "deb-systemd-invoke" => deb_systemd_invoke(args),
            "invoke-rc.d" | "service" => sysv_service(first, args),
            "update-rc.d" => Outcome::Handled("SysV init links are not used on Arch"),
            "dpkg-maintscript-helper" => {
                Outcome::Handled("dpkg's own config file bookkeeping; pacman tracks config files itself")
            }
            "dpkg-trigger" => Outcome::Handled("dpkg triggers have no pacman counterpart; pacman hooks cover the common ones"),
            f => match HOOKS.iter().find(|(name, _)| *name == f) {
                Some((_, hook)) => Outcome::Hook(hook),
                None => Outcome::Unknown("no translation for this command".into()),
            },
        })
    }
}

/// Whether `path` matches the shell glob `pattern`, where `*` and `?` stay within one
/// path segment as they do in the shell.
pub fn glob_path(pattern: &str, path: &str) -> bool {
    let (p, t): (Vec<&str>, Vec<&str>) = (pattern.split('/').collect(), path.split('/').collect());
    p.len() == t.len() && p.iter().zip(&t).all(|(p, t)| pattern_match(p, t))
}

pub(super) fn is_test(words: &[String]) -> bool {
    matches!(words.first().map(String::as_str), Some("[" | "[[" | "test" | "!" | "command" | "which" | "type" | "hash" | "getent"))
}

/// Text that belongs to setting up the vendor's apt repository: apt's own files and
/// tools, debsig policies, and the repository switches vendors keep in /etc/default.
pub(super) fn apt_setup_text(text: &str) -> bool {
    mentions_apt(text)
        || ["apt-config", "APT_CONFIG", "APT_SOURCESDIR", "APT_TRUSTEDDIR", "/debsig/", "repo_add_once", "repo_reenable_on_distupgrade"]
            .iter()
            .any(|m| text.contains(m))
}

/// Whether text still refers to shell variable `name` (unexpanded).
pub(super) fn mentions_var(text: &str, name: &str) -> bool {
    let plain = format!("${name}");
    text.contains(&format!("${{{name}}}"))
        || text.match_indices(&plain).any(|(i, _)| !text[i + plain.len()..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_'))
}

/// Whether a word still holds an expansion: `$NAME`, `${...}`, `$(...)`, a special
/// parameter or a backquote. A `$` before anything else (the end of a regex like
/// `^[0-9]+$`) is a plain character.
pub(super) fn unresolved(s: &str) -> bool {
    let chars: Vec<char> = s.chars().collect();
    chars.iter().enumerate().any(|(i, c)| {
        *c == '`' || (*c == '$' && chars.get(i + 1).is_some_and(|n| n.is_ascii_alphanumeric() || "_{(@*#?!$-".contains(*n)))
    })
}

pub(super) fn assignment(word: &str) -> Option<(&str, &str)> {
    let (name, value) = word.split_once('=')?;
    let valid = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then_some((name, value))
}

pub(super) fn is_harmless_redirect(target: &str) -> bool {
    target == "/dev/null" || target == "-" || target.chars().all(|c| c.is_ascii_digit())
}

/// A literal absolute path, normalized. None when it still holds expansions, globs or
/// brace patterns like `crashpad.{a,b}`.
pub(super) fn literal_path(s: &str) -> Option<String> {
    (s.starts_with('/') && !s.contains(['$', '`', '*', '?', '[', '{'])).then(|| resolve("/", s))
}

pub(super) fn literal_paths(paths: &[&String]) -> Option<Vec<String>> {
    if paths.is_empty() {
        return None;
    }
    paths.iter().map(|p| literal_path(p)).collect()
}
