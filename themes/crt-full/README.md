# Phosphor CRT · Terminal-inspired · Full music UI

Self-contained appearance-only v2 Music and Video theme. The palette, style and legacy artwork declarations are preserved on regeneration.

Font: DejaVu Sans Mono (game-font faces and license notices are supplied in this theme's fonts/ folder; no system install). Sources and licenses: `../../ui/fonts/GAME-FONTS.md`.

Covers all 267 public Music and Video component keys and all shared primitive kinds across header, library, list and player. Nested controls inherit kind rules, including popup items, sliders, scrollbars and lyrics. Only supported fields apply; settings/video chrome and layout are not themed.

The app discovers this folder through themes/<folder>/skin.json. Each section switch affects styling, not visibility. Legacy card animation remains experimental; disable animations for low CPU use.

Regenerate with `rust-external python3 scripts/expand-music-themes.py`; verify reproducibility with `--check`.
