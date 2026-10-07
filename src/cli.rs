use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::error::Result;

const USAGE: &str = "\
ferry: turn Debian .deb packages into pacman packages and keep them updated

Usage: ferry <command> [options]

Commands:
  inspect <file.deb>                 Show control fields, files, scripts and dep mapping
  convert <file.deb> [--direct] [--out <dir>] [--dry-run]
                                     Build a pacman package without installing it;
                                     --dry-run prints what would be built
  install <file.deb|name> [--direct] Convert and install with sudo pacman -U
  add <name> --source <direct|apt|github|manual> ...
                                     Register an app for updates
  list                               Show registered apps and their versions
  check [name]                       Report available updates, download nothing
  update [name] [--file <file.deb>] [--direct] [--no-install]
                                     Fetch, convert and install anything newer
  remove <name>                      Stop tracking an app (does not uninstall)

Options:
  -h, --help                         Show this help
  -V, --version                      Show the version

Environment:
  FERRY_HOME          Put config, state and cache under one directory
  FERRY_INSTALL_CMD   Command used instead of sudo pacman -U
  FERRY_GITHUB_TOKEN  Token for GitHub API requests
";

const UNFINISHED: &[&str] = &["add", "list", "check", "update", "remove"];

pub fn run(args: &[String]) -> ExitCode {
    let Some(first) = args.first() else {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    };

    match first.as_str() {
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        "-V" | "--version" => {
            println!("ferry {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "inspect" => inspect(&args[1..]),
        "convert" => convert(&args[1..]),
        "install" => install(&args[1..]),
        cmd if UNFINISHED.contains(&cmd) => {
            eprintln!("ferry: '{cmd}' is not implemented yet");
            ExitCode::FAILURE
        }
        other => {
            eprintln!("ferry: unknown command '{other}'. Run 'ferry --help' for the list.");
            ExitCode::from(2)
        }
    }
}

fn inspect(args: &[String]) -> ExitCode {
    let [path] = args else {
        return usage_error("usage: ferry inspect <file.deb>");
    };
    if path.starts_with('-') {
        return usage_error(&format!("unknown option '{path}'. Usage: ferry inspect <file.deb>"));
    }
    finish(crate::inspect::run(Path::new(path)))
}

const CONVERT_USAGE: &str = "usage: ferry convert <file.deb> [--direct] [--out <dir>] [--dry-run]";

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
                None => return usage_error(&format!("--out needs a directory. {CONVERT_USAGE}")),
            },
            s if s.starts_with('-') => return usage_error(&format!("unknown option '{s}'. {CONVERT_USAGE}")),
            s if deb.is_none() => deb = Some(PathBuf::from(s)),
            s => return usage_error(&format!("unexpected argument '{s}'. {CONVERT_USAGE}")),
        }
    }
    let Some(deb) = deb else {
        return usage_error(CONVERT_USAGE);
    };
    let (dry_run, direct, out) = opts;
    finish(crate::convert::run(&crate::convert::Options { deb, dry_run, direct, out }))
}

const INSTALL_USAGE: &str = "usage: ferry install <file.deb|name> [--direct]";

fn install(args: &[String]) -> ExitCode {
    let mut target = None;
    let mut direct = false;
    for a in args {
        match a.as_str() {
            "--direct" => direct = true,
            s if s.starts_with('-') => return usage_error(&format!("unknown option '{s}'. {INSTALL_USAGE}")),
            s if target.is_none() => target = Some(PathBuf::from(s)),
            s => return usage_error(&format!("unexpected argument '{s}'. {INSTALL_USAGE}")),
        }
    }
    let Some(target) = target else {
        return usage_error(INSTALL_USAGE);
    };
    if !target.exists() && target.extension().is_none_or(|e| e != "deb") {
        eprintln!("ferry: installing an app by name needs the app registry, which is not implemented yet; pass a .deb file");
        return ExitCode::FAILURE;
    }
    finish(crate::convert::build_package(&target, direct, None).and_then(|pkg| {
        println!("{} {}", crate::style::Style::for_stdout().good("Built"), pkg.display());
        crate::install::install(&pkg)
    }))
}

fn usage_error(msg: &str) -> ExitCode {
    eprintln!("ferry: {msg}");
    ExitCode::from(2)
}

fn finish(result: Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{} {e}", crate::style::Style::for_stderr().bad("ferry:"));
            ExitCode::FAILURE
        }
    }
}
