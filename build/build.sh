#!/usr/bin/env bash
# The only build entry point. Keeps cargo's registry and output inside build/.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

export CARGO_HOME="$root/build/cargo-home"
export CARGO_TARGET_DIR="$root/build/target"

cargo build --release --features gui "$@"

mkdir -p "$root/deploy"
for bin in pacdeb pacdeb-gui; do
    install -m 755 "$CARGO_TARGET_DIR/release/$bin" "$root/deploy/$bin"
    echo "built deploy/$bin"
done
