# pacdeb

pacdeb turns Debian `.deb` packages into native pacman packages for CachyOS and Arch
Linux, and keeps them updated when new versions come out. It is meant for apps that
only ship a `.deb` for Linux, such as Proton Mail and Signal, but handles any deb.

The result is a normal pacman package: it shows up in `pacman -Q`, its files are owned
and removed by pacman, and its launcher and icons appear in your app menu.

## Build

Needs Rust stable (`rustup default stable`).

```
build/build.sh
```

This puts the command line tool at `deploy/pacdeb` and the app at `deploy/pacdeb-gui`
(which needs GTK 4 and libadwaita, both part of a normal CachyOS desktop). Run it with `./run.sh`, or copy it somewhere on
your `PATH`, for example `~/.local/bin`.

At runtime pacdeb uses `makepkg` to build packages (it writes them itself with
`--direct`, or when makepkg is missing), `sudo` and `pacman` to install, and `gpg` and `gpgv`
(from the `gnupg` package) to check apt repository signatures.

## The app

`deploy/pacdeb-gui` is a window for the same things: the apps pacdeb tracks and their
versions, checking and updating, adding and editing apps, the sources they come from
(apt repositories can be browsed for every package they offer, and new ones added),
converting a .deb you open or drop on it, and the settings. `./setup.sh` adds it to the app menu and makes it an
"Open with" choice for .deb files.

It asks before installing, listing each package and any installed package it replaces,
then installs through the system's password dialog (pkexec). Everything else runs the
`pacdeb` command line tool next to it, so both work the same way.

## Quick start

Convert and install a deb you downloaded:

```
pacdeb install ~/Downloads/some-app_1.2.3_amd64.deb
```

It works like apt. Save the apt repositories your apps come from, then:

```
pacdeb update                  # check every source for new versions
pacdeb upgrade                 # build and install everything newer
pacdeb search editor           # search the saved repositories
pacdeb install example-app  # install by name: tracked, built in, or from a repository
```

Proton Mail and Example App are built in, so `pacdeb install proton-mail` works
with nothing set up first.

pacdeb never handles your password. Installing runs `sudo pacman -U` in your terminal,
so you see the usual sudo prompt and pacman's own confirmation.

## Commands

| Command | What it does |
|---|---|
| `inspect <file.deb>` | Show what is in a deb and how it would convert |
| `convert <file.deb>` | Build a package without installing it (`--dry-run` to only report) |
| `update` | Check every source for new versions (like `apt update`) |
| `upgrade [app]` | Build and install everything newer (like `apt upgrade`) |
| `install <name\|file.deb>` | Install by name from the tracked apps, built in apps or saved apt repositories, or a .deb file |
| `search <words>` | Search the saved apt repositories |
| `add <app>` | Track an app |
| `set [app]` | Change a tracked app, or the global channel |
| `list [--upgradable]` | Tracked apps with built and installed versions |
| `check [app]` | Ask the sources now without remembering the answer |
| `packages <app>` | List everything in an app's apt repository |
| `remove <app>...` | Uninstall apps and stop tracking them (like `apt remove`) |
| `untrack <app>` | Stop tracking an app, leave it installed |
| `timer enable` | Check after login and every 6 hours, with a desktop notification for new versions |
| `repo init` | Publish builds to a local pacman repository, so system updates install them |

`pacdeb help <command>` shows every option.

## Update sources

Each tracked app has one source:

- **direct**: a `.deb` at a URL. A feed (JSON, or a page read with a regex) can say
  which version is newest and where to download it.
  ```
  pacdeb add some-app --source direct --feed https://example.com/latest.json \
      --version-json version --url 'https://example.com/some-app_{version}_amd64.deb'
  ```
- **apt**: a package from a saved apt repository. Save the repository once, from the
  line in the vendor's install instructions, then track any of its packages:
  ```
  pacdeb apt add --line 'deb [arch=amd64] https://example.com/apt stable main' \
      --key-url https://example.com/key.asc --key-fingerprint <fpr>
  pacdeb apt packages example
  pacdeb add some-app --apt example
  ```
  The repository's signature is checked with `gpgv` against its key, every download
  against the index's checksum, and a Release file past its Valid-Until date is
  refused, as apt does. `pacdeb apt show <name>` reports its health (when it was
  updated, when the key expires, missing components); `apt edit`, `apt key` and
  `apt remove` change it for every app that uses it. See `pacdeb help apt`.
- **github**: release assets on GitHub.
  ```
  pacdeb add some-app --source github --repo owner/name --asset '*_amd64.deb'
  ```
- **manual**: no source. Upgrade with `pacdeb upgrade <app> --file <new.deb>`.

Apps with channels (like Proton Mail's Stable, EarlyAccess and Alpha) take
`--channel`, or use the global one from `pacdeb set --channel <name>`.

The last two builds of each app stay in `~/.cache/pacdeb/packages`. To go back a
version, install the older one with `sudo pacman -U <package>`.

## Updating with the rest of the system

pacdeb can keep a local pacman repository of its builds. Every updater reads the
repositories in `pacman.conf`, so `pacman -Syu`, the CachyOS updater and Shelly then
install pacdeb apps like any other package. With the timer on, new versions are built
in the background and wait in the repository for your next system update.

One-time setup, after `build/build.sh`:

```
./setup.sh
```

It creates `/var/lib/pacdeb/repo` (outside your home, because pacman downloads as the
`alpm` user), runs `pacdeb repo init`, tells pacman to trust pacdeb's signing key,
adds a `[pacdeb]` section to the end of `/etc/pacman.conf` (keeping a backup), and
turns on the timer. It asks for your password for the steps that need root, skips
steps already done, and `./setup.sh --remove` undoes all of it.

Packages and the database are signed with a key only pacdeb uses, so pacman refuses
anything else placed in that folder.

## What conversion does

- Moves files to Arch's layout (`/bin`, `/lib` and multiarch dirs into `/usr/bin` and
  `/usr/lib`) and drops apt's own files.
- Maps Debian dependency names to Arch package names, checking them against the repos.
  Names it cannot map are reported and left out, never guessed.
- Reads the maintainer scripts instead of running them. Known steps (symlinks,
  permissions, system users, update-alternatives) become part of the package, steps
  pacman hooks already handle are skipped, and anything else is shown to you.
- Names the package `<name>-deb` when a repo or the AUR has a package with the same
  name, since a system update would otherwise replace your build with that package.
  It provides and conflicts with the original name, so anything that needs the app
  still finds it. `--pkgname` picks a name yourself.
- Keeps Electron apps working: `chrome-sandbox` gets its setuid bit, and binaries are
  not stripped.

Your own dependency names go in `~/.config/pacdeb/depmap.toml`:

```
[map]
"libfoo1" = "foo"
"libbar-dev" = ""        # not needed on Arch
```

## Files

| Path | Holds |
|---|---|
| `~/.config/pacdeb/` | `apps.toml` (tracked apps, settings), `depmap.toml`, signing keys |
| `~/.local/share/pacdeb/` | Built versions and pkgrel counters |
| `~/.cache/pacdeb/` | Downloaded debs, build dirs, finished packages |

`PACDEB_HOME=<dir>` puts all three under one directory.
