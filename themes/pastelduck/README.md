# Mochune Pastel

Self-contained appearance-only v2 Music and Video theme. The palette, style and legacy artwork declarations are preserved on regeneration.

Font: PixelMplus10 (game-font faces and license notices are supplied in this theme's fonts/ folder; no system install). Sources and licenses: `../../ui/fonts/GAME-FONTS.md`.

Covers all 266 public Music and Video component keys and all shared primitive kinds across header, library, list and player. Nested controls inherit kind rules, including popup items, sliders, scrollbars and lyrics. Only supported fields apply; settings/video chrome and layout are not themed.

The app discovers this folder through themes/<folder>/skin.json. Each section switch affects styling, not visibility. Legacy card animation remains experimental; disable animations for low CPU use.

Regenerate with `rust-external python3 scripts/expand-music-themes.py`; verify reproducibility with `--check`.

## Mochune card animations

`mochune.gif` plays ten music-themed scenes: listen, dance, hop, sway, shuffle, spin, squish, peek, groove and nap. Individual clips are saved as `mochune-<scene>.gif`; set `assets.card.file` to one to loop only that behavior. `scene-order.json` describes the fixed sequence and `scenes.png` is the contact sheet. GIF encoders may merge identical frames, preserving the loop duration. Only the configured animation is lazily decoded. There is no original-duck setting or duck artwork in the shipped theme.
