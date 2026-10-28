> Current discovery policy: only `themes/<folder>/skin.json` inside the active
> themes root is accepted. Use **Rescan themes** to rebuild the picker and
> **Reload skin** after edits. Earlier arbitrary-file and embedded-preset
> instructions below are historical, not the current selection workflow.

# Mochune Music and Video theme authoring (humans and LLMs)

## Start with a real file

- `reference/skin.json`: loadable whole-mode example; copy the complete folder.
- `reference/format-reference.json`: same folder, every public component key plus
  an inert `format-demo` rule showing every paint field and state. That demo key
  is intentionally not a widget; copy its fields into a real component rule.
- `player-only.json`: minimal version-two player-only theme, no artwork needed.
- `pastelduck/skin.json`, `crt-full/skin.json`, and `pixelduck/skin.json`: complete, self-contained Music and Video examples.
- `skin.schema.json`: version-one/two editor schema; `skin-v1.schema.json` preserves
  the original schema. Schema metadata belongs in the editor configuration,
  **not** as an extra `$schema` field in the theme (unknown fields are rejected).
- `component-map.json`: exact component keys and owning sections, extracted from
  the actual Slint call sites. Shared primitive suffixes are described below.

Use standard JSON, not JSONC or JavaScript. Do not add comments or executable
expressions. Do not modify Rust/Slint to make a skin work. Report missing hooks.

## Compatibility and sections

Version 1 keeps the original `palette`, `style`, and `assets` contract and styles
only the music player. Existing files need no changes. Never automatically spread
a v1 palette across the library or header.

Version 2 adds optional `sections` (`header`, `library`, `list`, `player`, `video`) and
`theme_assets`. It retains the required twelve-color legacy `palette` and optional
legacy `style`/`assets`, so a v1 design can be migrated by changing `version` to 2
and adding section rules without throwing its existing artwork away.

Settings has five saved **styling** switches; they do not hide sections. Missing
sections use application defaults and are marked “not provided.” The player
switch controls legacy player styling too. The optional `video` section styles Home, New, History, channel/search pages, the recommendation sidebar, description/comments, captions and player controls. The shared `header` applies in Video only when `video` is supplied and enabled. A theme without `video` keeps the default Video appearance. Canceling
settings restores the saved choices and skin. Application layout, hit areas,
keyboard behavior, accessibility, real track data and playback callbacks remain
application-owned.

Each section has optional `defaults` and `components`. Resolution order:

1. Section defaults.
2. Component-kind base, its `normal` state, then its requested state.
3. Exact component-key base, its `normal` state, then its requested state.
4. Missing values fall back to the original application value.

Kinds include `surface`, `button`, `text`, `icon`, `input`, `loading`, `skeleton`,
`lyrics`, `slider`, `track`, `fill`, `thumb`, `scrollbar`, `scrollbar-thumb`,
`button-texture`, `panel-artwork`, `card-artwork`, and `frame`.

Avoid putting backgrounds/frames in section defaults: they would be inherited
by text and small nested elements too. Put backgrounds on `surface`, `button`,
or exact keys. Shared widget child keys append a suffix to the owner's key:
music buttons use `-icon`, `-texture`, `-music-button-text-1`; pickers use
`-playlistpicker-text-1`, `-playlistpicker-text-2`,
`-playlistpicker-icon-1`, `-playlistpicker-icon-2` and menu child suffixes.
Scrollbars use `-vertical`/`-horizontal`, then `-thumb`.

States: `normal`, `hover`, `pressed`, `active`, `disabled`, `focus`, `playing`,
`loading`. Only applicable states are emitted by a component; this is not a
scripting language. Play and pause icons have separate keys, as do liked and
unliked appearances through the heart button's `active` state. Popup styles
inherit the owning widget's section. Focus indicators and essential controls
must remain distinguishable in a usable theme.

## Paint fields

All fields are optional. Colors are `#RRGGBB` or `#RRGGBBAA` (alpha last).

