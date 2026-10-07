#!/usr/bin/env bash
# Runs the deployed binary. Never builds.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin="$root/deploy/ferry"

if [[ ! -x "$bin" ]]; then
    echo "deploy/ferry is missing. Run build/build.sh first." >&2
    exit 1
fi

exec "$bin" "$@"
