#!/usr/bin/env bash
# Run on the configured Linux build machine. All build/staging stays on its SSD.
set -euo pipefail
project_dir="$(dirname -- "$(readlink -f -- "$0")")"
cd -- "$project_dir"
exec "$HOME/.local/bin/rust-external" python3 scripts/package.py windows --build
