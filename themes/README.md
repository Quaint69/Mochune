# Music and Video themes

The app discovers valid themes at `themes/<folder>/skin.json`. The picker is
rebuilt from the folder contents at startup and with **Rescan themes**; there is
no arbitrary JSON file picker and no embedded Pastel/DuckPixel preset list.
**Reload skin** reloads the selected theme after edits.

## Included themes

- `pastelduck`: pink desktop appearance, with licensed PixelMplus10 regular/bold.
- `pixelduck`: crisp pixel appearance, with the locally renamed DuckPixel font.
- `crt-full`: amber Phosphor CRT appearance and original animated duck scenes.
- `reference`: small authoring example; not one of the three complete themes.

All three complete themes ship with portable, per-user installer and Debian
packages, including their artwork, fonts and original license notices.
Default stays available without loading any theme assets.

## Folder location and validation

Windows portable apps use `Mochune/themes`, beside the replaceable
`Mochune/runtime` folder, identified by `runtime/PORTABLE.txt`. This matches
the launcher's extraction layout and preserves editable themes across updates.
Windows installed apps use `%LOCALAPPDATA%/Mochune/themes`. Debian packages
use `/usr/share/mochune/themes`. Other executables use adjacent `themes`. Local debug builds
use this repository's `themes` folder when no executable-adjacent folder exists
(the development executable lives on the external build SSD). Release builds
never silently fall back to the developer's source tree. No arbitrary working
folder or user-specified root is searched.

Only direct, non-symlink theme folders are scanned, with a maximum of 256 root
entries. Invalid JSON, missing/escaping resources, unsupported properties,
over-budget assets, and invalid declared fonts exclude a theme from the list.
Scan validation runs on the existing worker, one theme at a time; decoded
artwork is dropped between candidates. No font registration occurs during scan.
Changes are picked up on rescan, not by continuous background watching.

Theme selection is persisted by path; adding/removing/reordering themes does not
change the saved selection. Missing or invalid saved selections fall back to
Mochune default. JSON entry points outside the active themes root (including
command-line preview, validation, or environment overrides) are rejected.
`MOCHUNE_MUSIC_SKIN=pixelduck` selects a folder; absolute paths are accepted
only if they resolve to the same permitted `themes/<folder>/skin.json` shape.

Copy custom themes **into this themes folder**, then rescan. Do not install
fonts system-wide; use theme-local declarations from `AUTHORING.md`. Original
font source inputs and pinned licenses are documented in `../ui/fonts/GAME-FONTS.md`.

Regenerate full component rules with
`rust-external python3 scripts/expand-music-themes.py`; use `--check` for static
reproducibility checks. Generation now reads self-contained full theme folders,
not the deleted original presets. See `AUTHORING.md`, `TESTING.md`,
`skin.schema.json`, and `component-map.json` for supported paint fields.

## GIFs

Each artwork slot currently loops one GIF. CRT's `ducklife.gif` combines all ten
scenes into one fixed pre-shuffled sequence, so it plays every scene. Separate
GIF runtime shuffle/sequencing is not implemented. Only the selected GIF is
loaded; individual clips are retained for authoring. Animation visibility and
the Animate skin artwork switch still gate decoding/redraw work.

Version-two themes may include a `video` section for browsing and video-player chrome. The three bundled themes cover both modes and share one artwork budget. See [AUTHORING.md](AUTHORING.md) and [component-map.json](component-map.json). Existing music-only and version-one files remain compatible.
