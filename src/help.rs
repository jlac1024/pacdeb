// SPDX-License-Identifier: AGPL-3.0-or-later
//! Help pages: one overview and one page per command.

/// The overview with the version on top.
pub fn overview() -> String {
    format!("pacdeb {}\n{OVERVIEW}", crate::version())
}

pub const OVERVIEW: &str = "\
Turns Debian .deb packages into pacman packages and keeps them updated.

Usage: pacdeb <command> [options]

Working with a .deb file:
  inspect <file.deb>            Show what is in a deb and how it would convert
  convert <file.deb>            Build a pacman package without installing it

Like apt:
  update                        Check every source for new versions, download nothing
  upgrade [app]                 Build and install everything newer
  install <name|file.deb>       Install a tracked app, a built in one, or any
                                package from the saved apt repositories
  search <words>                Search the packages in the saved apt repositories
  list [--upgradable]           Show tracked apps, or only those with updates

Managing apps and sources:
  add <app>                     Track an app (proton-mail, example-app built in)
  set [app]                     Change a tracked app, or the global channel
  remove <app>...               Uninstall apps and stop tracking them
  untrack <app>                 Stop tracking an app, leave it installed
  check [app]                   Ask the sources now, without remembering the answer
  apt list|add|show|edit|key|remove|packages
                                Manage saved apt repositories
  packages <app|repository>     List everything in an apt repository
  timer enable|disable|status   Update on a schedule and notify about new versions
  repo init|status|remove       Publish builds to a local pacman repository

Run 'pacdeb help <command>' or 'pacdeb <command> --help' for details.

Options:
  -h, --help                    Show this help
  -V, --version, version        Show the version and the build it comes from

Environment:
  PACDEB_HOME          Keep config, state and cache under one directory
  PACDEB_INSTALL_CMD   Command run instead of 'sudo pacman -U'
  PACDEB_GITHUB_TOKEN  Token for GitHub API requests (raises the rate limit)
  NO_COLOR             Turn off colored output
  PACDEB_PROGRESS      off: no progress bars; lines: progress as lines (for pacdeb-gui)

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
Usage: pacdeb install <name|name/repository|file.deb> [--direct]

Builds a package and installs it with 'sudo pacman -U', like 'apt install'.
You see the sudo prompt and pacman's own confirmation.

  <name>             A tracked app: its newest version (reusing the last build
                     when it is current). A built in app (see 'pacdeb add'): it is
                     tracked, then installed. Otherwise the newest package of that
                     name in any saved apt repository, which is then tracked.
  <name/repository>  That package from that saved apt repository.
  <file.deb>         Convert this file. If no tracked app has its package name, it
                     is tracked as a manual app so 'list' and 'upgrade' know it.

Options:
  --direct     Write the package directly instead of running makepkg

If pacman fails, the built package stays in the cache and its path is printed.
";

const SEARCH: &str = "\
Usage: pacdeb search <words>

Lists the packages in the saved apt repositories whose name or description
contains every word, with their repository and newest version, and the built in
apps that match. Uses the package lists from the last 'pacdeb update'.
Install one with 'pacdeb install <name>' or 'pacdeb install <name>/<repository>'.
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
  apt      A package from a saved apt repository (see 'pacdeb help apt')
    --apt <repository>        The saved repository (implies --source apt)
    --package <p>             The Debian package, if not the app name
    Or describe a new repository, which is saved under the app's name:
    --repo <url>  --suite <s>  --component <c>  [--arch <a>]
    --key-url <url> [--key-fingerprint <fpr>]  or  --key <file>
  github   Release assets on GitHub
    --repo <owner/name>  --asset <pattern, e.g. *_amd64.deb>  [--prerelease]
  manual   No source; upgrade with 'pacdeb upgrade <app> --file <deb>'

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
Usage: pacdeb list [--upgradable]

Shows each tracked app with its source, channel, the last version pacdeb built,
and the version pacman has installed. With --upgradable, only the apps whose
source offered something newer at the last 'pacdeb update'.
";

const CHECK: &str = "\
Usage: pacdeb check [app] [--notify]

Asks each app's source (or just this app's) for its newest version now and
prints it, without remembering the answer. 'pacdeb update' does the same for
every app and also refreshes the apt package lists. Downloads no debs.

Options:
  --notify   Also send a desktop notification when the updates found differ
             from the last ones notified about
";

const UPDATE: &str = "\
Usage: pacdeb update

