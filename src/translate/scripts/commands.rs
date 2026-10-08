// SPDX-License-Identifier: AGPL-3.0-or-later
//! What each command a maintainer script runs becomes on Arch.

use std::collections::HashMap;

use super::*;

pub(super) fn update_alternatives(args: &[String]) -> Outcome {
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

pub(super) fn chmod(args: &[String]) -> Outcome {
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

pub(super) fn chown(args: &[String]) -> Outcome {
    let owner = args.iter().find(|a| !a.starts_with('-'));
    let to_root = owner.is_some_and(|o| o.split([':', '.']).all(|part| part.is_empty() || part == "root" || part == "0"));
    if to_root {
        Outcome::Handled("every file in the package is owned by root")
    } else {
        Outcome::Unknown("pacman packages hold root owned files; other owners need a manual step".into())
    }
}

pub(super) fn ln(args: &[String]) -> Outcome {
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

pub(super) fn rm(args: &[String]) -> Outcome {
    let paths: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    match literal_paths(&paths) {
        Some(paths) => Outcome::Actions(paths.into_iter().map(|path| Action::Remove { path }).collect()),
        None => Outcome::Unknown("rm on a path pacdeb cannot resolve".into()),
    }
}

pub(super) fn cp(args: &[String]) -> Outcome {
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

pub(super) fn mkdir(args: &[String]) -> Outcome {
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
pub(super) fn install(args: &[String]) -> Outcome {
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
pub(super) fn literal_name(s: &str) -> Option<String> {
    let ok = !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c));
    ok.then(|| s.to_string())
}

/// Splits arguments into option values and positionals. `takes_value` lists options
/// that consume the next word. Unresolved words in option position (like an
/// "$ADDUSER_OPTS" holding flags) are skipped.
pub(super) fn split_args<'a>(args: &'a [String], takes_value: &[&str]) -> (HashMap<&'a str, &'a str>, Vec<&'a str>) {
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
pub(super) fn add_user(cmd: &str, args: &[String]) -> Outcome {
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
pub(super) fn add_group(args: &[String]) -> Outcome {
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
pub(super) fn usermod(args: &[String]) -> Outcome {
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
pub(super) fn gpasswd(args: &[String]) -> Outcome {
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["-a", user, group] => match (literal_name(user), literal_name(group)) {
            (Some(user), Some(group)) => Outcome::Actions(vec![Action::GroupMember { user, group }]),
            _ => Outcome::Unknown("could not read the gpasswd arguments".into()),
        },
        _ => Outcome::Unknown("only 'gpasswd -a <user> <group>' is translated".into()),
    }
}

pub(super) fn unit_name(u: &str) -> Option<String> {
    let name = u.trim_matches(['\'', '"']);
    if name.is_empty() || unresolved(name) || name.contains(['*', '?', '/']) {
        return None;
    }
    Some(if name.contains('.') { name.to_string() } else { format!("{name}.service") })
}

/// Turns "enable"/"start" style verbs on units into service steps, and the rest into
/// nothing to do.
pub(super) fn service_steps(verb: &str, units: &[&str], user: bool) -> Outcome {
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

pub(super) fn systemctl(args: &[String]) -> Outcome {
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
pub(super) fn deb_systemd_helper(args: &[String]) -> Outcome {
    let user = args.iter().any(|a| a == "--user");
    let words: Vec<&str> = args.iter().map(String::as_str).filter(|a| !a.starts_with('-')).collect();
    match words.split_first() {
        Some((verb, units)) => service_steps(verb, units, user),
        None => Outcome::Unknown("deb-systemd-helper without a command".into()),
    }
}

/// `deb-systemd-invoke [--user] VERB UNIT...`. It only acts on units that are enabled,
/// so its starts count only together with an enable.
pub(super) fn deb_systemd_invoke(args: &[String]) -> Outcome {
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
pub(super) fn sysv_service(cmd: &str, args: &[String]) -> Outcome {
    let words: Vec<&str> = args.iter().map(String::as_str).filter(|a| !a.starts_with('-')).collect();
    match words.as_slice() {
        [name, action, ..] => service_steps(action, &[*name], false),
        _ => Outcome::Unknown(format!("could not read the {cmd} arguments")),
    }
}

pub(super) fn write_heredoc(target: &str, h: &Heredoc, vars: &HashMap<String, String>) -> Outcome {
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
pub(super) fn expand_text(line: &str, vars: &HashMap<String, String>) -> Option<String> {
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
