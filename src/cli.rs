use std::process::ExitCode;

const USAGE: &str = "\
ferry: turn Debian .deb packages into pacman packages and keep them updated

Usage: ferry <command> [options]

Commands:
  inspect <file.deb>                 Show control fields, files, scripts and dep mapping
  convert <file.deb> [--direct] [--out <dir>]
                                     Build a pacman package without installing it
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

const COMMANDS: &[&str] = &[
    "inspect", "convert", "install", "add", "list", "check", "update", "remove",
];

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
        cmd if COMMANDS.contains(&cmd) => {
            eprintln!("ferry: '{cmd}' is not implemented yet");
            ExitCode::FAILURE
        }
        other => {
            eprintln!("ferry: unknown command '{other}'. Run 'ferry --help' for the list.");
            ExitCode::from(2)
        }
    }
}
