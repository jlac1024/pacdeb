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

This puts the binary at `deploy/pacdeb`. Run it with `./run.sh`, or copy it somewhere on
your `PATH`, for example `~/.local/bin`.

At runtime pacdeb uses `makepkg` to build packages (it writes them itself with
`--direct`, or when makepkg is missing), `sudo` and `pacman` to install, and `gpg` and `gpgv`
(from the `gnupg` package) to check apt repository signatures.

## Quick start

Convert and install a deb you downloaded:

```
pacdeb install ~/Downloads/some-app_1.2.3_amd64.deb
```

Track an app so it stays updated. Proton Mail and Example App are built in:

```
pacdeb add example-app
pacdeb add proton-mail --channel Stable
pacdeb update
```

Later, see what is new and install it:

```
pacdeb check
pacdeb update
```

pacdeb never handles your password. Installing runs `sudo pacman -U` in your terminal,
so you see the usual sudo prompt and pacman's own confirmation.

## Commands

| Command | What it does |
|---|---|
| `inspect <file.deb>` | Show what is in a deb and how it would convert |
| `convert <file.deb>` | Build a package without installing it (`--dry-run` to only report) |
| `install <file.deb\|app>` | Build and install |
| `add <app>` | Track an app |
| `set [app]` | Change a tracked app, or the global channel |
| `list` | Tracked apps with built and installed versions |
| `check [app]` | Report available updates, download nothing |
| `update [app]` | Download, build and install anything newer |
| `remove <app>` | Stop tracking an app (leaves it installed) |

`pacdeb help <command>` shows every option.

## Update sources

Each tracked app has one source:

- **direct**: a `.deb` at a URL. A feed (JSON, or a page read with a regex) can say
  which version is newest and where to download it.
  ```
  pacdeb add some-app --source direct --feed https://example.com/latest.json \
      --version-json version --url 'https://example.com/some-app_{version}_amd64.deb'
  ```
- **apt**: a Debian repository. The repository's signature is checked with `gpgv`
  against its signing key, and every download against the index's checksum.
  ```
  pacdeb add some-app --source apt --repo https://example.com/apt --suite stable \
      --component main --key-url https://example.com/key.asc --key-fingerprint <fpr>
  ```
- **github**: release assets on GitHub.
  ```
  pacdeb add some-app --source github --repo owner/name --asset '*_amd64.deb'
  ```
- **manual**: no source. Update with `pacdeb update <app> --file <new.deb>`.

Apps with channels (like Proton Mail's Stable, EarlyAccess and Alpha) take
`--channel`, or use the global one from `pacdeb set --channel <name>`.

The last two builds of each app stay in `~/.cache/pacdeb/packages`. To go back a
version, install the older one with `sudo pacman -U <package>`.

## What conversion does

- Moves files to Arch's layout (`/bin`, `/lib` and multiarch dirs into `/usr/bin` and
  `/usr/lib`) and drops apt's own files.
- Maps Debian dependency names to Arch package names, checking them against the repos.
  Names it cannot map are reported and left out, never guessed.
- Reads the maintainer scripts instead of running them. Known steps (symlinks,
  permissions, system users, update-alternatives) become part of the package, steps
  pacman hooks already handle are skipped, and anything else is shown to you.
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