| Field | Meaning / bounds |
|---|---|
| `background`, `gradient_end` | Flat fill, or vertical two-color gradient |
| `border`, `border_width` | Color and width, 0–12 logical pixels |
| `radius` | 0–64 logical pixels; clipping remains application-owned |
| `text`, `tint` | Text color / fallback-icon tint; custom artwork keeps its colors |
| `font_family` | Built-in DejaVu Sans, DejaVu Sans Mono, Mochune Display, or a family declared in `fonts` |
| `font_size`, `font_weight` | 8–32 px, 100–900; text can still truncate in fixed geometry |
| `opacity` | 0–1 where supported by the component |
| `image` | Named artwork reference; replaces icon imagery or surface background |
| `frame` | Named transparent foreground artwork, above the component's children |
| `frame_slice` | 0–64 source pixels, clamped to half the frame; positive enables nine-slice |
| `replace_chrome` | Removes the original surface fill/border, not its semantic content |
| `scale` | 0.5–1.5 visual scale on images / slider visuals, not hit areas |
| `offset_x`, `offset_y` | -16–16 px artwork offsets; not control relocation |
| `inset` | 0–24 px artwork safe inset; album uses `album-image` |
| `pixelated` | Nearest-neighbor artwork rendering |

Full-theme guardrails: keep the outer `header` border transparent with
`border_width: 0` and no `frame`. Its 40px layout has zero inner padding, so
an enclosing border/frame collides with the neighboring button borders.
Keep button borders; do not restore an enclosing header outline without a
separately approved layout change. The generator and coverage test enforce this.

Photos currently use the shared icon wrapper. Override `tint` to `#00000000`
and `pixelated` to `false` on `list-icon-3` (track thumbnail) and `album-image`
(player cover); otherwise the shared icon tint recolors photos into solid
silhouettes. Do not apply symbolic-icon tint rules to photographic content.

Supply custom TTF/OTF faces and their licenses under the theme folder using `fonts` declarations. Reload applies validated fonts without rebuilding the app. Registration is bounded for the session (16 unique faces and the existing session byte budget); restart only when that budget is exhausted. Windows native video controls share the same validated font bytes.

Not every field applies to every kind: typography belongs to text/input,
frames to surfaces, scale to artwork/slider visuals. Unsupported combinations
do not create a new widget or alter layout. Use transparent PNG frames instead
of expecting arbitrary clipping masks. Text and icon overrides never alter
click callbacks. Keep decorated content inside its section.

## Assets and resource guarantees

`theme_assets` maps a short identifier to `{"file":"frame.png","fit":"fill"}`.
`image` and `frame` refer to that identifier, not a path. Up to 40 named assets;
PNG/JPEG/WebP up to 512×512, or GIF up to 256×256 / 300 frames. The current surface decoration uses
fill and nine-slice framing; icon replacements retain the slot's contain/cover
behavior. The legacy `assets` slots retain their existing fit semantics.

Paths are relative to the skin folder; parent paths, remote URLs, drive paths
and symlinks escaping the folder are rejected. Same filenames share one decoded
asset across legacy and new slots. Use the same spelling for shared paths.
No per-row raster cache or whole-animation frame cache is created.

All sections **share** the existing 8 MiB decoded working reservation and 8 MiB
encoded limit, not separate allowances. JSON ≤64 KiB; each asset ≤2 MiB. Static
assets reserve two RGBA copies. GIFs (including named section assets) retain the 256×256 / 300-frame limit, eight-copy reservation, lazy worker and bounded update/pixel-work caps. Native Windows video controls reuse decoded image handles, not another asset cache. Live blur/shaders, scripts, control rearrangement and remote assets are not implemented.

Disabled section styles stop drawing; currently decoded assets remain shared
until switching/reloading the theme. This avoids duplicate decode work but is
not per-section lazy asset unloading. Theme reload can temporarily retain old
and new working sets. The artwork reservation is not total process RAM.

## Borderless controls and artist-drawn icons

Borders are optional, independently of icon artwork. Use `border_width: 0`
on a button (or the shared `button` kind) to remove its painted outline.
An exact-key rule overrides the kind's hover/active border width too. Use
`replace_chrome: true` to remove the stock fill, outline, and legacy button
texture, while preserving the icon, hit area, label, and click behavior.
Explicit `frame` artwork is still opt-in foreground decoration; omit it when
you want no frame. A border drawn into your own PNG must be removed in the PNG.

Declare your PNG/JPEG/WebP in `theme_assets`, then reference its identifier in
the icon child's `image`, not the parent button's `image` (that is a background).
For example, inside the player's `components`, with `my-play`, `my-pause`,
`my-heart`, and `my-liked-heart` declared in `theme_assets`:

