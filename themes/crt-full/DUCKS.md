# CRT duck animations

The card plays `ducklife.gif`: all ten duck scenes in a fixed, pre-shuffled
33.6-second loop. `scene-order.json` records the order. Individual `duck-*.gif`
clips and the contact sheet `scenes.png` are preserved for authoring. Set
`assets.card.file` to an individual clip to loop only that behavior.

The app loops one GIF per artwork slot; it does not currently shuffle or
sequence separate GIF files at runtime. Only the referenced GIF is loaded.
Enable Show empty card and Animate skin artwork to see it.
Art follows the repository's GPL-3.0-only license.
