#!/usr/bin/env bash
set -euo pipefail
export MOCHUNE_DEV_LAUNCH=1
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
if [[ -f "$project_dir/scripts/register-dev-desktop.sh" ]]; then
    bash "$project_dir/scripts/register-dev-desktop.sh" || true
fi
exec /home/monoma/.local/bin/cargo run -j 2 --manifest-path /media/monoma/debian-fresh/home/monoma/Development/rust/projects/mochune-native/Cargo.toml -- "$@"