```json
{
  "button": {"border_width": 0},
  "play-pause-button": {"replace_chrome": true},
  "play-icon": {"image": "my-play"},
  "pause-icon": {"image": "my-pause"},
  "heart-button": {"replace_chrome": true},
  "heart-button-icon": {
    "image": "my-heart",
    "states": {"active": {"image": "my-liked-heart"}}
  }
}
```

Other music buttons follow the same `-icon` convention: `previous-button-icon`,
`next-button-icon`, `shuffle-button-icon`, `repeat-button-icon`,
`volume-button-icon`, and `captions-button-icon`. List hearts belong to the
`list` section and use `heart-button-9-icon`. Custom images keep their original
colors. Keep hover, active/liked, disabled, and keyboard-focus states readable;
borderless does not have to mean no interaction feedback.

## Validate and iterate

From the repository, using the guarded SSD tools:

```sh
/home/monoma/.local/bin/cargo run --locked --offline -- --validate-theme /absolute/path/skin.json
```

The report uses the real loader: asset bytes, dimensions, GIF frame counts,
sharing and per-asset reserved bytes. Safety failures are hard errors, not
warnings. Animated files outside supported GIF slots are not a performance fix.

Place the complete theme in the active themes folder, rescan, and select it in Settings → Themes. Use Reload
after editing; bad reloads keep the previous skin. The settings thumbnail is a
small player preview. For an isolated whole-mode preview with fake local data
and no playback/account/network, run:

```sh
/home/monoma/.local/bin/cargo run --locked --offline -- --preview-theme /absolute/path/skin.json
```

Press R to reload, Space to change play/pause, and the heart to change liked
state. Inspect all five sections, compact mode, long text, selected rows and
open menus. The preview uses the real components and does not save settings.
Animation redraw CPU is still experimental; do not claim it is optimized.

For a bounded large-list preview, append `--preview-rows 10000` (clamped to
3–100,000 rows). Append `--preview-static` to freeze legacy card animations
before measuring. These options only affect the isolated authoring preview;
they do not modify the user's library or saved preferences. Compare the same
fixture and playback state when measuring theme memory or animation cost;
process memory includes more than the artwork reservation.

For automated offscreen rendering, build with `native-overlay-test` and run
`--probe-music-skins`. It checks presets, the reference theme, and switch-off
restoration without creating distribution packages.

## Theme-local fonts

Distribute the entire theme folder, not just `skin.json`. No system installation
or app allowlist edit is needed. Add a top-level declaration (v1 and v2):

```json
"fonts": [
  {"family": "My Custom Font", "file": "fonts/custom-regular.ttf", "license": "fonts/LICENSE.txt"},
  {"family": "My Custom Font", "file": "fonts/custom-bold.ttf", "license": "fonts/LICENSE.txt"}
]
```

Use the font's exact internal family in `style.font_family`, section defaults,
component rules, or state rules. Weight selects regular/bold as usual. Built-in
family names cannot be overridden. Missing glyphs use renderer fallback.

- At most four single-face `.ttf`/`.otf` files, 2 MiB each, 4 MiB per theme.
- Each face needs a nonempty UTF-8 license/permission notice (at most 64 KiB).
  Include actual redistribution rights and source attribution when sharing;
  the loader validates presence, not legal permission.
- Font and license paths must stay inside the selected JSON's folder. Absolute,
  parent-traversal, Windows drive/backslash, and escaping symlink paths reject
  the theme. No font URLs or automatic downloads.
- Missing, oversized, invalid or mismatched-family fonts produce status warnings;
  unavailable custom families use bundled DejaVu Sans. Unknown undeclared
  family names reject the configuration to catch typos.
- Slint has no unregister API. Unique registered bytes are retained for the
  session, deduplicated on reload, capped at 8 MiB / 16 faces (failed registration
  attempts count too). Renderer glyph caches/allocations are additional.
  Restart to reclaim registrations or load fonts beyond the session cap.
- Local fonts are registered only when applying a theme, not during offline
  validation. The validator reports `font_faces_ready` and `font_warnings`.

