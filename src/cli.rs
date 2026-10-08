use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::error::Result;
use crate::help;

pub fn run(args: &[String]) -> ExitCode {
    let Some(first) = args.first() else {
        print!("{}", help::OVERVIEW);
        return ExitCode::SUCCESS;
    };
    // 'pacdeb <command> --help' anywhere in the arguments shows that command's page.
    if let Some(page) = help::page(first).filter(|_| args[1..].iter().any(|a| a == "-h" || a == "--help")) {
        print!("{page}");
        return ExitCode::SUCCESS;
    }

    match first.as_str() {
        "-h" | "--help" => {
            print!("{}", help::OVERVIEW);
            ExitCode::SUCCESS
        }
        "help" => match &args[1..] {
            [] => {
                print!("{}", help::OVERVIEW);
                ExitCode::SUCCESS
            }
            [cmd] => match help::page(cmd) {
                Some(page) => {
                    print!("{page}");
                    ExitCode::SUCCESS
                }
                None => usage_error("", &format!("no command named '{cmd}'")),
            },
            _ => usage_error("", "usage: pacdeb help [command]"),
        },
        "-V" | "--version" => {
            println!("pacdeb {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "inspect" => inspect(&args[1..]),
        "convert" => convert(&args[1..]),
        "install" => install(&args[1..]),
        "update" => update(&args[1..]),
        "add" => registry_cmd("add", &args[1..], "usage: pacdeb add <name> [--preset <p> | --source <type>] [options]", |pos, f| match pos {
            [name] => crate::apps::add(name, f),
            _ => Err(crate::error::Error::new("usage: pacdeb add <name> [--preset <p> | --source <type>] [options]")),
        }),
        "set" => registry_cmd("set", &args[1..], "usage: pacdeb set <name> [options], or pacdeb set --channel <name>", |pos, f| match pos {
            [] => crate::apps::set(None, f),
            [name] => crate::apps::set(Some(name), f),
            _ => Err(crate::error::Error::new("usage: pacdeb set <name> [options], or pacdeb set --channel <name>")),
        }),
        "list" => match &args[1..] {
            [] => finish(crate::apps::list()),
            _ => usage_error("list", "usage: pacdeb list"),
        },
        "remove" => match &args[1..] {
            [name] if !name.starts_with('-') => finish(crate::apps::remove(name)),
            _ => usage_error("remove", "usage: pacdeb remove <app>"),
        },
        "check" => {
            let notify = args[1..].iter().any(|a| a == "--notify");
            let rest: Vec<&String> = args[1..].iter().filter(|a| *a != "--notify").collect();
            match rest[..] {
                [] => finish(crate::apps::check(None, notify)),
                [name] if !name.starts_with('-') => finish(crate::apps::check(Some(name), notify)),
                _ => usage_error("check", "usage: pacdeb check [app] [--notify]"),
            }
        }
        "packages" => match &args[1..] {
            [app] if !app.starts_with('-') => finish(crate::browse::run(app)),
            _ => usage_error("packages", "usage: pacdeb packages <app>"),
        },
        "repo" => match &args[1..] {
            [a] if a == "init" => finish(crate::repo::init(None)),
            [a, dir] if a == "init" && !dir.starts_with('-') => finish(crate::repo::init(Some(dir))),
            [a] if a == "status" => finish(crate::repo::status()),
            [a] if a == "remove" => finish(crate::repo::forget()),
            _ => usage_error("repo", "usage: pacdeb repo init [dir] | status | remove"),
        },
        "timer" => match &args[1..] {
            [action] if ["enable", "disable", "status", "run"].contains(&action.as_str()) => finish(crate::timer::run(action)),
            _ => usage_error("timer", "usage: pacdeb timer enable|disable|status"),
        },
        other => usage_error("", &format!("unknown command '{other}'")),
    }
}

fn inspect(args: &[String]) -> ExitCode {
    let [path] = args else {
        return usage_error("inspect", "usage: pacdeb inspect <file.deb>");
    };
    if path.starts_with('-') {
        return usage_error("inspect", &format!("unknown option '{path}'. Usage: pacdeb inspect <file.deb>"));
    }
    finish(crate::inspect::run(Path::new(path)))
}

const CONVERT_USAGE: &str = "usage: pacdeb convert <file.deb> [--direct] [--out <dir>] [--dry-run]";

fn convert(args: &[String]) -> ExitCode {
    let mut deb = None;
    let mut opts = (false, false, None);
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        match a.as_str() {
            "--dry-run" => opts.0 = true,
            "--direct" => opts.1 = true,
            "--out" => match iter.next() {
                Some(dir) => opts.2 = Some(PathBuf::from(dir)),
                None => return usage_error("convert", &format!("--out needs a directory. {CONVERT_USAGE}")),
            },
            s if s.starts_with('-') => return usage_error("convert", &format!("unknown option '{s}'. {CONVERT_USAGE}")),
            s if deb.is_none() => deb = Some(PathBuf::from(s)),
            s => return usage_error("convert", &format!("unexpected argument '{s}'. {CONVERT_USAGE}")),
        }
    }
    let Some(deb) = deb else {
        return usage_error("convert", CONVERT_USAGE);
    };
    let (dry_run, direct, out) = opts;
    finish(crate::convert::run(&crate::convert::Options { deb, dry_run, direct, out }))
}

const INSTALL_USAGE: &str = "usage: pacdeb install <file.deb|name> [--direct]";

fn install(args: &[String]) -> ExitCode {
    let mut target = None;
    let mut direct = false;
    for a in args {
        match a.as_str() {
            "--direct" => direct = true,
            s if s.starts_with('-') => return usage_error("install", &format!("unknown option '{s}'. {INSTALL_USAGE}")),
            s if target.is_none() => target = Some(PathBuf::from(s)),
            s => return usage_error("install", &format!("unexpected argument '{s}'. {INSTALL_USAGE}")),
        }
    }
    let Some(target) = target else {
        return usage_error("install", INSTALL_USAGE);
    };
    // A path to a file is a deb; anything else is a tracked app's name.
    if target.exists() || target.extension().is_some_and(|e| e == "deb") {
        finish(crate::update::install_file(&target, direct))
    } else {
        finish(crate::update::install_app(&target.to_string_lossy(), direct))
    }
}

const UPDATE_USAGE: &str = "usage: pacdeb update [name] [--file <file.deb>] [--direct] [--no-install]";

fn update(args: &[String]) -> ExitCode {
    let mut opts = crate::update::Options { name: None, file: None, direct: false, no_install: false, notify: false };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--direct" => opts.direct = true,
            "--no-install" => opts.no_install = true,
            "--file" => match it.next() {
                Some(f) => opts.file = Some(PathBuf::from(f)),
                None => return usage_error("update", &format!("--file needs a .deb. {UPDATE_USAGE}")),
            },
            s if s.starts_with('-') => return usage_error("update", &format!("unknown option '{s}'. {UPDATE_USAGE}")),
            s if opts.name.is_none() => opts.name = Some(s.to_string()),
            s => return usage_error("update", &format!("unexpected argument '{s}'. {UPDATE_USAGE}")),
        }
    }
    finish(crate::update::update(&opts))
}

/// Parses add/set style arguments and runs the command with them.
fn registry_cmd(cmd: &str, args: &[String], usage: &str, run: impl FnOnce(&[String], &crate::apps::Flags) -> Result<()>) -> ExitCode {
    match crate::apps::parse_flags(args) {
        Ok((pos, flags)) => finish(run(&pos, &flags)),
        Err(e) => usage_error(cmd, &format!("{e}. {usage}")),
    }
}

/// Reports a mistake in the command line and where to read about it. An empty
/// `cmd` points at the overview.
fn usage_error(cmd: &str, msg: &str) -> ExitCode {
    let st = crate::style::Style::for_stderr();
    let more = if cmd.is_empty() { "pacdeb --help".to_string() } else { format!("pacdeb {cmd} --help") };
    eprintln!("{} {msg}\nRun '{more}' for details.", st.bad("pacdeb:"));
    ExitCode::from(2)
}

fn finish(result: Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{} {e}", crate::style::Style::for_stderr().bad("pacdeb:"));
            ExitCode::FAILURE
        }
    }
}
