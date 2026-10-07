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
  add <name> [--preset <p> | --source <direct|apt|github|manual>] [options]
                                     Track an app for updates (known apps have presets)
  set <name> [options]               Change a tracked app, for example its --channel
  set --channel <name>               Set the global channel ('' clears it)
  list                               Show tracked apps and their versions
  check [name]                       Report available updates, download nothing
  update [name] [--file <file.deb>] [--direct] [--no-install]
                                     Fetch, convert and install anything newer
  remove <name>                      Stop tracking an app (does not uninstall)

Options for add and set:
  --channel <c>  --pkgname <n>  --provides a,b  --conflicts a,b
  --depends a,b (extra)  --no-depends a,b (dropped)
  direct: --url <u> (may use {version})  --feed <u>  --version-json <path>
          --version-pattern <app_{version}.deb>  --url-json <path>  --checksum-json <path>
  apt:    --repo <u>  --suite <s>  --component <c>  --package <p>  --arch <a>  --key <file>
  github: --repo <owner/name>  --asset <pattern>  --prerelease
  Values may use {channel}.

Options:
  -h, --help                         Show this help
  -V, --version                      Show the version

Environment:
  FERRY_HOME          Put config, state and cache under one directory
  FERRY_INSTALL_CMD   Command used instead of sudo pacman -U
  FERRY_GITHUB_TOKEN  Token for GitHub API requests
";



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
        "update" => update(&args[1..]),
        "add" => registry_cmd(&args[1..], "usage: ferry add <name> [--preset <p> | --source <type>] [options]", |pos, f| match pos {
            [name] => crate::apps::add(name, f),
            _ => Err(crate::error::Error::new("usage: ferry add <name> [--preset <p> | --source <type>] [options]")),
        }),
        "set" => registry_cmd(&args[1..], "usage: ferry set <name> [options], or ferry set --channel <name>", |pos, f| match pos {
            [] => crate::apps::set(None, f),
            [name] => crate::apps::set(Some(name), f),
            _ => Err(crate::error::Error::new("usage: ferry set <name> [options], or ferry set --channel <name>")),
        }),
        "list" => match &args[1..] {
            [] => finish(crate::apps::list()),
            _ => usage_error("usage: ferry list"),
        },
        "remove" => match &args[1..] {
            [name] if !name.starts_with('-') => finish(crate::apps::remove(name)),
            _ => usage_error("usage: ferry remove <name>"),
        },
        "check" => match &args[1..] {
            [] => finish(crate::apps::check(None)),
            [name] if !name.starts_with('-') => finish(crate::apps::check(Some(name))),
            _ => usage_error("usage: ferry check [name]"),
        },
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
    // A path to a file is a deb; anything else is a tracked app's name.
    if target.exists() || target.extension().is_some_and(|e| e == "deb") {
        finish(crate::update::install_file(&target, direct))
    } else {
        finish(crate::update::install_app(&target.to_string_lossy(), direct))
    }
}

const UPDATE_USAGE: &str = "usage: ferry update [name] [--file <file.deb>] [--direct] [--no-install]";

fn update(args: &[String]) -> ExitCode {
    let mut opts = crate::update::Options { name: None, file: None, direct: false, no_install: false };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--direct" => opts.direct = true,
            "--no-install" => opts.no_install = true,
            "--file" => match it.next() {
                Some(f) => opts.file = Some(PathBuf::from(f)),
                None => return usage_error(&format!("--file needs a .deb. {UPDATE_USAGE}")),
            },
            s if s.starts_with('-') => return usage_error(&format!("unknown option '{s}'. {UPDATE_USAGE}")),
            s if opts.name.is_none() => opts.name = Some(s.to_string()),
            s => return usage_error(&format!("unexpected argument '{s}'. {UPDATE_USAGE}")),
        }
    }
    finish(crate::update::update(&opts))
}

/// Parses add/set style arguments and runs the command with them.
fn registry_cmd(args: &[String], usage: &str, run: impl FnOnce(&[String], &crate::apps::Flags) -> Result<()>) -> ExitCode {
    match crate::apps::parse_flags(args) {
        Ok((pos, flags)) => finish(run(&pos, &flags)),
        Err(e) => usage_error(&format!("{e}. {usage}")),
    }
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
