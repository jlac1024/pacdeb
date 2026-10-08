# pacdeb

pacdeb installs Debian `.deb` packages on CachyOS and Arch Linux by turning them into
real pacman packages, and keeps them updated, much like apt does on Debian. It is made
for apps that only ship a `.deb` for Linux, such as Proton Mail and Signal, and
works with any deb.

What you get is a normal pacman package. It shows up in `pacman -Q`, pacman owns and
removes its files, and its launcher and icons appear in your app menu. With the local
repository set up, your usual system update (the CachyOS updater, Shelly or
`pacman -Syu`) installs new versions too.

```
pacdeb update                  # check every source for new versions
pacdeb upgrade                 # build and install everything newer
pacdeb install example-app  # install by name
pacdeb search editor           # search your saved apt repositories
pacdeb remove proton-mail      # uninstall
```

There is also an app, **pacdeb-gui**, that does the same with a window.

## Getting started

You need Rust stable (`rustup default stable`). Then, from the project folder:

```
build/build.sh
./setup.sh
```

`build/build.sh` builds `deploy/pacdeb` (the command) and `deploy/pacdeb-gui` (the app).
`./setup.sh` does the one-time system setup, asking for your password only for the
steps that need root:

1. Creates the local repository folder, `/var/lib/pacdeb/repo`. It lives outside your
   home folder because pacman downloads as the `alpm` user.
2. Sets up the repository and a signing key that only pacdeb uses.
3. Tells pacman to trust that key.
4. Adds a `[pacdeb]` section to the end of `/etc/pacman.conf`, keeping a backup.
5. Turns on scheduled update checks.
6. Adds pacdeb to the app menu and as an "Open with" choice for `.deb` files.
7. Links `pacdeb` and `pacdeb-gui` into `~/.local/bin` and installs tab completion for
   fish, bash and zsh.

Steps already done are skipped, so it is safe to run again after an update.
`./setup.sh --remove` undoes all of it.

At runtime pacdeb uses `makepkg` to build packages (or writes them itself with
`--direct`, or when makepkg is missing), `sudo` and `pacman` to install and remove,
`gpg` and `gpgv` to check signatures, and `notify-send` for notifications. All are part
of a normal CachyOS install. The app needs GTK 4 and libadwaita, which are too.

## Using it like apt

| Command | What it does |
|---|---|
| `update` | Checks every source and saved apt repository for new versions and says what can be upgraded. Downloads no packages. |
| `upgrade [app]` | Downloads, builds and installs everything newer, with one `sudo pacman -U`. |
| `install <name>` | Installs a tracked app, a built in one (`proton-mail`, `example-app`), or the newest package of that name in your saved apt repositories, and starts tracking it. `name/repository` picks the repository. |
| `install <file.deb>` | Converts and installs a deb you downloaded. |
| `search <words>` | Searches the packages in your saved apt repositories. |
| `list [--upgradable]` | Shows tracked apps with their built and installed versions. |
| `remove <app>...` | Uninstalls with `sudo pacman -R` and stops tracking. |
| `untrack <app>` | Stops tracking an app but leaves it installed. |

pacdeb never asks for or stores your password. Installing and removing run `sudo pacman`
in your terminal, so you see the usual sudo prompt and pacman's own confirmation. If
pacman stops, nothing is installed or removed. Downloads and builds show a progress bar.

`pacdeb help <command>` explains every command and option, and Tab completes commands,
app names, repository names and package names.

## Where apps come from

Every tracked app has one source.

**Saved apt repositories.** Save a vendor's apt repository once, from the line in their
install instructions, then track any of its packages:

```
pacdeb apt add --line 'deb [arch=amd64] https://example.com/apt stable main' \
    --key-url https://example.com/key.asc --key-fingerprint <fingerprint>
pacdeb apt packages example
pacdeb install some-app
```

A `.list` or `.sources` file works too (`--file`), including a key pasted into a
`.sources` file. pacdeb checks the repository's signature with its key, every
download against the checksum the signed index gives, and refuses a Release file past
its Valid-Until date, as apt does. `--key-fingerprint` pins the key, so a different key
is refused.

