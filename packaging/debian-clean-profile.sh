#!/bin/sh
# Run as the profile owner, never as root on another user's directory.
set -eu
case "${HOME:-}" in /*) ;; *) echo "Mochune: invalid home directory" >&2; exit 1;; esac
[ "$HOME" != / ] || exit 1
for relative in .local/share/DuckTube .config/DuckTube .cache/DuckTube .local/state/DuckTube .local/share/Mochune .config/Mochune .cache/Mochune .local/state/Mochune .local/share/ytlite .config/ytlite .cache/ytlite .local/state/ytlite; do
    # No globbing or download-directory deletion. rm does not follow leaf links.
    rm -rf -- "$HOME/$relative"
done
