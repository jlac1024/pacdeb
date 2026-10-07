//! Reads Debian maintainer scripts without running them. Each command is sorted into
//! something Ferry turns into package contents, something a pacman hook already does,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Actions(Vec<Action>),
    /// A pacman hook does this already; the value names it.
    Hook(&'static str),
    /// Nothing to do on Arch; the value says why.
    Handled(&'static str),
    AptRepo,
    /// The command sits in a branch that never runs for this package.
    Skipped(String),
    Unknown(String),
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

/// `exists` answers whether a path exists once the package is installed: Some when the
/// package decides it, None when it depends on the rest of the system.
pub fn analyze(script: &str, text: &str, exists: &dyn Fn(&str) -> Option<bool>) -> Vec<Command> {
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
        exists,
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

#[derive(Debug)]
enum Frame {
    If { now: Tri, any: Tri, text: String },
    Case { word: Option<String>, now: Tri, any: Tri },
    Loop { text: String },
    /// A command that only runs after `&&` or `||`.
    Gate { now: Tri, text: String },
}

impl Frame {
    fn now(&self) -> Tri {
        match self {
            Frame::If { now, .. } | Frame::Case { now, .. } | Frame::Gate { now, .. } => *now,
            Frame::Loop { .. } => Tri::Maybe,
        }
    }
    fn text(&self) -> String {
        match self {
            Frame::If { text, .. } | Frame::Gate { text, .. } => text.clone(),
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
    exists: &'a dyn Fn(&str) -> Option<bool>,
    out: Vec<Command>,
}

impl Analyzer<'_> {
    fn active(&self) -> Tri {
        let guard = if self.guards.is_empty() { Tri::Yes } else { Tri::Maybe };
        self.frames.iter().fold(guard, |acc, f| acc.and(f.now()))
    }

    /// The conditions that hold the current command at `want` (Maybe or No).
    fn conditions(&self, want: Tri) -> String {
        let mut parts: Vec<String> = self.frames.iter().filter(|f| f.now() == want).map(Frame::text).collect();
        if want == Tri::Maybe {
            parts.extend(self.guards.iter().map(|g| format!("{g} did not end the script first")));
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
                    let value = self.eval_list(cond);
                    let text = list_text(cond);
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
                        *text = format!("not ({text})");
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
                    let value = self.eval_test(&cmds[k].words);
                    let (now, text) = match cmds[k].sep {
                        Sep::And => (value, cmds[k].text()),
                        _ => (value.not(), format!("not ({})", cmds[k].text())),
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

    fn eval_list(&self, cmds: &[Cmd]) -> Tri {
        let mut value = Tri::Yes;
        let mut sep = Sep::And;
        for (i, c) in cmds.iter().enumerate() {
            let v = self.eval_test(&c.words);
            value = if i == 0 {
                v
            } else {
                match sep {
                    Sep::And => value.and(v),
                    Sep::Or => value.or(v),
                    _ => Tri::Maybe,
                }
            };
            sep = c.sep;
        }
        value
    }

    fn eval_test(&self, words: &[String]) -> Tri {
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
                match (self.exists)(&resolve("/", path)) {
                    Some(b) => Tri::of(b),
                    None => Tri::Maybe,
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
                    Tri::Maybe => self.guards.push(format!("the {first} on line {}", line.number)),
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
                Outcome::Actions(actions) => Outcome::Unknown(format!(
                    "runs only if {}, which depends on the system; it would add {}",
                    self.conditions(Tri::Maybe),
                    actions.iter().map(describe_action).collect::<Vec<_>>().join("; ")
                )),
                Outcome::Unknown(why) => {
                    Outcome::Unknown(format!("{why}; runs only if {}", self.conditions(Tri::Maybe)))
                }
                other => other,
            },
            Tri::No => match outcome {
                Outcome::Actions(_) | Outcome::Unknown(_) => {
                    Outcome::Skipped(format!("does not run, {} is false for this package", self.conditions(Tri::No)))
                }
                _ => return,
            },
        };
        let mut text = std::iter::once(first.to_string()).chain(args.iter().cloned()).collect::<Vec<_>>().join(" ");
        for r in &cmd.redirects {
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
            "systemctl" if args.first().is_some_and(|a| a == "daemon-reload") => Outcome::Hook("systemd reload"),
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

/// A literal absolute path, normalized. None when it still holds expansions or globs.
fn literal_path(s: &str) -> Option<String> {
    (s.starts_with('/') && !s.contains(['$', '`', '*', '?', '['])).then(|| resolve("/", s))
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
        None => Outcome::Unknown("chmod on a path Ferry cannot resolve".into()),
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
            None => Outcome::Unknown("ln -s to a path Ferry cannot resolve".into()),
        },
        _ => Outcome::Unknown("only 'ln -s <target> <absolute link>' is translated".into()),
    }
}

fn rm(args: &[String]) -> Outcome {
    let paths: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    match literal_paths(&paths) {
        Some(paths) => Outcome::Actions(paths.into_iter().map(|path| Action::Remove { path }).collect()),
        None => Outcome::Unknown("rm on a path Ferry cannot resolve".into()),
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
        None => Outcome::Unknown("mkdir on a path Ferry cannot resolve".into()),
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
        return Outcome::Unknown("install with paths Ferry cannot resolve".into());
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

fn write_heredoc(target: &str, h: &Heredoc, vars: &HashMap<String, String>) -> Outcome {
    let Some(path) = literal_path(target) else {
        return Outcome::Unknown(format!("writes to {target}, a path Ferry cannot resolve"));
    };
    let mut content = String::new();
    for l in &h.body {
        if h.expand {
            match expand_text(l, vars) {
                Some(t) => content.push_str(&t),
                None => return Outcome::Unknown(format!("the text written to {path} uses values Ferry cannot resolve")),
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
        while s.ends_with('\\') && i < raw.len() {
            s.pop();
            s.push(' ');
            s.push_str(raw[i].trim_start());
            i += 1;
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

/// Shows a list of commands with the separators between them.
fn list_text(cmds: &[Cmd]) -> String {
    let mut out = String::new();
    for (i, c) in cmds.iter().enumerate() {
        out.push_str(&c.text());
        if i + 1 < cmds.len() {
            out.push_str(match c.sep {
                Sep::And => " && ",
                Sep::Or => " || ",
                Sep::Pipe => " | ",
                _ => "; ",
            });
        }
    }
    out
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
                None => word.extend(&chars[i..j]),
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

    fn package_paths() -> impl Fn(&str) -> Option<bool> {
        |p: &str| {
            if ["/opt/App/chrome-sandbox", "/opt/App/app", "/usr/lib/app/resources/x.xml"].contains(&p) {
                Some(true)
            } else if p.starts_with("/opt/App/") || p.starts_with("/usr/lib/app/") {
                Some(false)
            } else {
                None
            }
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
            ("postinst", "systemctl enable app.service", Some("unknown")),
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
        assert_eq!(summary, [(3, "actions"), (6, "skipped"), (8, "unknown"), (10, "unknown"), (12, "actions"), (14, "unknown")]);
        let Outcome::Unknown(why) = &got[5].2 else { unreachable!() };
        assert!(why.contains("runs only if [ -f /etc/apparmor.d/abi/4.0 ]"), "{why}");
        assert!(why.contains("/etc/apparmor.d/app with the script's"), "{why}");
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
        let Outcome::Unknown(why) = &got[0].2 else { panic!("{got:?}") };
        assert!(why.contains("the exit on line 2 did not end the script first"), "{why}");

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
