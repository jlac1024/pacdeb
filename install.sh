#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Installs pacdeb on CachyOS (or Arch Linux) in one go:
#   1. the tools to build it (base-devel, git, and Rust through rustup), when missing
#   2. builds the pacdeb package from this folder, running its tests
#   3. installs it with pacman, which asks you to confirm
#   4. sets up the system with pacdeb-setup: the local repository and its signing
#      key, the [pacdeb] section in pacman.conf, and scheduled updates
# Run it again after updating the source to install the new version; steps already
# done are skipped. 'install.sh --uninstall' undoes the setup and removes pacdeb.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
sudo="${SUDO-sudo}"
step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
as_root() { if [[ -n "$sudo" ]]; then "$sudo" "$@"; else "$@"; fi; }
fail() {
    echo "install.sh: $*" >&2
    exit 1
}

command -v pacman > /dev/null || fail "pacman was not found; pacdeb is for CachyOS and Arch Linux"
# makepkg refuses to run as root, and pacdeb's settings belong to your own account.
[[ $EUID -ne 0 ]] || fail "run it as yourself, not as root; it asks for your password when it needs it"

install_pacdeb() {
    step "Tools to build pacdeb"
    local need=()
    pacman -Qq base-devel &> /dev/null || need+=(base-devel)
    command -v git > /dev/null || need+=(git)
    # rustup rather than the rust package: its toolchain does not depend on the
    # system's LLVM version.
    command -v cargo > /dev/null || need+=(rustup)
    if [[ ${#need[@]} -gt 0 ]]; then
        echo "    installing ${need[*]}"
        as_root pacman -S --needed "${need[@]}"
    else
        echo "    already installed"
    fi
    if command -v rustup > /dev/null && ! rustup toolchain list 2> /dev/null | grep -q .; then
        rustup default stable
    fi

    step "Building the pacdeb package"
    local out="$root/build/packages"
    mkdir -p "$out"
    (cd "$root/packaging" && BUILDDIR="$root/build/makepkg" PKGDEST="$out" makepkg --force --cleanbuild)
    local pkg
    pkg="$(cd "$root/packaging" && PKGDEST="$out" makepkg --packagelist | grep -v -- '-debug-' | head -1)"
    [[ -f "$pkg" ]] || fail "makepkg finished but $pkg is missing"

    step "Installing $(basename "$pkg")"
    as_root pacman -U "$pkg"

    step "Setting up the system"
    /usr/bin/pacdeb-setup

    step "Done"
    echo "pacdeb $(/usr/bin/pacdeb --version | head -1 | cut -d' ' -f2-) is installed."
    echo "Try 'pacdeb update' in a new terminal, or open pacdeb from the app menu."
}

uninstall_pacdeb() {
    if [[ -x /usr/bin/pacdeb-setup ]]; then
        step "Undoing the system setup"
        /usr/bin/pacdeb-setup --remove
    elif [[ -x "$root/deploy/pacdeb" ]]; then
        step "Undoing the system setup"
        "$root/setup.sh" --remove
    fi
    if pacman -Qq pacdeb &> /dev/null; then
        step "Removing the pacdeb package"
        as_root pacman -R pacdeb
    fi
    step "Done"
    echo "Apps pacdeb installed stay installed; remove them with 'sudo pacman -R <package>'."
    echo "Your pacdeb settings are kept in ~/.config/pacdeb."
}

case "${1:-}" in
    "") install_pacdeb ;;
    --uninstall) uninstall_pacdeb ;;
    -h | --help)
        echo "Usage: install.sh [--uninstall]"
        echo "Builds and installs pacdeb, then sets up the system so its apps update with it."
        ;;
    *)
        echo "Usage: install.sh [--uninstall]" >&2
        exit 2
        ;;
esac
