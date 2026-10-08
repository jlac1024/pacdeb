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
#
# Run as yourself, it asks for your password at the start. Run with sudo, it never
# stops to ask: the root steps run directly, and the build and your settings run as
# the account that ran sudo.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
sudo="${SUDO-sudo}"
# Tests point this at a stub.
setup_cmd="${PACDEB_SETUP_CMD:-/usr/bin/pacdeb-setup}"
step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
as_root() { if [[ -n "$sudo" ]]; then "$sudo" "$@"; else "$@"; fi; }
fail() {
    echo "install.sh: $*" >&2
    exit 1
}

command -v pacman > /dev/null || fail "pacman was not found; pacdeb is for CachyOS and Arch Linux"
# makepkg refuses to run as root, and pacdeb's settings belong to an account, so as
# root the build runs as the account that ran sudo.
if [[ "${PACDEB_TEST_EUID:-$EUID}" -eq 0 ]]; then
    user="${SUDO_USER-}"
    [[ -n "$user" && "$user" != root ]] || fail "run it as yourself, or with sudo from your own account, not from a root shell"
    sudo=""
    read -r -a runuser <<< "${RUNUSER_CMD:-runuser -u}"
    as_user() { "${runuser[@]}" "$user" -- env HOME="$(getent passwd "$user" | cut -d: -f6)" "$@"; }
else
    as_user() { "$@"; }
fi

# Asks for the password once, up front, then keeps sudo's timestamp fresh while the
# build runs. The build takes long enough for the timestamp to run out before
# pacman -U, which would leave a password prompt waiting at the end.
ask_password_now() {
    [[ -n "$sudo" ]] || return 0
    echo "pacdeb needs your password to install packages; it asks once now."
    "$sudo" -v || fail "sudo did not accept the password"
    while sleep 60; do "$sudo" -n -v 2> /dev/null || exit; done &
    keepalive=$!
    trap 'kill "$keepalive" 2> /dev/null' EXIT
}

install_pacdeb() {
    ask_password_now

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
        as_user rustup default stable
    fi

    step "Building the pacdeb package"
    local out="$root/build/packages"
    as_user mkdir -p "$out"
    as_user env -C "$root/packaging" BUILDDIR="$root/build/makepkg" PKGDEST="$out" makepkg --force --cleanbuild
    local pkg
    pkg="$(as_user env -C "$root/packaging" PKGDEST="$out" makepkg --packagelist | grep -v -- '-debug-' | head -1)"
    [[ -f "$pkg" ]] || fail "makepkg finished but $pkg is missing"

    step "Installing $(basename "$pkg")"
    as_root pacman -U "$pkg"

    step "Setting up the system"
    "$setup_cmd"

    step "Done"
    echo "pacdeb $(/usr/bin/pacdeb --version | head -1 | cut -d' ' -f2-) is installed."
    echo "Try 'pacdeb update' in a new terminal, or open pacdeb from the app menu."
}

uninstall_pacdeb() {
    ask_password_now
    if [[ -x "$setup_cmd" ]]; then
        step "Undoing the system setup"
        "$setup_cmd" --remove
    elif [[ -x "$root/deploy/pacdeb" ]]; then
        step "Undoing the system setup"
        as_user "$root/setup.sh" --remove
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
