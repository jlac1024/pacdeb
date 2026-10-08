//! Help pages: one overview and one page per command.

pub const OVERVIEW: &str = "\
pacdeb: turn Debian .deb packages into pacman packages and keep them updated

Usage: pacdeb <command> [options]

Working with a .deb file:
  inspect <file.deb>            Show what is in a deb and how it would convert
  convert <file.deb>            Build a pacman package without installing it
  install <file.deb|app>        Build and install with sudo pacman -U

Keeping apps updated:
  add <app>                     Track an app (proton-mail, example-app built in)
  set [app]                     Change a tracked app, or the global channel
  list                          Show tracked apps and their versions
  check [app]                   Report available updates, download nothing
  packages <app>                List everything in an app's apt repository
  update [app]                  Download, build and install anything newer
  remove <app>                  Stop tracking an app (does not uninstall it)
  timer enable|disable|status   Check on a schedule and notify about updates
  repo init|status|remove       Publish builds to a local pacman repository

Run 'pacdeb help <command>' or 'pacdeb <command> --help' for details.

Options:
  -h, --help                    Show this help
  -V, --version                 Show the version

Environment:
  PACDEB_HOME          Keep config, state and cache under one directory
  PACDEB_INSTALL_CMD   Command run instead of 'sudo pacman -U'
  PACDEB_GITHUB_TOKEN  Token for GitHub API requests (raises the rate limit)
  NO_COLOR             Turn off colored output

Files:
  ~/.config/pacdeb/apps.toml     Tracked apps and settings
  ~/.config/pacdeb/depmap.toml   Your own Debian to Arch dependency names
  ~/.local/share/pacdeb/         Built versions and pkgrel counters
  ~/.cache/pacdeb/               Downloaded debs, build dirs, finished packages
";

const INSPECT: &str = "\
Usage: pacdeb inspect <file.deb>

Shows the deb's control fields, a summary of its files, its maintainer scripts,
and how each dependency maps to an Arch package. Changes nothing.
";

const CONVERT: &str = "\
Usage: pacdeb convert <file.deb> [--dry-run] [--direct] [--out <dir>]

Builds a pacman package from the deb without installing it. Every warning
(unmapped dependencies, script lines pacdeb could not translate) is listed.

Options:
  --dry-run      Show what would be built and every warning, build nothing
  --direct       Write the package directly instead of running makepkg
                 (used anyway when makepkg is missing)
  --out <dir>    Put the package here instead of ~/.cache/pacdeb/packages
";

const INSTALL: &str = "\
Usage: pacdeb install <file.deb|app> [--direct]

Builds a package and installs it with 'sudo pacman -U'. You see the sudo prompt
and pacman's own confirmation.

  <file.deb>   Convert this file. If no tracked app has its package name, it is
               tracked as a manual app so 'list' and 'update' know about it.
  <app>        Install the newest version from a tracked app's source, reusing
               the last build when it is already current.

Options:
  --direct     Write the package directly instead of running makepkg

If pacman fails, the built package stays in the cache and its path is printed.
";

const ADD: &str = "\
Usage: pacdeb add <app> [--preset <name> | --source <type> <source options>] [options]

Starts tracking an app so 'check' and 'update' look for new versions.
Known apps need nothing else: 'pacdeb add proton-mail', 'pacdeb add example-app'.

Sources:
  direct   A .deb at a URL, with an optional feed that names the newest version
    --url <url>               Download URL; may contain {version}
    --feed <url>              Page or JSON file that names the newest version
    --version-json <path>     Where the version is in a JSON feed, e.g. Release.Version
    --version-regex <regex>   Versions in a text feed; the first group is the version
    --version-pattern <text>  Simpler form, e.g. app_{version}_amd64.deb
    --url-json <path>         Where the download URL is in a JSON feed
    --checksum-json <path>    Where a sha256 or sha512 is in a JSON feed
  apt      A Debian package repository
    --repo <url>  --suite <s>  --component <c>  [--package <p>]  [--arch <a>]
    --key-url <url>           Signing key to download
    --key-fingerprint <fpr>   Refuse the key unless it has this fingerprint
    --key <file>              Use a key file you already have
  github   Release assets on GitHub
    --repo <owner/name>  --asset <pattern, e.g. *_amd64.deb>  [--prerelease]
  manual   No source; update with 'pacdeb update <app> --file <deb>'

Options for any app:
  --channel <name>           Release channel, filled in for {channel} in any value
  --pkgname <name>           Package name to build instead of the deb's. Without it,
                             a name a repo or the AUR also uses gets a -deb suffix
  --provides a,b             Extra provides, e.g. to stand in for an AUR package
  --conflicts a,b            Packages this one replaces
  --depends a,b              Dependencies to add
  --no-depends a,b           Dependencies to leave out
";

const SET: &str = "\
Usage: pacdeb set <app> [options]
       pacdeb set --channel <name>

