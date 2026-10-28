#!/usr/bin/env bash
set -euo pipefail
project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
app_data="${XDG_DATA_HOME:-$HOME/.local/share}"
entry="$app_data/applications/mochune-dev.desktop"
# Remove only the development entry created by our earlier registration.
legacy="$app_data/applications/mochune.desktop"
if [[ -f "$legacy" ]] && grep -q '^X-Mochune-Development=true$' "$legacy"; then rm -- "$legacy"; fi
# Retire only earlier development launchers owned by this project.
for old_entry in "$app_data/applications/ducktube-dev.desktop" "$app_data/applications/ducktube.desktop"; do
    if [[ -f "$old_entry" ]] && grep -q '^X-DuckTube-Development=true$' "$old_entry"; then rm -- "$old_entry"; fi
done
# Separate desktop identity avoids shadowing a packaged installation.
mkdir -p -- "$app_data/applications"
# Avoid rewriting the entry on every launch, and never overwrite a user launcher.
if [[ -f "$entry" ]] && ! grep -q '^X-Mochune-Development=true$' "$entry"; then exit 0; fi
content="[Desktop Entry]
Type=Application
Name=Mochune (Development)
Exec=\"$project_dir/run.sh\"
Icon=$project_dir/ui/assets/logo-128.png
StartupWMClass=mochune-dev
Terminal=false
Categories=AudioVideo;Player;
X-Mochune-Development=true"
if [[ ! -f "$entry" ]] || [[ "$(cat -- "$entry")" != "$content" ]]; then
    printf '%s\n' "$content" > "$entry"
    if command -v update-desktop-database >/dev/null; then update-desktop-database "$app_data/applications" >/dev/null 2>&1 || true; fi
fi