| Command | What it does |
|---|---|
| `apt list` | Saved repositories, the apps using them, and warnings |
| `apt show <name>` | Health: publisher, last update, valid until, what it offers, the key and when it expires |
| `apt edit <name>` | Changes the URL, suite, components or architecture for every app using it |
| `apt key <name>` | Fetches the signing key again or replaces it |
| `apt remove <name>` | Deletes a saved repository |
| `apt packages <name>` | Lists everything it offers |

Changes are checked against the live repository and only saved if they work.

**Download links and feeds.** A `.deb` at a fixed address, or one a version feed (JSON,
or a web page read with a regex) points to:

```
pacdeb add some-app --source direct --feed https://example.com/latest.json \
    --version-json version --url 'https://example.com/some-app_{version}_amd64.deb'
```

**GitHub releases.**

```
pacdeb add some-app --source github --repo owner/name --asset '*_amd64.deb'
```

**By hand.** Apps installed from a file you downloaded are upgraded with
`pacdeb upgrade <app> --file <new.deb>`.

Apps with release channels, like Proton Mail's Stable, EarlyAccess and Alpha, take
`--channel`, or follow the one set with `pacdeb set --channel <name>`.

## Updates with the rest of the system

With `./setup.sh` done, pacdeb checks for new versions 5 minutes after login and every
6 hours. New versions are downloaded, built and signed in the background and wait in
the local `[pacdeb]` repository, and a notification says they are ready. Your next
system update installs them like any other package. `pacdeb timer disable` turns the
checks off.

The last two builds of each app stay in `~/.cache/pacdeb/packages`. To go back a
version, install the older one with `sudo pacman -U <package>`.

## The app

`pacdeb-gui` (in the app menu as pacdeb) has five tabs:

- **Apps**: tracked apps and their versions, with Update, Upgrade all, and an Upgrade
  button on each app that has a new version. Apps can be added, edited, uninstalled or
  untracked.
- **Search**: every package in your saved apt repositories and the built in apps, each
  with an Install button.
- **Sources**: saved apt repositories with their health, to browse, manage or add (by
  pasting the vendor's apt line), plus the feeds and GitHub projects apps come from.
- **Convert**: open or drop a `.deb` to see what pacdeb would build from it, then build
  or install it.
- **Settings**: the release channel, scheduled checks, the local repository and the
  version.

The app shows what will change and asks before installing or removing anything, then
uses the system's password dialog. Everything else runs the `pacdeb` command, so both
behave the same.

## What converting a deb does

- Moves files to Arch's layout (`/bin`, `/lib` and multiarch folders into `/usr/bin`
  and `/usr/lib`) and leaves out apt's own files.
- Maps Debian dependency names to Arch package names, checking them against the Arch
  repositories. Names it cannot map are reported and left out, never guessed. Your own
  names go in `~/.config/pacdeb/depmap.toml`:
  ```
  [map]
  "libfoo1" = "foo"
  "libbar-dev" = ""        # not needed on Arch
  ```
- Reads the package's install scripts instead of running them. Known steps (symlinks,
  permissions, system users, update-alternatives) become part of the package, steps
  pacman already handles are skipped, and anything else is shown to you.
- Names the package `<name>-deb` when an Arch repository or the AUR has a package with
  the same name, so a system update cannot replace your build with that one. It still
  provides the original name, so anything that needs the app finds it. `--pkgname`
  picks a name yourself.
- Keeps Electron apps working: `chrome-sandbox` gets its setuid bit, and binaries are
  not stripped.

`pacdeb inspect <file.deb>` shows what is in a deb, and `pacdeb convert --dry-run
<file.deb>` shows everything converting it would do, including every warning.

## Files

| Path | Holds |
|---|---|
| `~/.config/pacdeb/` | `apps.toml` (tracked apps, saved repositories, settings), `depmap.toml`, signing keys |
| `~/.local/share/pacdeb/` | What was built and what each source last offered |
| `~/.cache/pacdeb/` | Downloaded debs, package lists, build folders, finished packages |
| `/var/lib/pacdeb/repo` | The local pacman repository |

`PACDEB_HOME=<dir>` puts the first three under one folder. `pacdeb --help` lists the
other environment settings.

## License

pacdeb is free software under the GNU Affero General Public License, version 3 or (at
your option) any later version. See [LICENSE](LICENSE). It comes with no warranty.

Copyright (C) 2026 Jeff
