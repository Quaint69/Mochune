#!/usr/bin/env bash
set -euo pipefail
project_dir="$(dirname -- "$(readlink -f -- "$0")")"
cd -- "$project_dir"
exec "$HOME/.local/bin/rust-external" python3 scripts/package.py windows-installer --build