Changes a tracked app. Takes the same options as 'add' (see 'pacdeb help add');
only the ones given change. '--source <type>' replaces the whole source.
An empty value clears a setting, e.g. --channel ''.

Without an app name, sets the global channel used by apps that have none of
their own. Proton Mail's channels are Stable, EarlyAccess and Alpha.
";

const LIST: &str = "\
Usage: pacdeb list

Shows each tracked app with its source, channel, the last version pacdeb built,
and the version pacman has installed.
";

const CHECK: &str = "\
Usage: pacdeb check [app] [--notify]

Asks each app's source (or just this app's) for its newest version and
compares it with what pacdeb last built. Downloads no debs.

Options:
  --notify   Also send a desktop notification when the updates found differ
             from the last ones notified about (the timer uses this)
";

const UPDATE: &str = "\
Usage: pacdeb update [app] [--file <file.deb>] [--no-install] [--direct]

Downloads anything newer, checks its checksum, builds it, and installs all new
packages with one 'sudo pacman -U'. Without an app name, updates every tracked app.

Options:
  --file <file.deb>  Use this deb for the app instead of downloading
                     (how manual apps are updated)
  --no-install       Build only
  --direct           Write packages directly instead of running makepkg

The last two builds of each app are kept in ~/.cache/pacdeb/packages, so you can
go back with 'sudo pacman -U <older package>'.
";

const REMOVE: &str = "\
Usage: pacdeb remove <app>

Stops tracking an app. The installed package stays; remove it with
'sudo pacman -R <package>' if you want it gone.
";

const TIMER: &str = "\
Usage: pacdeb timer enable|disable|status

Runs 'pacdeb timer run' 5 minutes after login and every 6 hours, using a systemd
user timer in ~/.config/systemd/user (no root needed).

With a repository (see 'pacdeb help repo'), new versions are downloaded and built
in the background and published there, and a notification says they are ready;
your normal system update installs them. Without one, you get a notification
whose Update button opens a terminal running 'pacdeb update'. Each set of
updates is announced once.

  enable    Turn scheduled checks on
  disable   Turn them off and remove the timer
  status    Show whether they are on and when the next check runs
  run       What the timer runs; can be run by hand

The terminal is $TERMINAL when set, otherwise the first one found (konsole,
gnome-terminal, alacritty, kitty, foot and others).
";

const REPO: &str = "\
Usage: pacdeb repo init [dir]
       pacdeb repo status
       pacdeb repo remove

Keeps a local pacman repository of everything pacdeb builds, so pacdeb apps
update with the rest of the system: pacman -Syu, the CachyOS updater, Shelly and
AUR helpers all read the repositories in pacman.conf.

  init [dir]   Set up the repository in dir (default /var/lib/pacdeb/repo), make
               a signing key only pacdeb uses, publish the current builds, and
               print the steps that tell pacman to trust it. The folder must
               exist and be yours: sudo install -d -o \"$USER\" -m 755 <dir>
  status       Show the packages in it and whether pacman.conf lists it
  remove       Stop publishing to it (the folder is left)

setup.sh in the project folder does the whole setup, sudo steps included, and
'setup.sh --remove' undoes it.

The folder is outside your home because pacman downloads as the 'alpm' user.
Packages and the database are signed; pacman refuses anything pacdeb did not sign.
";

const PACKAGES: &str = "\
Usage: pacdeb packages <app>

Lists every package in the apt repository a tracked app comes from, with its
newest version and a short description, after checking the repository's
signature. Tracked packages are marked with *. Any of them can be tracked too,
with the 'pacdeb add' line printed at the end.
";

/// The help page for a command, or None if there is no such command.
pub fn page(command: &str) -> Option<&'static str> {
    Some(match command {
        "inspect" => INSPECT,
        "convert" => CONVERT,
        "install" => INSTALL,
        "add" => ADD,
        "set" => SET,
        "list" => LIST,
        "check" => CHECK,
        "update" => UPDATE,
        "remove" => REMOVE,
        "timer" => TIMER,
        "repo" => REPO,
        "packages" => PACKAGES,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMANDS: &[&str] = &["inspect", "convert", "install", "add", "set", "list", "check", "update", "remove", "timer", "repo", "packages"];

    #[test]
    fn every_command_has_a_page_in_the_overview() {
        for c in COMMANDS {
            let p = page(c).unwrap_or_else(|| panic!("no help for {c}"));
            assert!(p.starts_with(&format!("Usage: pacdeb {c}")), "{c}");
            assert!(OVERVIEW.contains(&format!("  {c} ")), "{c} missing from the overview");
        }
        assert_eq!(page("nope"), None);
    }

    #[test]
    fn pages_fit_a_terminal_and_avoid_dashes() {
        for text in COMMANDS.iter().filter_map(|c| page(c)).chain([OVERVIEW]) {
            for line in text.lines() {
                assert!(line.chars().count() <= 88, "too wide: {line}");
                assert!(!line.contains('\u{2014}'), "em-dash: {line}");
            }
        }
    }
}
