#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Installs pacdeb's files into a package folder. Both PKGBUILDs call it, so the local
# build and the release package hold the same files.
#
# Usage: package.sh <folder with the pacdeb binaries> <source folder> <package folder>
set -euo pipefail

bin="$1"
src="$2"
dest="$3"

install -Dm755 "$bin/pacdeb" "$dest/usr/bin/pacdeb"
install -Dm755 "$bin/pacdeb-gui" "$dest/usr/bin/pacdeb-gui"
install -Dm755 "$src/setup.sh" "$dest/usr/bin/pacdeb-setup"

install -d "$dest/usr/share/fish/vendor_completions.d" "$dest/usr/share/bash-completion/completions" "$dest/usr/share/zsh/site-functions"
"$bin/pacdeb" completions fish > "$dest/usr/share/fish/vendor_completions.d/pacdeb.fish"
"$bin/pacdeb" completions bash > "$dest/usr/share/bash-completion/completions/pacdeb"
"$bin/pacdeb" completions zsh > "$dest/usr/share/zsh/site-functions/_pacdeb"

install -Dm644 "$src/packaging/pacdeb.desktop" "$dest/usr/share/applications/pacdeb.desktop"
install -Dm644 "$src/LICENSE" "$dest/usr/share/licenses/pacdeb/LICENSE"
install -Dm644 "$src/README.md" "$dest/usr/share/doc/pacdeb/README.md"