Like 'apt update': reads every saved apt repository (checking its signature and
keeping its package lists for search and install) and asks every tracked app's
source for its newest version. Remembers what it found and says which apps can
be upgraded. Downloads no debs. Run 'pacdeb upgrade' to install them.
";

const UPGRADE: &str = "\
Usage: pacdeb upgrade [app] [--file <file.deb>] [--no-install] [--direct]

Like 'apt upgrade': downloads what is newer, checks its checksum, builds it, and
installs all new packages with one 'sudo pacman -U'. Uses what the last
'pacdeb update' found (asking the source again when that is over a day old).
Without an app name, upgrades every tracked app.

Options:
  --file <file.deb>  Use this deb for the app instead of downloading
                     (how manual apps are upgraded)
  --no-install       Build only
  --direct           Write packages directly instead of running makepkg

The last two builds of each app are kept in ~/.cache/pacdeb/packages, so you can
go back with 'sudo pacman -U <older package>'.
";

const REMOVE: &str = "\
Usage: pacdeb remove <app>...

Like 'apt remove': uninstalls the apps with 'sudo pacman -R' (pacman lists what
it removes and asks first), then stops tracking them. If pacman does not remove
them, nothing changes. An app's package name works too (example-app-deb).
Apps that are not installed are only untracked.

To stop tracking an app but keep it installed, use 'pacdeb untrack <app>'.
";

const UNTRACK: &str = "\
Usage: pacdeb untrack <app>

Stops tracking an app: pacdeb forgets its source and build records and takes it
out of the local repository. The installed package stays; 'pacdeb remove <app>'
uninstalls it instead.
";

const TIMER: &str = "\
Usage: pacdeb timer enable|disable|status

Runs 'pacdeb timer run' 5 minutes after login and every 6 hours, using a systemd
user timer in ~/.config/systemd/user (no root needed).

With a repository (see 'pacdeb help repo'), new versions are downloaded and built
in the background and published there, and a notification says they are ready;
your normal system update installs them. Without one, you get a notification
whose Upgrade button opens a terminal running 'pacdeb upgrade'. Each set of
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
Usage: pacdeb packages <app|repository>

Lists every package in a saved apt repository, or the one an app comes from,
with its newest version and a short description, after checking the
repository's signature. Tracked packages are marked with *. The same as
'pacdeb apt packages <repository>'.
";

const APT: &str = "\
Usage: pacdeb apt <command> ...

Saved apt repositories. Apps take packages from them by name
('pacdeb add <package> --apt <repository>'); several apps can share one.

  list
      Every saved repository, the apps using it, when it was last checked,
      and warnings (expiring keys, missing components, stale repositories).
  add [name] --line 'deb [options] <url> <suite> <components...>'
  add [name] --file <file.list|file.sources>
  add <name> <url> <suite> <components...>
      Saves a repository from a vendor's apt line or file. Needs
      --key-url <url> (pin it with --key-fingerprint <fpr>) or --key <file>,
      unless a .sources file includes the key. Without a name, one is made
      from the URL. The repository is checked before it is saved.
  show <name> [--offline]
      Details and health: who publishes it, when it was updated and until
      when its Release file is valid, what it offers, its signing key and
      when that expires, and which apps use it.
  edit <name> [--url <url>] [--suite <s>] [--components a,b] [--arch <a>]
      Changes a repository for every app using it. Saved only if the changed
      repository works.
  key <name> [--key-url <url> | --key <file>] [--key-fingerprint <fpr>]
      Fetches the signing key again (from its saved link when none is given)
      or replaces it. A pinned fingerprint must still match unless a new one
      is given, and the repository must be signed by the new key.
  remove <name> [--with-apps]
      Deletes a repository. Apps using it must be moved or removed first;
      --with-apps stops tracking them too (they stay installed).
  packages <name>
      Everything the repository offers.

pacdeb refuses a repository whose signed Release file is past its
Valid-Until date, as apt does.
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
        "upgrade" => UPGRADE,
        "search" => SEARCH,
        "remove" => REMOVE,
        "untrack" => UNTRACK,
        "timer" => TIMER,
        "repo" => REPO,
        "packages" => PACKAGES,
        "apt" => APT,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMANDS: &[&str] = &["inspect", "convert", "install", "add", "set", "list", "check", "update", "remove", "timer", "repo", "packages", "apt", "upgrade", "search", "untrack"];

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
