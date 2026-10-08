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
# once: save a vendor's apt repository, from the line in their install instructions
pacdeb apt add --line 'deb https://example.com/apt stable main' --key-url https://example.com/key.asc

pacdeb search editor           # search your saved repositories
pacdeb install some-app        # install by name
pacdeb update                  # check every source for new versions
pacdeb upgrade                 # build and install everything newer
pacdeb remove some-app         # uninstall
```

There is also an app, **pacdeb-gui**, that does the same with a window.

## Getting started

From the project folder:

```
./install.sh
```

It installs what is needed to build pacdeb (base-devel, git, and Rust through rustup)
if they are missing, builds pacdeb as a pacman package and runs its tests, installs it
with `sudo pacman -U`, and then sets up the system with `pacdeb-setup`:

1. Creates the local repository folder, `/var/lib/pacdeb/repo`. It lives outside your
   home folder because pacman downloads as the `alpm` user.
2. Sets up the repository and a signing key that only pacdeb uses.
3. Tells pacman to trust that key.
4. Adds a `[pacdeb]` section to the end of `/etc/pacman.conf`, keeping a backup.
5. Turns on scheduled update checks.

The package puts `pacdeb`, `pacdeb-gui` and `pacdeb-setup` in `/usr/bin`, adds pacdeb
to the app menu (and as an "Open with" choice for `.deb` files), and installs tab
completion for fish, bash and zsh. It asks for your password only for the steps that
need root. To install a newer version, update the source and run `./install.sh` again;
steps already done are skipped. `./install.sh --uninstall` undoes the setup and
removes the package, leaving the apps pacdeb installed in place.

At runtime pacdeb uses `makepkg` to build packages (or writes them itself with
`--direct`), `sudo` and `pacman` to install and remove, `gpg` and `gpgv` to check
signatures, and `notify-send` for notifications. The package depends on all of them.

**Working on pacdeb itself.** `build/build.sh` builds `deploy/pacdeb` and
`deploy/pacdeb-gui` without installing anything, and `./setup.sh` sets up the system
around them, linking both into `~/.local/bin`. `packaging/PKGBUILD` is the recipe
`install.sh` uses.

## Using it like apt

| Command | What it does |
|---|---|
| `update` | Checks every source and saved apt repository for new versions and says what can be upgraded. Downloads no packages. |
| `upgrade [app]` | Downloads, builds and installs everything newer, with one `sudo pacman -U`. |
| `install <name>` | Installs a tracked app, or the newest package of that name in your saved apt repositories and starts tracking it. `name/repository` picks the repository. |
| `install <file.deb>` | Converts and installs a deb you downloaded. |
| `search <words>` | Searches the packages in your saved apt repositories. |
| `show <name>` | Shows a package's details, and what its dependencies become on Arch, before you install it. |
| `list [--upgradable]` | Shows tracked apps with their built and installed versions. |
| `remove <app>...` | Uninstalls with `sudo pacman -R` and stops tracking. |
| `untrack <app>` | Stops tracking an app but leaves it installed. |

pacdeb comes with no apps or repositories of its own: you add the apt repositories
and download sources you trust (see below), and from then on it keeps those apps
updated.

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
- **Search**: every package in your saved apt repositories, each with an Install
  button.
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

## What it can and cannot do

pacdeb works best for desktop apps that bring what they need with them: Electron apps,
Chromium-based browsers, and apps that bundle their own libraries. It has been tested
with Proton Mail, Google Chrome, Microsoft Edge, Brave, Signal, Element,
Spotify, 1Password, Mullvad VPN, Discord, Zoom, Obsidian, Bitwarden, RustDesk, VS Code
and Chrome Remote Desktop. Things to know:

- **It does not run Debian's install scripts.** It reads them and turns the steps it
  understands into parts of the package. Steps it does not understand are skipped and
  listed as warnings, so an app that depends on one of them may need setting up by hand.
  Read the warnings the first time you install an app.
- **Some dependencies have no Arch name.** pacdeb maps Debian package names to Arch
  ones and checks them against the Arch repositories. A name it cannot map is left out
  and reported, never guessed. An app built against a library version Arch does not
  ship (for example an older ffmpeg) may then not start.
- **Background services are not started for you.** For apps with a system service,
  like Mullvad VPN and RustDesk, pacdeb prints the `systemctl` command that starts it.
- **It is not for system software.** Kernel modules, drivers, libraries that other
  packages use, and desktop environments belong in Arch's own packages. Debian-specific
  security policies (AppArmor, SELinux) are not carried over.
- **There is no dpkg.** Apps that call `dpkg` or `apt` themselves at runtime will not
  find them. Apps that update themselves in their own folder, like Discord, still do.
- **apt support covers what apps need**: signed binary repositories over http or https.
  Unsigned repositories, source packages and apt's pinning and priorities are not
  supported. Only x86_64 has been tested; arm64 should work but has not been tried.
- **Downloads are only as trustworthy as their source.** Repository indexes are checked
  against their signing key, and packages against the checksums the index or the
  vendor publishes. A plain download link with no published checksum is only protected
  by HTTPS. pacdeb repackages the vendor's binaries; it does not audit them.
- **Package names can change.** When an Arch repository or the AUR already has a
  package with the app's name, pacdeb's build gets a `-deb` suffix, as described below.

### Chrome Remote Desktop

Chrome Remote Desktop installs and its host starts, but on CachyOS it does not work the
way it does on Ubuntu, and we do not recommend it:

- It cannot show your current desktop. On KDE Plasma it starts a separate, new desktop
  session in a virtual display, and connecting to the session you are already logged
  in to is not supported.
- That separate session needs Plasma's X11 session (`plasma-x11-session`) installed.
- pacdeb works around two Debian assumptions automatically: Arch's X server only lets
  console users start it, so the host uses a virtual X server (Xvfb) instead, and
  Debian's `/etc/X11/Xsession`, which the host runs to start a desktop, does not exist
  on Arch, so pacdeb provides one. Even so, expect rough edges.

To reach your actual desktop remotely on KDE, use KDE's own remote desktop (`krdp`,
in System Settings under Remote Desktop) with any RDP client, over a VPN such as
Tailscale if you connect from outside your network.

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

Copyright (C) 2026 Jeff LaCombe (jlac1024)
