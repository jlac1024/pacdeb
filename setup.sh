#!/usr/bin/env bash
# One-time system setup for pacdeb, so its apps update with the rest of the system:
#   1. the repository folder (outside home, since pacman downloads as the alpm user)
#   2. the repository and its signing key (pacdeb repo init)
#   3. pacman's trust in that key (pacman-key)
#   4. the [pacdeb] section in pacman.conf
#   5. the timer that builds new versions in the background
#   6. a start menu entry for pacdeb-gui, which also opens .deb files
# Steps already done are skipped, so it is safe to run again. sudo is used only for
# the steps that need root. 'setup.sh --remove' undoes all of it.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
pacdeb="${PACDEB_BIN:-$root/deploy/pacdeb}"
repo_dir="${PACDEB_REPO_DIR:-/var/lib/pacdeb/repo}"
pacman_conf="${PACMAN_CONF:-/etc/pacman.conf}"
apps_dir="${PACDEB_APPS_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/applications}"
desktop_file="$apps_dir/pacdeb.desktop"
# Tests set SUDO to empty and PACMAN_KEY_CMD to a pacman-key with its own keyring.
sudo="${SUDO-sudo}"
read -r -a pacman_key <<< "${PACMAN_KEY_CMD:-pacman-key}"

step() { printf '\n\033[1m==> %s\033[0m\n' "$*"; }
skip() { printf '    already done\n'; }
as_root() { if [[ -n "$sudo" ]]; then "$sudo" "$@"; else "$@"; fi; }

if [[ ! -x "$pacdeb" ]]; then
    echo "$pacdeb is missing. Run build/build.sh first." >&2
    exit 1
fi

fingerprint() {
    "$pacdeb" repo status 2> /dev/null | sed -n 's/^Signing key: //p'
}

has_section() {
    grep -qx '\[pacdeb\][[:space:]]*' "$pacman_conf"
}

write_desktop_file() {
    printf '%s\n' \
        '[Desktop Entry]' \
        'Type=Application' \
        'Name=pacdeb' \
        'GenericName=Debian Package Converter' \
        'Comment=Install .deb apps as pacman packages and keep them updated' \
        "Exec=\"$root/deploy/pacdeb-gui\" %f" \
        'Icon=system-software-install' \
        'Terminal=false' \
        'Categories=System;Settings;PackageManager;' \
        'MimeType=application/vnd.debian.binary-package;application/x-deb;' \
        'Keywords=deb;debian;package;install;update;'
}

setup() {
    step "Repository folder $repo_dir"
    if [[ -d "$repo_dir" && -w "$repo_dir" ]]; then
        skip
    else
        as_root install -d -o "$(id -un)" -m 755 "$repo_dir"
        echo "    created"
    fi

    step "Repository and signing key"
    "$pacdeb" repo init "$repo_dir" | sed -n '/^Published\|^Repository ready/p'
    local fpr
    fpr="$(fingerprint)"
    if [[ -z "$fpr" ]]; then
        echo "pacdeb did not report a signing key; see 'pacdeb repo status'" >&2
        exit 1
    fi

    step "pacman trusts the key $fpr"
    if as_root "${pacman_key[@]}" --list-keys "$fpr" &> /dev/null; then
        skip
    else
        as_root "${pacman_key[@]}" --add "$repo_dir/pacdeb.pub.asc"
        as_root "${pacman_key[@]}" --lsign-key "$fpr"
    fi

    step "[pacdeb] section in $pacman_conf"
    if has_section; then
        skip
    else
        as_root cp "$pacman_conf" "$pacman_conf.pacdeb-backup"
        printf '\n[pacdeb]\nSigLevel = Required\nServer = file://%s\n' "$repo_dir" | as_root tee -a "$pacman_conf" > /dev/null
        echo "    added (the previous file is saved as $pacman_conf.pacdeb-backup)"
    fi

    step "Update timer"
    "$pacdeb" timer enable

    step "Start menu entry"
    if [[ -x "$root/deploy/pacdeb-gui" ]]; then
        mkdir -p "$apps_dir"
        write_desktop_file > "$desktop_file"
        update-desktop-database "$apps_dir" 2> /dev/null || true
        echo "    $desktop_file"
    else
        echo "    skipped: deploy/pacdeb-gui is missing (run build/build.sh)"
    fi

    step "Done"
    echo "pacdeb apps now show up in your system updates once pacman refreshes its"
    echo "package lists (the CachyOS updater and 'sudo pacman -Syu' both do that)."
}

remove() {
    local fpr
    fpr="$(fingerprint)"

    step "Update timer"
    "$pacdeb" timer disable

    step "Start menu entry"
    if [[ -f "$desktop_file" ]]; then
        rm "$desktop_file"
        update-desktop-database "$apps_dir" 2> /dev/null || true
        echo "    removed"
    else
        skip
    fi

    step "[pacdeb] section in $pacman_conf"
    if has_section; then
        as_root cp "$pacman_conf" "$pacman_conf.pacdeb-backup"
        # Drop the [pacdeb] section, from its header up to the next section or the end,
        # and the blank lines right before it that setup added.
        awk '
            /^[[:space:]]*$/ { if (!drop) blanks = blanks $0 "\n"; next }
            /^\[pacdeb\][[:space:]]*$/ { drop = 1; blanks = ""; next }
            /^\[/ { drop = 0 }
            !drop { printf "%s", blanks; blanks = ""; print }
            END { if (!drop) printf "%s", blanks }
        ' "$pacman_conf.pacdeb-backup" \
            | as_root tee "$pacman_conf" > /dev/null
        echo "    removed (the previous file is saved as $pacman_conf.pacdeb-backup)"
    else
        skip
    fi

    step "pacman's trust in the key"
    if [[ -n "$fpr" ]] && as_root "${pacman_key[@]}" --list-keys "$fpr" &> /dev/null; then
        as_root "${pacman_key[@]}" --delete "$fpr"
    else
        skip
    fi

    step "Repository"
    "$pacdeb" repo remove
    if [[ -d "$repo_dir" ]]; then
        as_root rm -r "$repo_dir"
        echo "    deleted $repo_dir"
    fi

    step "Done"
    echo "Apps installed from the repository stay installed; pacdeb still updates them"
    echo "with 'pacdeb update'."
}

case "${1:-}" in
    "") setup ;;
    --remove) remove ;;
    -h | --help)
        echo "Usage: setup.sh [--remove]"
        echo "Sets up pacdeb's local repository, pacman's trust in it, and the update timer."
        ;;
    *)
        echo "Usage: setup.sh [--remove]" >&2
        exit 2
        ;;
esac
