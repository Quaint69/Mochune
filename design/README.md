# Mochune brand assets

The default mascot is an ivory mochi wearing orange headphones. Original masters and pixel theme variants are saved in [mascot](mascot/). The angled concept is retained as a draft; `mochune-front.png` is the front-facing default. Theme icons use flat, fixed 64px pixel palettes.

Generate the app/taskbar icons and theme cards with `python3 scripts/make-brand-assets.py` (Pillow), then regenerate installer artwork with `python3 scripts/make-installer-art.py`. Card GIFs are 160×80, with a short integer-pixel bob. Only the selected card is decoded.

Only Mochune mascot themes are shipped. The original duck themes are retained in an external owner backup; they are not selectable in Settings. Each style includes listening, dancing, hopping, swaying, shuffling, spinning, squishing, peeking, equalizer-grooving and napping scenes. `mochune.gif` combines the ten scenes into a 240-source-frame loop; individual clips are also available. The standard appearance uses the warm Mochune loop only while its card is visible.

Mochune Display is the custom rounded, banner-inspired wordmark font (seven brand letters, not a general UI font); see `make-wordmark.py` and its bundled MIT notice. Original artwork retains the repository license. Saved project covers are in [social](social/); these are assets, not changes to the owner's personal X profile.

App icons are exported directly from the unchanged mascot master at 16, 20, 22, 24, 28, 32, 40, 48, 64, 96, 128, 256 and 512px. The ICO includes ten Windows/DPI sizes through 256px, each as its own PNG frame. Small exports use tighter optical padding; they do not use the simplified prototype mascot.