Treat local font files as trusted input to the platform renderer; confinement,
size limits and parsing do not sandbox a font engine. Keep theme files unchanged
while loading. Old copies referring to DuckPixel/PixelMplus10 without a `fonts`
declaration should be replaced by regenerated full folders or updated manually.


## Video styling checklist for theme-making agents

Add a `video` section to version 2; do not rename the existing music sections.
The schema and `component-map.json` are authoritative. `video-page` is the page
background; `video-tile` is a Home/channel/history card, with a `hover` state.
`video-details` owns description/comments. `video-controls` is the control strip
on both Linux and Windows; `video-control-button` owns shared button state paint.
`video-seekbar` and `video-volume` use the existing slider child suffixes
`-track`, `-fill`, `-thumb`. `video-quality-*` styles the quality/CC popup and its
text. Windows-specific `video-native-*` keys cover captions and status visuals;
Linux/main-window counterparts use `video-player-*`. Style both when providing
an exact-key override; shared kind rules cover both automatically.

Keep `artwork` separate from `icon`: photographic thumbnails should keep their
original pixels and smooth sampling. Never recolor decoded video frames. A theme
cannot change codec choice, hardware decoding, video dimensions, cache limits,
scrolling, queue behavior or keyboard shortcuts. Decoration is confined to the
existing widget boxes; frame/image backgrounds and small GIFs are supported,
but no layout edits or web renderer are introduced.

Use an opaque, readable popup/control surface when changing text to a dark color.
Preserve contrast on duration badges and captions over bright/dark video. Do not
turn every `surface` into a framed card; target named surfaces. The bundled theme
generator follows these rules and keeps the theme JSON below 64 KiB.

Validate with `mochune --validate-theme themes/<folder>/skin.json`. Check the application in
Music, both compact modes, Video player with controls inside/outside/fullscreen,
Home/New/History, channel search, description/comments, and unavailable captions.
Test Windows's native overlay too; it shares the resolver and asset budget but
has its own clipping/input surface. Switching back to Default must restore all
stock styling. Disabled sections must keep functional controls and readable text.

Home grids and video sidebar rows are reusable bounded pools. Themes must not
attach independent timers, allocate a raster per feed item, or require loading
all history thumbnails. Animations stop when their mode/window is inactive.


### Protected live-video surfaces

`video-player-surface-2` supplies the visual corner radius and optional border (corner masks on
Linux and native window shape on Windows). The video canvas and its clipping
ancestors are deliberately plain rectangles. `radius`, `border_width` (0–12px) and `border` are painted by a foreground contour (Windows uses native window regions to expose it). The shipped borders stay off. Background images, opacity, frames and rounded clipping on those containers are ignored: rounded clipping
creates a cached intermediate GPU layer and can stall live video in Pastel.

When controls are inside the picture or fullscreen, `video-controls` always uses
the application's translucent gradient, with no themed background/frame/border.
Themes still style the individual buttons, icons, menus and sliders at normal
opacity. Outside/below the picture, the strip keeps its full themed background.
Do not use a container opacity to fade all controls together.

The clock, stock volume glyph and closed CC/quality labels use opaque white over the protected dark gradient for contrast; their themed text colors still apply when controls are outside the picture. Popup contents keep their themed foreground and background.


`video-captions` is the CC button: use `states.active` for enabled captions,
`normal` for Off, and `disabled` when unavailable. The popup keeps the shared
`video-quality-*` keys. The active button's themed label color is retained when
its themed background is supplied; other in-picture labels stay high-contrast.

The outer application window is always opaque and square in normal, maximized, compact and mini modes. The `window` paint controls its background; its `radius` is ignored. Internal cards, thumbnails and the video picture retain their own themeable corner radii.


## Mascot and alternative card

The bundled themes use a fixed-palette `mochune-logo.png` through the header's `header-icon-1` and `header-icon-2` image rules. Customize that named asset to redraw the brand in your theme's style. Keep the front-facing squashed mochi, two eyes and symmetrical headphones recognizable.

`assets.card` is the default animation. An optional top-level `card_alternative` uses the same `{ "file": "...", "fit": "contain" }` format. It appears as **Original duck card** in Settings; the three bundled themes use it to preserve their duck animation. Both paths obey the same confinement and resource limits. Only the selected card loads; switching replaces its worker animation epoch. Existing themes without an alternative remain supported.
