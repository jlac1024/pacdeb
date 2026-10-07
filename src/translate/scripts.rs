//! Reads Debian maintainer scripts without running them. Each command is sorted into
//! something pacdeb turns into package contents, something a pacman hook already does,
//! something that needs nothing on Arch, or something a person has to look at.
//!
//! This is not a shell. It follows plain command lines, literal variables, `if`/`case`
//! branches whose conditions it can decide from the package itself, and functions at
//! the place they are called. Whatever it cannot follow is reported, never guessed at.

use std::collections::HashMap;

use super::fs::mentions_apt;
use super::pathutil::{basename, parent, resolve};

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
const NEUTRAL: [&str; 23] = [
    "[", "[[", "test", "true", "false", ":", "set", "command", "which", "type", "hash", "shift",
    "break", "continue", "unset", "trap", "umask", "wait", "sleep", "readlink", "head", "tail",
    "dirname",
];

const MAX_CALL_DEPTH: usize = 8;

/// `facts` says what a path is once the package is installed: Some when the package
/// decides it, None when it depends on the rest of the system.
pub fn analyze(script: &str, text: &str, facts: &dyn Fn(&str) -> Option<PathFact>) -> Vec<Command> {
    // What dpkg passes as $1 on a fresh install or a removal. DPKG_ROOT is only set for
    // installs into another root.
    let action = match script {
        "preinst" => "install",
        "postinst" => "configure",
        _ => "remove",
    };
    let mut a = Analyzer {
        script: script.to_string(),
        vars: HashMap::from([("DPKG_ROOT".into(), String::new()), ("1".into(), action.into())]),
        functions: HashMap::new(),
        frames: Vec::new(),
        guards: Vec::new(),
        stopped: false,
        returned: false,
        depth: 0,
        facts,
        out: Vec::new(),
    };
    a.run_lines(&logical_lines(text));
    a.out
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
    depth: usize,
    facts: &'a dyn Fn(&str) -> Option<PathFact>,
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

    fn run_lines(&mut self, lines: &[Line]) {
        let mut i = 0;
        while i < lines.len() && !self.stopped && !self.returned {
            let line = &lines[i];
            i += 1;
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
        while k < cmds.len() && !self.stopped && !self.returned {
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
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        match a.as_slice() {
            ["!", ..] => self.eval_bracket(&args[1..]).not(),
            [op, path] if ["-e", "-f", "-x", "-d", "-L", "-h", "-r", "-s", "-w"].contains(op) => {
                if unresolved(path) || !path.starts_with('/') {
                    return Tri::Maybe;
                }
                let Some(fact) = (self.facts)(&resolve("/", path)) else {
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

    fn assign(&mut self, name: &str, value: &str, active: Tri) {
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
        self.depth += 1;
        self.run_lines(&body);
        self.depth -= 1;
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
        if mentions_apt(&all) && first != "update-mime-database" {
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
            "chown" | "chgrp" => chown(args),
            "ln" => ln(args),
            "rm" => rm(args),
            "cp" => cp(args),
            "mkdir" => mkdir(args),
            "install" => install(args),
            "xdg-icon-resource" if args.first().is_some_and(|a| a == "forceupdate") => Outcome::Hook("icon cache"),
            "xdg-desktop-menu" if args.first().is_some_and(|a| a == "forceupdate") => Outcome::Hook("desktop database"),
            "adduser" | "useradd" => add_user(first, args),
            "addgroup" | "groupadd" => add_group(args),
            "usermod" => usermod(args),
            "gpasswd" => gpasswd(args),
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

fn is_test(words: &[String]) -> bool {
    matches!(words.first().map(String::as_str), Some("[" | "[[" | "test" | "!"))
}

fn unresolved(s: &str) -> bool {
    s.contains(['$', '`'])
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

/// A literal absolute path, normalized. None when it still holds expansions, globs or
/// brace patterns like `crashpad.{a,b}`.
fn literal_path(s: &str) -> Option<String> {
    (s.starts_with('/') && !s.contains(['$', '`', '*', '?', '[', '{'])).then(|| resolve("/", s))
}

fn literal_paths(paths: &[&String]) -> Option<Vec<String>> {
    if paths.is_empty() {
        return None;
    }
    paths.iter().map(|p| literal_path(p)).collect()
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
            let mut pairs = vec![(link, path)];
            for chunk in slaves.chunks(4) {
                match chunk {
                    [flag, link, _name, path] if flag == "--slave" => pairs.push((link, path)),
                    _ => return unreadable(),
                }
            }
            let mut actions = Vec::new();
            for (link, path) in pairs {
                let (Some(link), Some(target)) = (literal_path(link), literal_path(path)) else {
                    return unreadable();
                };
                actions.push(Action::Symlink { link, target });
            }
            Outcome::Actions(actions)
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
    match literal_paths(paths) {
        Some(paths) => Outcome::Actions(paths.into_iter().map(|path| Action::Chmod { path, mode }).collect()),
        None => Outcome::Unknown("chmod on a path pacdeb cannot resolve".into()),
    }
}

fn chown(args: &[String]) -> Outcome {
    let owner = args.iter().find(|a| !a.starts_with('-'));
    let to_root = owner.is_some_and(|o| o.split([':', '.']).all(|part| part.is_empty() || part == "root" || part == "0"));
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
        [target, link] if symbolic && !link.ends_with('/') && !unresolved(target) => match literal_path(link) {
            Some(link) => Outcome::Actions(vec![Action::Symlink { link, target: target.to_string() }]),
            None => Outcome::Unknown("ln -s to a path pacdeb cannot resolve".into()),
        },
        _ => Outcome::Unknown("only 'ln -s <target> <absolute link>' is translated".into()),
    }
}

fn rm(args: &[String]) -> Outcome {
    let paths: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    match literal_paths(&paths) {
        Some(paths) => Outcome::Actions(paths.into_iter().map(|path| Action::Remove { path }).collect()),
        None => Outcome::Unknown("rm on a path pacdeb cannot resolve".into()),
    }
}

fn cp(args: &[String]) -> Outcome {
    if args.iter().any(|a| a.starts_with('-') && !matches!(a.as_str(), "-f" | "-p" | "-a" | "--")) {
        return Outcome::Unknown("cp options other than -f, -p and -a are not translated".into());
    }
    let paths: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    match (paths.as_slice(), literal_paths(&paths)) {
        ([_, to], Some(p)) => {
            let from = p[0].clone();
            let to = if to.ends_with('/') { format!("{}/{}", p[1], basename(&from)) } else { p[1].clone() };
            Outcome::Actions(vec![Action::Copy { from, to }])
        }
        _ => Outcome::Unknown("only 'cp <file> <file>' with literal paths is translated".into()),
    }
}

fn mkdir(args: &[String]) -> Outcome {
    let mut paths = Vec::new();
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "-p" | "--parents" => {}
            "-m" => i += 1,
            s if s.starts_with('-') => return Outcome::Unknown(format!("mkdir option {s} is not translated")),
            _ => paths.push(a),
        }
        i += 1;
    }
    match literal_paths(&paths) {
        Some(paths) => Outcome::Actions(paths.into_iter().map(|path| Action::Mkdir { path }).collect()),
        None => Outcome::Unknown("mkdir on a path pacdeb cannot resolve".into()),
    }
}

/// `install -d <dirs>` and `install [-m mode] <file> <dest>`.
fn install(args: &[String]) -> Outcome {
    let mut dirs = false;
    let mut mode = None;
    let mut paths = Vec::new();
    let mut i = 0;
    while let Some(a) = args.get(i) {
        match a.as_str() {
            "-d" => dirs = true,
            "-m" => {
                i += 1;
                mode = args.get(i).and_then(|m| u32::from_str_radix(m, 8).ok());
                if mode.is_none() {
                    return Outcome::Unknown("only numeric install modes are translated".into());
                }
            }
            "-D" | "-p" | "-v" => {}
            s if s.starts_with('-') => return Outcome::Unknown(format!("install option {s} is not translated")),
            _ => paths.push(a),
        }
        i += 1;
    }
    let Some(p) = literal_paths(&paths) else {
        return Outcome::Unknown("install with paths pacdeb cannot resolve".into());
    };
    if dirs {
        return Outcome::Actions(p.into_iter().map(|path| Action::Mkdir { path }).collect());
    }
    let [from, to] = p.as_slice() else {
        return Outcome::Unknown("only 'install <file> <dest>' is translated".into());
    };
    let mut actions = vec![Action::Copy { from: from.clone(), to: to.clone() }];
    if let Some(mode) = mode {
        actions.push(Action::Chmod { path: to.clone(), mode });
    }
    Outcome::Actions(actions)
}

/// A literal user or group name: no expansions, nothing odd.
fn literal_name(s: &str) -> Option<String> {
    let ok = !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c));
    ok.then(|| s.to_string())
}

/// Splits arguments into option values and positionals. `takes_value` lists options
/// that consume the next word. Unresolved words in option position (like an
/// "$ADDUSER_OPTS" holding flags) are skipped.
fn split_args<'a>(args: &'a [String], takes_value: &[&str]) -> (HashMap<&'a str, &'a str>, Vec<&'a str>) {
    let mut opts = HashMap::new();
    let mut positional = Vec::new();
    let mut i = 0;
    while let Some(a) = args.get(i) {
        let a = a.as_str();
        if let Some((k, v)) = a.split_once('=').filter(|(k, _)| k.starts_with("--")) {
            opts.insert(k, v);
        } else if takes_value.contains(&a) {
            if let Some(v) = args.get(i + 1) {
                opts.insert(a, v.as_str());
            }
            i += 1;
        } else if a.starts_with('-') {
            opts.insert(a, "");
        } else if !(a.starts_with('$') && positional.is_empty() && args.len() > i + 1) {
            positional.push(a);
        }
        i += 1;
    }
    (opts, positional)
}

/// `adduser [options] NAME`, `adduser USER GROUP` and `useradd [options] NAME`.
/// Debian policy only lets maintainer scripts create system users, so every user
/// created here becomes a sysusers.d entry.
fn add_user(cmd: &str, args: &[String]) -> Outcome {
    const VALUE: [&str; 20] = [
        "--home", "--shell", "--gecos", "--comment", "--ingroup", "--uid", "--gid", "--firstuid", "--lastuid", "--add_extra_groups",
        "-d", "-s", "-c", "-g", "-G", "-u", "-k", "-K", "-b", "--home-dir",
    ];
    let (opts, names) = split_args(args, &VALUE);
    let unreadable = || Outcome::Unknown(format!("could not read the {cmd} arguments"));
    match names.as_slice() {
        [user, group] if cmd == "adduser" => match (literal_name(user), literal_name(group)) {
            (Some(user), Some(group)) => Outcome::Actions(vec![Action::GroupMember { user, group }]),
            _ => unreadable(),
        },
        [name] => {
            let Some(name) = literal_name(name) else {
                return unreadable();
            };
            // `adduser --group NAME` without --system is addgroup.
            if cmd == "adduser" && opts.contains_key("--group") && !opts.contains_key("--system") {
                return Outcome::Actions(vec![Action::SystemGroup { name }]);
            }
            let home = ["--home", "-d", "--home-dir"]
                .iter()
                .find_map(|k| opts.get(k))
                .filter(|h| literal_path(h).is_some())
                .map(|h| h.to_string());
            let comment = ["--gecos", "--comment", "-c"].iter().find_map(|k| opts.get(k)).map(|c| c.to_string());
            let mut actions = vec![Action::SystemUser { name: name.clone(), home, comment }];
            for key in ["--ingroup", "-g", "-G", "--add_extra_groups"] {
                for group in opts.get(key).into_iter().flat_map(|g| g.split(',')) {
                    match literal_name(group) {
                        Some(group) if group != name => actions.push(Action::GroupMember { user: name.clone(), group }),
                        Some(_) => {}
                        None => return unreadable(),
                    }
                }
            }
            Outcome::Actions(actions)
        }
        _ => unreadable(),
    }
}

/// `addgroup [--system] NAME`, `addgroup USER GROUP` and `groupadd [-r] NAME`.
fn add_group(args: &[String]) -> Outcome {
    let (_, names) = split_args(args, &["--gid", "-g", "-K"]);
    match names.as_slice() {
        [name] => match literal_name(name) {
            Some(name) => Outcome::Actions(vec![Action::SystemGroup { name }]),
            None => Outcome::Unknown("could not read the group name".into()),
        },
        [user, group] => match (literal_name(user), literal_name(group)) {
            (Some(user), Some(group)) => Outcome::Actions(vec![Action::GroupMember { user, group }]),
            _ => Outcome::Unknown("could not read the group arguments".into()),
        },
        _ => Outcome::Unknown("could not read the group arguments".into()),
    }
}

/// `usermod -aG GROUP[,GROUP] USER`.
fn usermod(args: &[String]) -> Outcome {
    let (opts, names) = split_args(args, &["-G", "-aG", "--groups", "-g", "-d", "-s", "-c", "-u"]);
    let groups = opts.get("-aG").or_else(|| opts.get("-G")).or_else(|| opts.get("--groups"));
    let appends = opts.contains_key("-aG") || opts.contains_key("-a") || opts.contains_key("--append");
    match (groups, names.as_slice()) {
        (Some(groups), [user]) if appends => {
            let mut actions = Vec::new();
            for g in groups.split(',') {
                match (literal_name(user), literal_name(g)) {
                    (Some(user), Some(group)) => actions.push(Action::GroupMember { user, group }),
                    _ => return Outcome::Unknown("could not read the usermod arguments".into()),
                }
            }
            Outcome::Actions(actions)
        }
        _ => Outcome::Unknown("only 'usermod -aG <groups> <user>' is translated".into()),
    }
}

/// `gpasswd -a USER GROUP`.
fn gpasswd(args: &[String]) -> Outcome {
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["-a", user, group] => match (literal_name(user), literal_name(group)) {
            (Some(user), Some(group)) => Outcome::Actions(vec![Action::GroupMember { user, group }]),
            _ => Outcome::Unknown("could not read the gpasswd arguments".into()),
        },
        _ => Outcome::Unknown("only 'gpasswd -a <user> <group>' is translated".into()),
    }
}

fn unit_name(u: &str) -> Option<String> {
    let name = u.trim_matches(['\'', '"']);
    if name.is_empty() || unresolved(name) || name.contains(['*', '?', '/']) {
        return None;
    }
    Some(if name.contains('.') { name.to_string() } else { format!("{name}.service") })
}

/// Turns "enable"/"start" style verbs on units into service steps, and the rest into
/// nothing to do.
fn service_steps(verb: &str, units: &[&str], user: bool) -> Outcome {
    let verb = match verb {
        "enable" | "reenable" => "enable",
        "start" | "restart" | "try-restart" | "reload" | "reload-or-restart" | "try-reload-or-restart" => "start",
        "stop" | "disable" => return Outcome::Handled("pacman does not stop or disable services; do it yourself if needed"),
        "daemon-reload" => return Outcome::Hook("systemd reload"),
        "mask" | "unmask" | "is-enabled" | "is-active" | "status" | "preset" | "was-enabled" | "debian-installed"
        | "update-state" | "purge" => return Outcome::Handled("dpkg's own service bookkeeping, not needed on Arch"),
        other => return Outcome::Unknown(format!("service action '{other}' is not translated")),
    };
    let mut steps = Vec::new();
    for u in units {
        match unit_name(u) {
            Some(unit) => steps.push(ServiceStep { verb: verb.to_string(), unit, user }),
            None => return Outcome::Unknown(format!("could not read the unit name '{u}'")),
        }
    }
    if steps.is_empty() {
        return Outcome::Unknown("no unit named".into());
    }
    Outcome::Service(steps)
}

fn systemctl(args: &[String]) -> Outcome {
    let user = args.iter().any(|a| a == "--user" || a == "--global");
    let now = args.iter().any(|a| a == "--now");
    let words: Vec<&str> = args.iter().map(String::as_str).filter(|a| !a.starts_with('-')).collect();
    let Some((verb, units)) = words.split_first() else {
        return Outcome::Unknown("systemctl without a command".into());
    };
    match service_steps(verb, units, user) {
        // `enable --now` also starts.
        Outcome::Service(mut steps) if now && *verb == "enable" => {
            let starts: Vec<ServiceStep> = steps.iter().map(|s| ServiceStep { verb: "start".into(), ..s.clone() }).collect();
            steps.extend(starts);
            Outcome::Service(steps)
        }
        other => other,
    }
}

/// debhelper's wrapper: `deb-systemd-helper [--user] [--quiet] VERB UNIT...`.
fn deb_systemd_helper(args: &[String]) -> Outcome {
    let user = args.iter().any(|a| a == "--user");
    let words: Vec<&str> = args.iter().map(String::as_str).filter(|a| !a.starts_with('-')).collect();
    match words.split_first() {
        Some((verb, units)) => service_steps(verb, units, user),
        None => Outcome::Unknown("deb-systemd-helper without a command".into()),
    }
}

/// `deb-systemd-invoke [--user] VERB UNIT...`. It only acts on units that are enabled,
/// so its starts count only together with an enable.
fn deb_systemd_invoke(args: &[String]) -> Outcome {
    match deb_systemd_helper(args) {
        Outcome::Service(steps) => Outcome::Service(
            steps
                .into_iter()
                .map(|s| if s.verb == "start" { ServiceStep { verb: "start-if-enabled".into(), ..s } } else { s })
                .collect(),
        ),
        other => other,
    }
}

/// `invoke-rc.d [--quiet] NAME ACTION` and `service NAME ACTION`.
fn sysv_service(cmd: &str, args: &[String]) -> Outcome {
    let words: Vec<&str> = args.iter().map(String::as_str).filter(|a| !a.starts_with('-')).collect();
    match words.as_slice() {
        [name, action, ..] => service_steps(action, &[*name], false),
        _ => Outcome::Unknown(format!("could not read the {cmd} arguments")),
    }
}

fn write_heredoc(target: &str, h: &Heredoc, vars: &HashMap<String, String>) -> Outcome {
    let Some(path) = literal_path(target) else {
        return Outcome::Unknown(format!("writes to {target}, a path pacdeb cannot resolve"));
    };
    let mut content = String::new();
    for l in &h.body {
        if h.expand {
            match expand_text(l, vars) {
                Some(t) => content.push_str(&t),
                None => return Outcome::Unknown(format!("the text written to {path} uses values pacdeb cannot resolve")),
            }
        } else {
            content.push_str(l);
        }
        content.push('\n');
    }
    Outcome::Actions(vec![Action::Write { path, content: content.into_bytes() }])
}

/// Expands variables in heredoc text. None when something stays unresolved.
fn expand_text(line: &str, vars: &HashMap<String, String>) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' if chars.get(i + 1).is_some_and(|c| "$`\\".contains(*c)) => {
                out.push(chars[i + 1]);
                i += 2;
            }
            '$' if chars.get(i + 1).is_some_and(|c| c.is_ascii_alphanumeric() || "_{(".contains(*c)) => {
                let mut piece = String::new();
                i = expand(&chars, i, vars, &mut piece);
                if unresolved(&piece) {
                    return None;
                }
                out.push_str(&piece);
            }
            '`' => return None,
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Some(out)
}

/// Detects `name() {`, `name()` and `function name {`. Returns the name and, for a
/// one line function, its body text.
fn function_start(line: &str) -> Option<(String, Option<String>)> {
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
fn function_body(lines: &[Line], mut start: usize) -> (Vec<Line>, usize) {
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
fn split_case_pattern(line: &str) -> Option<(Vec<String>, &str)> {
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
fn glob_match(pattern: &str, word: &str) -> bool {
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
struct Heredoc {
    body: Vec<String>,
    /// False for a quoted delimiter, which turns off expansion.
    expand: bool,
}

#[derive(Debug, Clone)]
struct Line {
    number: usize,
    text: String,
    heredoc: Option<Heredoc>,
}

/// Joins backslash continuations and attaches heredoc bodies to the line that opens them.
fn logical_lines(text: &str) -> Vec<Line> {
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
fn unclosed_quote(s: &str) -> bool {
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
fn heredoc_delimiter(s: &str) -> Option<(String, bool, bool)> {
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
enum Sep {
    End,
    Semi,
    And,
    Or,
    Pipe,
    Background,
}

#[derive(Debug, PartialEq)]
enum Tok {
    Word(String),
    Op(Sep),
    Redirect(String),
}

#[derive(Debug, Clone)]
struct Cmd {
    words: Vec<String>,
    redirects: Vec<String>,
    /// The separator after this command.
    sep: Sep,
}

impl Cmd {
    fn text(&self) -> String {
        self.words.iter().map(|w| if w.is_empty() { "\"\"" } else { w.as_str() }).collect::<Vec<_>>().join(" ")
    }
}

/// Describes one test command in plain words, such as "/etc/foo exists".
fn humanize(words: &[String]) -> String {
    let w: Vec<&str> = words.iter().map(String::as_str).collect();
    match w.as_slice() {
        ["!", ..] => format!("not ({})", humanize(&words[1..])),
        ["[" | "[[", inner @ .., "]" | "]]"] | ["test", inner @ ..] => bracket_words(inner),
        ["command", "-v", x] | ["which", x] | ["hash", x] | ["type", x] => format!("{x} is installed"),
        _ => format!("'{}' succeeds", w.join(" ")),
    }
}

fn bracket_words(a: &[&str]) -> String {
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

fn file_test(op: &str, path: &str, negated: bool) -> String {
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
fn readlink_of(word: &str) -> Option<&str> {
    Some(word.strip_prefix("$(readlink ")?.strip_suffix(')')?.trim())
}

fn split_commands(toks: Vec<Tok>) -> Vec<Cmd> {
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
fn tokenize(s: &str, vars: &HashMap<String, String>) -> Vec<Tok> {
    let chars: Vec<char> = s.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c.is_whitespace() {
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

/// Handles `$NAME`, `$1`, `${NAME}`, `${NAME:-default}`, `$(dirname <path>)` and other
/// `$(...)` at `chars[i]`. Known values are substituted; everything else is copied as
/// written so later checks see an unresolved expansion.
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
            let inner: String = chars[(i + 2).min(j)..j.saturating_sub(1).max(i + 2)].iter().collect();
            match dirname_of(&inner, vars) {
                Some(dir) => word.push_str(&dir),
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
            let (name, default) = match inner.split_once(":-").or_else(|| inner.split_once('-')) {
                Some((n, d)) => (n.to_string(), Some(d.to_string())),
                None => (inner.clone(), None),
            };
            match (vars.get(&name), default) {
                (Some(v), Some(d)) if v.is_empty() && !unresolved(&d) => word.push_str(&d),
                (Some(v), _) => word.push_str(v),
                _ => word.extend(&chars[i..=i + close]),
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

/// Evaluates `dirname <literal path>`, the one command substitution maintainer scripts
/// use to build paths.
fn dirname_of(inner: &str, vars: &HashMap<String, String>) -> Option<String> {
    let rest = inner.trim().strip_prefix("dirname ")?;
    let words: Vec<String> = tokenize(rest, vars)
        .into_iter()
        .map(|t| match t {
            Tok::Word(w) => Some(w),
            _ => None,
        })
        .collect::<Option<_>>()?;
    match words.as_slice() {
        [p] if p.starts_with('/') && !unresolved(p) => Some(parent(&resolve("/", p)).to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package_paths() -> impl Fn(&str) -> Option<PathFact> {
        |p: &str| match p {
            "/opt/App/chrome-sandbox" | "/opt/App/app" => Some(PathFact::File { exec: true, empty: false }),
            "/usr/lib/app/resources/x.xml" => Some(PathFact::File { exec: false, empty: false }),
            "/opt/App" | "/opt/App/data" => Some(PathFact::Dir),
            "/opt/App/link" => Some(PathFact::Symlink),
            _ if p.starts_with("/opt/App/") || p.starts_with("/usr/lib/app/") => Some(PathFact::Missing),
            _ => None,
        }
    }

    fn run(script: &str, text: &str) -> Vec<(usize, String, Outcome)> {
        let exists = package_paths();
        analyze(script, text, &exists).into_iter().map(|c| (c.line, c.text, c.outcome)).collect()
    }

    fn kind(o: &Outcome) -> &'static str {
        match o {
            Outcome::Actions(_) => "actions",
            Outcome::Hook(_) => "hook",
            Outcome::Handled(_) => "handled",
            Outcome::AptRepo => "apt",
            Outcome::Skipped(_) => "skipped",
            Outcome::Conditional { .. } => "conditional",
            Outcome::Service(_) => "service",
            Outcome::Unknown(_) => "unknown",
        }
    }

    fn symlink(link: &str, target: &str) -> Action {
        Action::Symlink { link: link.into(), target: target.into() }
    }

    #[test]
    fn classifies_single_commands() {
        let cases: Vec<(&str, &str, Option<&str>)> = vec![
            ("postinst", "set -e", None),
            ("postinst", "exit 0", None),
            ("postinst", "#DEBHELPER#", None),
            ("postinst", "echo done", None),
            ("postinst", "update-alternatives --install /usr/bin/app app /opt/App/app 100", Some("actions")),
            ("postinst", "update-alternatives --install /usr/bin/app app $DIR/app 100", Some("unknown")),
            ("postinst", "update-alternatives --install /usr/bin/app app", Some("unknown")),
            ("prerm", "update-alternatives --remove app /opt/App/app", Some("handled")),
            ("postinst", "update-alternatives --set app /opt/App/app", Some("unknown")),
            ("postinst", "chmod 4755 '/opt/App/chrome-sandbox' || true", Some("actions")),
            ("postinst", "chmod u+s /opt/App/chrome-sandbox", Some("unknown")),
            ("postinst", "chmod -R 755 /opt/App", Some("unknown")),
            ("postinst", "chown root:root /opt/App/chrome-sandbox", Some("handled")),
            ("postinst", "chown nobody:nogroup /var/lib/app", Some("unknown")),
            ("postinst", "ln -sf /opt/App/app /usr/bin/app", Some("actions")),
            ("postinst", "ln /opt/App/app /usr/bin/app", Some("unknown")),
            ("postinst", "ln -sr /opt/App/app /usr/bin/app", Some("unknown")),
            ("postrm", "rm -f /usr/bin/app", Some("actions")),
            ("postrm", "rm -rf \"$HOME/.config/app\"", Some("unknown")),
            ("postinst", "cp /opt/App/a.xml /usr/share/mime/packages/", Some("actions")),
            ("postinst", "cp -r /opt/App/dir /usr/share/x", Some("unknown")),
            ("postinst", "mkdir -p /usr/share/mime/packages", Some("actions")),
            ("postinst", "install -m 644 /opt/App/a /usr/share/a", Some("actions")),
            ("postinst", "update-desktop-database -q > /dev/null 2>&1", Some("hook")),
            ("postinst", "gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true", Some("hook")),
            ("postinst", "ldconfig", Some("hook")),
            ("postinst", "xdg-icon-resource forceupdate --theme hicolor", Some("hook")),
            ("postinst", "xdg-desktop-menu install /opt/App/app.desktop", Some("unknown")),
            ("postinst", "systemctl daemon-reload", Some("hook")),
            ("postinst", "systemctl enable app.service", Some("service")),
            ("postinst", "deb-systemd-helper enable app.service", Some("service")),
            ("postinst", "deb-systemd-helper --quiet was-enabled app.service", Some("handled")),
            ("postinst", "deb-systemd-invoke start app.service", Some("service")),
            ("postinst", "invoke-rc.d app start", Some("service")),
            ("prerm", "deb-systemd-invoke stop app.service", Some("handled")),
            ("postinst", "update-rc.d app defaults", Some("handled")),
            ("preinst", "dpkg-maintscript-helper rm_conffile /etc/init.d/app -- \"$@\"", Some("handled")),
            ("postinst", "systemctl start 'app@*'", Some("unknown")),
            ("postinst", "adduser --system --home /var/lib/app app", Some("actions")),
            ("postinst", "adduser $OPTS _app_net", Some("actions")),
            ("postinst", "useradd -r -d /var/lib/app -s /usr/bin/nologin -G video,audio app", Some("actions")),
            ("postinst", "addgroup --system appgroup", Some("actions")),
            ("postinst", "adduser app video", Some("actions")),
            ("postinst", "usermod -aG video app", Some("actions")),
            ("postinst", "adduser --system $NAME", Some("unknown")),
            ("postrm", "deluser --quiet app", Some("handled")),
            ("postinst", "echo 'x' > /etc/app.conf", Some("unknown")),
            ("postinst", "apt-get update", Some("apt")),
            ("postinst", "echo deb http://x stable main > /etc/apt/sources.list.d/x.list", Some("apt")),
            ("postinst", "# writes /etc/apt/sources.list.d/x.list", None),
            ("postinst", "SOURCES=/etc/apt/sources.list.d/x.list", None),
        ];
        for (script, line, want) in cases {
            let got = run(script, line);
            match want {
                None => assert!(got.is_empty(), "{line:?}: expected nothing, got {got:?}"),
                Some(want) => {
                    assert_eq!(got.len(), 1, "{line:?}: {got:?}");
                    assert_eq!(kind(&got[0].2), want, "{line:?}: {:?}", got[0].2);
                }
            }
        }
    }

    #[test]
    fn resolves_variables_defaults_and_dirname() {
        let script = r#"ROOT="${DPKG_ROOT:-}"
DIR="$ROOT/usr/share/mime/packages"
FILE="$DIR/app.xml"
mkdir -p "$(dirname "$FILE")"
cp "$ROOT/usr/lib/app/resources/x.xml" "$FILE"
chmod 0644 "$FILE"
"#;
        let got = run("postinst", script);
        let outcomes: Vec<&Outcome> = got.iter().map(|(_, _, o)| o).collect();
        assert_eq!(
            outcomes,
            [
                &Outcome::Actions(vec![Action::Mkdir { path: "/usr/share/mime/packages".into() }]),
                &Outcome::Actions(vec![Action::Copy {
                    from: "/usr/lib/app/resources/x.xml".into(),
                    to: "/usr/share/mime/packages/app.xml".into()
                }]),
                &Outcome::Actions(vec![Action::Chmod { path: "/usr/share/mime/packages/app.xml".into(), mode: 0o644 }]),
            ]
        );
    }

    #[test]
    fn follows_case_on_the_dpkg_action() {
        let script = "case \"$1\" in\n  configure)\n    ln -s /opt/App/app /usr/bin/app\n    ;;\n  abort-upgrade|abort-remove)\n    weird-tool\n    ;;\n  *)\n    other-tool\n    ;;\nesac\n";
        let got = run("postinst", script);
        let summary: Vec<(usize, &str)> = got.iter().map(|(n, _, o)| (*n, kind(o))).collect();
        assert_eq!(summary, [(3, "actions"), (6, "skipped"), (9, "skipped")]);
        assert_eq!(got[0].2, Outcome::Actions(vec![symlink("/usr/bin/app", "/opt/App/app")]));
        // In postrm, $1 is "remove", so the configure branch is the one that is skipped.
        let got = run("postrm", script);
        assert_eq!(kind(&got[0].2), "skipped");
    }

    #[test]
    fn decides_conditions_from_the_package() {
        let script = r#"
if [ -x /opt/App/app ]; then
  ln -s /opt/App/app /usr/bin/app
fi
if [ -x /opt/App/missing ]; then
  ln -s /opt/App/missing /usr/bin/missing
elif [ -z "$ROOTX" ]; then
  maybe-tool
else
  chmod 4755 /opt/App/chrome-sandbox
fi
[ -x /opt/App/missing ] || rm -f /usr/bin/stale
if [ -f /etc/apparmor.d/abi/4.0 ]; then
  cat > /etc/apparmor.d/app <<EOF
profile app /opt/App/app {}
EOF
fi
"#;
        let got = run("postinst", script);
        let summary: Vec<(usize, &str)> = got.iter().map(|(n, _, o)| (*n, kind(o))).collect();
        assert_eq!(
            summary,
            [(3, "actions"), (6, "skipped"), (8, "conditional"), (10, "conditional"), (12, "actions"), (14, "conditional")]
        );
        let Outcome::Conditional { condition, would } = &got[5].2 else { unreachable!() };
        assert_eq!(condition, "/etc/apparmor.d/abi/4.0 exists");
        assert!(would.contains("/etc/apparmor.d/app with the script's"), "{would}");
        let Outcome::Skipped(why) = &got[1].2 else { unreachable!() };
        assert!(why.ends_with("false for the package: /opt/App/missing is executable"), "{why}");
        let Outcome::Conditional { condition, .. } = &got[2].2 else { unreachable!() };
        assert_eq!(condition, "'$ROOTX' is empty");
    }

    #[test]
    fn runs_functions_where_they_are_called() {
        let script = r#"APP=/opt/App
never_called() {
  frobnicate
}
setup()
{
  ln -s "$APP/app" /usr/bin/app
  if [ "$1" = "full" ]; then
    chmod 4755 "$APP/chrome-sandbox"
  fi
  return 0
  unreachable-tool
}
one_liner() { ldconfig; }
setup full
one_liner
"#;
        let got = run("postinst", script);
        let summary: Vec<(usize, &str)> = got.iter().map(|(n, _, o)| (*n, kind(o))).collect();
        assert_eq!(summary, [(7, "actions"), (9, "actions"), (14, "hook")]);
    }

    #[test]
    fn writes_heredocs() {
        let script = "NAME=demo\ncat > /usr/share/demo/a.conf <<EOF\nname=$NAME\nliteral=\\$HOME\nEOF\ncat > /usr/share/demo/b.conf <<'EOF'\nkeep=$NAME\nEOF\ncat > /usr/share/demo/c.conf <<EOF\nuser=$UNKNOWN\nEOF\n";
        let got = run("postinst", script);
        let writes: Vec<&Outcome> = got.iter().map(|(_, _, o)| o).collect();
        assert_eq!(
            writes[0],
            &Outcome::Actions(vec![Action::Write { path: "/usr/share/demo/a.conf".into(), content: b"name=demo\nliteral=$HOME\n".to_vec() }])
        );
        assert_eq!(
            writes[1],
            &Outcome::Actions(vec![Action::Write { path: "/usr/share/demo/b.conf".into(), content: b"keep=$NAME\n".to_vec() }])
        );
        assert_eq!(kind(writes[2]), "unknown");
    }

    #[test]
    fn early_exit_makes_the_rest_conditional() {
        let script = "if [ ! -d /etc/foo ]; then\n  exit 0\nfi\nln -s /opt/App/app /usr/bin/app\n";
        let got = run("postinst", script);
        assert_eq!(got.len(), 1);
        let Outcome::Conditional { condition, .. } = &got[0].2 else { panic!("{got:?}") };
        assert_eq!(condition, "the exit on line 2 did not stop the script first");

        let got = run("postinst", "exit 0\nln -s /opt/App/app /usr/bin/app\n");
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn tokenizes() {
        let vars = HashMap::from([("D".to_string(), "/opt/x".to_string()), ("E".to_string(), String::new())]);
        let words = |s: &str| -> Vec<Vec<String>> { split_commands(tokenize(s, &vars)).into_iter().map(|c| c.words).collect() };
        let cases: Vec<(&str, Vec<Vec<&str>>)> = vec![
            ("a b c", vec![vec!["a", "b", "c"]]),
            ("a 'b c' \"d e\"", vec![vec!["a", "b c", "d e"]]),
            ("a; b && c || d | e", vec![vec!["a"], vec!["b"], vec!["c"], vec!["d"], vec!["e"]]),
            ("ls $D ${D}/y \"$D/z\" '$D'", vec![vec!["ls", "/opt/x", "/opt/x/y", "/opt/x/z", "$D"]]),
            ("ls ${E:-/fallback} ${D:-/fallback} ${NOPE:-x}", vec![vec!["ls", "/fallback", "/opt/x", "${NOPE:-x}"]]),
            ("ls $UNKNOWN $(pwd) `pwd` $(dirname /a/b/c)", vec![vec!["ls", "$UNKNOWN", "$(pwd)", "`pwd`", "/a/b"]]),
            ("cmd >/dev/null 2>&1 # note", vec![vec!["cmd"]]),
            ("x)  ;;", vec![vec!["x)"], vec![";;"]]),
        ];
        for (input, want) in cases {
            assert_eq!(words(input), want, "{input:?}");
        }
        let cmds = split_commands(tokenize("x > /etc/f 2>>/var/log/y <in &>/dev/null", &vars));
        assert_eq!(cmds[0].redirects, ["/etc/f", "/var/log/y", "in", "/dev/null"]);
        let seps: Vec<Sep> = split_commands(tokenize("a && b || c; d", &vars)).iter().map(|c| c.sep).collect();
        assert_eq!(seps, [Sep::And, Sep::Or, Sep::Semi, Sep::End]);
    }

    #[test]
    fn reads_users_groups_and_services() {
        let one = |line: &str| run("postinst", line).remove(0).2;
        assert_eq!(
            one("useradd -r -d /var/lib/app -c 'App daemon' -G video,audio app"),
            Outcome::Actions(vec![
                Action::SystemUser { name: "app".into(), home: Some("/var/lib/app".into()), comment: Some("App daemon".into()) },
                Action::GroupMember { user: "app".into(), group: "video".into() },
                Action::GroupMember { user: "app".into(), group: "audio".into() },
            ])
        );
        assert_eq!(
            one("adduser --system --group --no-create-home --gecos \"App\" app"),
            Outcome::Actions(vec![Action::SystemUser { name: "app".into(), home: None, comment: Some("App".into()) }])
        );
        assert_eq!(one("adduser --group appgroup"), Outcome::Actions(vec![Action::SystemGroup { name: "appgroup".into() }]));
        assert_eq!(
            one("systemctl --user enable --now app-env.service"),
            Outcome::Service(vec![
                ServiceStep { verb: "enable".into(), unit: "app-env.service".into(), user: true },
                ServiceStep { verb: "start".into(), unit: "app-env.service".into(), user: true },
            ])
        );
        assert_eq!(one("invoke-rc.d --quiet app restart"), Outcome::Service(vec![ServiceStep { verb: "start".into(), unit: "app.service".into(), user: false }]));
    }

    #[test]
    fn path_tests_respect_file_types() {
        let cases = [
            ("[ -f /opt/App ]", "skipped"),
            ("[ -d /opt/App ]", "actions"),
            ("[ -d /opt/App/app ]", "skipped"),
            ("[ -x /usr/lib/app/resources/x.xml ]", "skipped"),
            ("[ -x /opt/App/app ]", "actions"),
            ("[ -L /opt/App/link ]", "actions"),
            ("[ -f /opt/App/link ]", "conditional"),
            ("[ -e /opt/App/link ]", "actions"),
            ("[ -e /opt/App/nope ]", "skipped"),
        ];
        for (test, want) in cases {
            let got = run("postinst", &format!("if {test}; then ln -s /opt/App/app /usr/bin/app; fi\n"));
            assert_eq!(got.len(), 1, "{test}: {got:?}");
            assert_eq!(kind(&got[0].2), want, "{test}");
        }
    }

    #[test]
    fn joins_multi_line_strings_and_refuses_brace_paths() {
        let script = "MSG=\"\nName: Please log out\nPriority: Medium\n\"\nrm -rf /var/lib/app/crash.{a,b}\n";
        let got = run("postrm", script);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].0, 5);
        assert_eq!(kind(&got[0].2), "unknown");
        let cases = [("a \"b", true), ("a \"b\"", false), ("it's", true), ("echo # it's", false), ("x='a\"b'", false), ("\"a\\\"b", true)];
        for (input, want) in cases {
            assert_eq!(unclosed_quote(input), want, "{input:?}");
        }
    }

    #[test]
    fn describes_conditions_in_plain_words() {
        let w = |s: &str| -> Vec<String> { s.split(' ').map(String::from).collect() };
        let cases = [
            ("[ -f /etc/x ]", "/etc/x exists"),
            ("[ ! -e /usr/bin/ccd ]", "/usr/bin/ccd does not exist"),
            ("[ -L /usr/bin/ccd ]", "/usr/bin/ccd is a symlink"),
            ("[ -d /etc/apt/sources.list.d ]", "/etc/apt/sources.list.d is a directory"),
            ("command -v aa-enabled", "aa-enabled is installed"),
            ("aa-enabled --quiet", "'aa-enabled --quiet' succeeds"),
            ("! aa-enabled", "not ('aa-enabled' succeeds)"),
            ("test -x /opt/a", "/opt/a is executable"),
        ];
        for (input, want) in cases {
            assert_eq!(humanize(&w(input)), want, "{input:?}");
        }
        let vars = HashMap::from([("L".to_string(), "/usr/bin/ccd".to_string())]);
        let words: Vec<String> = split_commands(tokenize("[ \"$(readlink \"$L\")\" = x ]", &vars)).remove(0).words;
        assert_eq!(humanize(&words), "/usr/bin/ccd points to x");
    }

    #[test]
    fn finds_heredoc_delimiters() {
        let cases = [
            ("cat <<EOF", Some(("EOF", false, false))),
            ("cat > f << 'END'", Some(("END", true, false))),
            ("cat <<-\"X\"", Some(("X", true, true))),
            ("cat <<< word", None),
            ("echo '<<EOF'", None),
            ("# <<EOF", None),
        ];
        for (input, want) in cases {
            let got = heredoc_delimiter(input);
            assert_eq!(got.as_ref().map(|(w, q, t)| (w.as_str(), *q, *t)), want, "{input:?}");
        }
    }

    #[test]
    fn detects_functions_and_globs() {
        let cases = [
            ("setup() {", Some(("setup", None))),
            ("setup ()", Some(("setup", None))),
            ("function setup {", Some(("setup", None))),
            ("one() { ldconfig; }", Some(("one", Some("ldconfig;")))),
            ("echo $(foo)", None),
            ("if [ x ]; then", None),
        ];
        for (input, want) in cases {
            let got = function_start(input);
            assert_eq!(got.as_ref().map(|(n, b)| (n.as_str(), b.as_deref())), want, "{input:?}");
        }
        let globs = [("*", "x", true), ("abort-*", "abort-upgrade", true), ("configure", "configure", true), ("conf*e", "configure", true), ("x", "y", false)];
        for (p, w, want) in globs {
            assert_eq!(glob_match(p, w), want, "{p} vs {w}");
        }
    }
}
