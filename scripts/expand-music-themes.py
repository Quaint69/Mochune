#!/usr/bin/env python3
"""Regenerate complete Music and Video themes from their self-contained folders.

Run through rust-external. --check compares generated JSON/assets with disk.
Coverage means public component-map keys plus shared primitive kinds; it does
not imply settings chrome or unsupported paint fields are themeable.
"""
import argparse
import copy
import json
import os
import re
import subprocess
from pathlib import Path

PROJECT = Path(__file__).resolve().parent.parent
THEMES = PROJECT / "themes"
KINDS = (
    "surface", "button", "text", "icon", "input", "loading", "skeleton",
    "lyrics", "slider", "track", "fill", "thumb", "scrollbar",
    "scrollbar-thumb", "button-texture", "panel-artwork", "card-artwork", "frame", "artwork",
)


def pixel_heart(liked):
    """Original 16px sprite; transparent surround, never a rectangular card."""
    from io import BytesIO
    from PIL import Image

    silhouette = (
        "................",
        "................",
        "...###...###....",
        "..#####.#####...",
        ".#############..",
        ".#############..",
        ".#############..",
        "..###########...",
        "...#########....",
        "....#######.....",
        ".....#####......",
        "......###.......",
        ".......#........",
        "................",
        "................",
        "................",
    )
    pixels = {(x, y) for y, row in enumerate(silhouette) for x, c in enumerate(row) if c == "#"}
    image = Image.new("RGBA", (16, 16))
    for x, y in pixels:
        edge = any((x + dx, y + dy) not in pixels for dx, dy in ((-1, 0), (1, 0), (0, -1), (0, 1)))
        if edge:
            image.putpixel((x, y), (72, 30, 34, 255) if liked else (235, 221, 186, 255))
        elif liked:
            image.putpixel((x, y), (255, 162, 150, 255) if y <= 5 and x <= 5
                           else (180, 49, 60, 255) if y >= 9 else (237, 83, 94, 255))
    output = BytesIO()
    image.save(output, format="PNG", optimize=True)
    return output.getvalue()


def generate(name, mapping):
    source = THEMES / name
    theme = json.loads((source / "skin.json").read_text())
    original = copy.deepcopy(theme)
    if name == "pixelduck":
        theme["style"]["font_family"] = "DuckPixel"
        theme["fonts"] = [{"family": "DuckPixel", "file": f"fonts/{face}",
                           "license": "fonts/DuckPixel-LICENSE.txt"}
                          for face in ("DuckPixel.otf", "DuckPixel-Bold.otf")]
    elif name == "pastelduck":
        theme["style"]["font_family"] = "PixelMplus10"
        theme["fonts"] = [{"family": "PixelMplus10", "file": f"fonts/{face}",
                           "license": "fonts/PixelMplus-LICENSE.txt"}
                          for face in ("PixelMplus10-Regular.ttf", "PixelMplus10-Bold.ttf")]
    p, style = theme["palette"], theme["style"]
    radius = style["radius"]
    theme["version"] = 2
    theme["theme_assets"] = {"section-frame": {"file": "frame.png", "fit": "fill"}}
    button_states = {
        "hover": {"background": p["button_hover"], "border": p["accent"]},
        "pressed": {"background": p["accent"], "border": p["text"]},
        "active": {"background": p["button_hover"], "border": p["accent"]},
        "disabled": {"opacity": 0.45},
        "focus": {"border": p["accent"], "border_width": 2},
        "playing": {"background": p["button_hover"], "border": p["played"]},
        "loading": {"background": p["surface"]},
    }
    rules = {
        "surface": {"radius": radius, "border": p["border"]},
        "button": {"background": p["button"], "border": p["border"],
                   "border_width": 0, "radius": radius, "states": button_states},
        "text": {"text": p["text"], "states": {
            "disabled": {"opacity": 0.55}, "playing": {"text": p["accent"]}}},
        "icon": {"tint": p["text"], "pixelated": style["pixelated"],
                 "states": {"active": {"tint": p["liked"]},
                            "playing": {"tint": p["accent"]},
                            "disabled": {"opacity": 0.45}}},
        "input": {"text": p["text"], "states": {"disabled": {"opacity": 0.55}}},
        "loading": {"tint": p["accent"], "opacity": 0.85},
        "skeleton": {"background": p["surface"], "radius": radius},
        "lyrics": {"text": p["text"]},
        "slider": {"radius": radius, "states": {
            "focus": {"border": p["accent"], "border_width": 1},
            "disabled": {"opacity": 0.45}}},
        "track": {"background": p["track"], "radius": radius},
        "fill": {"background": p["played"], "radius": radius},
        "thumb": {"background": p["accent"], "border": p["text"],
                  "border_width": 1, "radius": radius},
        "scrollbar": {"background": p["panel"], "radius": radius},
        "scrollbar-thumb": {"background": p["border"], "radius": radius,
                            "states": {"hover": {"background": p["accent"]},
                                       "pressed": {"background": p["played"]}}},
        # Leave legacy raster artwork visible; these rules do not replace GIFs.
        "button-texture": {"pixelated": style["pixelated"]},
        "panel-artwork": {"pixelated": style["pixelated"]},
        "card-artwork": {"pixelated": style["pixelated"]},
        "frame": {"pixelated": style["pixelated"]},
        "artwork": {"tint":"#00000000","pixelated":False},
    }
    sections = {}
    for section in ("header", "library", "list", "player", "video"):
        components = copy.deepcopy(rules)
        for key, meta in mapping.items():
            if meta["section"] == section:
                # State paint stays in the kind rule so an exact base cannot
                # accidentally mask its hover/playing colors. Keep each public
                # key explicit without multiplying identical state tables.
                kind = meta["kind"]
                components[key] = (
                    {"radius": radius} if kind in ("button", "surface", "slider", "scrollbar")
                    else {"font_family": style["font_family"]} if kind in ("text", "input", "lyrics")
                    else {"pixelated": style["pixelated"]} if kind in ("icon", "frame", "panel-artwork", "card-artwork")
                    else {}
                )
        # Only the section's primary surface gets a frame: nested rows and
        # controls must not inherit a giant raster frame or opaque text fill.
        primary = {"header": "header", "library": "library-surface-1",
                   "list": "list-surface-1", "player": "music-player", "video": "video-page"}[section]
        components[primary].update(background=p["panel"], border=p["border"],
                                   border_width=2, radius=radius)
        if section in ("library", "list"):
            components[primary].update(frame="section-frame", frame_slice=4)
        if section == "list":
            components["save-played-music"] = {
                "background": p["track"], "border": p["border"], "border_width": 1,
                "radius": radius, "states": {"active": {"background": p["played"]},
                                               "hover": {"border": p["accent"]}}}
            components["cache-switch-thumb"] = {
                "background": p["text"], "radius": min(8, radius),
                "states": {"active": {"background": p["accent_text"]}}}
        if section == "header":
            # The 40px header has zero inner padding. An outer border/frame
            # collides with adjacent button borders; never reintroduce either.
            components[primary].update(border="#00000000", border_width=0)
            components["window"].update(background=p["panel"])
            components["search-field"].update(background=p["surface"], border_width=1)
            if name == "pastelduck":
                components["header-surface-1"].update(background=p["button"],border_width=0,border="#00000000")
        # Photos share the icon wrapper, but must not inherit the icon tint:
        # colorizing a thumbnail turns its RGB content into a solid silhouette.
        for photo in {"list": ("list-icon-3",), "player": ("album-image",)}.get(section, ()):
            components[photo].update(tint="#00000000", pixelated=False)
        # Popup surfaces need an opaque base and item-specific state paint;
        # otherwise a light theme leaves dark stock menus behind its text.
        for key, meta in mapping.items():
            if meta["section"] != section or meta["kind"] != "button":
                continue
            popup = {"filter-button": "browsemenu", "sort-button": "browsemenu",
                     "list-button-6": "playlistpicker",
                     "player-button-5": "playlistpicker", "video-filter": "playlistpicker",
                     "video-browsemenu": "browsemenu"}.get(key)
            if popup is None:
                continue
            for suffix in (f"-{popup}-surface-1",):
                components[key + suffix] = {"background": p["panel"], "border": p["border"],
                                            "border_width": 1, "radius": radius}
            for suffix in (f"-{popup}-surface-2",):
                components[key + suffix] = {"states": copy.deepcopy(button_states)}
        if section in ("library", "list"):
            row = "library-surface-3" if section == "library" else "list-surface-2"
            components[row].update(background=p["surface"], border_width=0,
                                   states=copy.deepcopy(button_states))
        if section == "player":
            components.update({
                "lyrics-previous": {"text": p["muted"]},
                "lyrics-current": {"text": p["accent"], "font_weight": 700},
                "lyrics-next": {"text": p["text"]},
                "lyrics-following": {"text": p["muted"]},
            })
        if section == "video":
            # Preserve content photos; tint applies only to symbolic icons.
            for key,meta in mapping.items():
                if meta["section"] != "video": continue
                path=PROJECT / meta["source"]
                text=path.read_text() if path.is_file() else ""
                marker='theme-key: "'+key+'"'
                pos=text.find(marker)
                if pos<0:continue
                # Properties end at the first child node; no inherited child fill.
                body=text[pos+len(marker):]
                stop=re.search(r'\b(?:ThemeBox|ThemeText|ThemeImage|Image|HorizontalBox|VerticalBox|TouchArea)\s*\{',body)
                if stop:body=body[:stop.start()]
                if meta["kind"] in ("text","input") and "AppColors.muted" in body:
                    components[key]["text"]=p["muted"]
                if meta["kind"]=="surface" and "background:" in body:
                    if "has-hover" in body:
                        components[key]["states"]={"hover":{"background":p["button_hover"]}}
                    elif any(x in body for x in ("AppColors.panel","AppColors.raised","AppColors.window","#000000a0","#000000b8","#000b")):
                        components[key]["background"]=p["panel"]
                    elif "AppColors.muted-surface" in body:
                        components[key]["background"]=p["surface"]
            components["video-tile"].update(background="#00000000",border_width=0,states={"hover":{"background":p["surface"]}})
            components["video-player-surface-18"].update(background="#00000000",states={"hover":{"background":p["surface"]}})
            for key in ("video-page","video-details","video-controls","video-channel-card"):
                components[key].update(background=p["panel"],border_width=0)
            components["video-comment-card"].update(background=p["surface"],border_width=0)
            components["video-control-button"].update(states=copy.deepcopy(button_states))
            components["video-captions"].update(background="#00000000",border_width=0,
                states={"active":{"background":p["button_hover"],"border":p["accent"],"border_width":1},
                        "hover":{"background":p["button"]},"disabled":{"opacity":0.45}})
            components["video-quality-surface-2"].update(background=p["panel"],border_width=1)
            components["video-quality-surface-3"].update(states=copy.deepcopy(button_states))
        sections[section] = {"defaults": {"font_family": style["font_family"]},
                             "components": components}
    if name == "groovyduck":
        # Decorative paper stays in the player; lists remain solid and legible.
        for section in sections.values():
            components = section["components"]
            components["button"]["border_width"] = 0
            components["button"]["states"]["pressed"].update(background=p["track"])
            components["button"]["states"]["focus"].update(border=p["accent"], border_width=3)
            components["text"]["states"]["playing"]["text"] = p["text"]
            components["icon"]["states"]["playing"]["tint"] = p["text"]
            components["scrollbar-thumb"]["background"] = p["muted"]
        for section in ("library", "list"):
            primary = "library-surface-1" if section == "library" else "list-surface-1"
            components = sections[section]["components"]
            components[primary].pop("frame", None)
            components[primary].pop("frame_slice", None)
            components[primary].update(border_width=0)
        player = sections["player"]["components"]
        sections["header"]["components"]["header-text-1"].update(
            font_family="Shrikhand", font_size=19, font_weight=400)
        player["player-text-1"].update(font_family="Shrikhand", font_size=14, font_weight=400)
        player["music-player"].update(border_width=0)
        player["player-surface-2"].update(background=p["panel"], border_width=0, radius=10)
        player["player-surface-4"].update(background=p["surface"], border_width=0, radius=8)
        player["lyrics-current"].update(text=p["text"])
        player["play-pause-button"].update(background=p["button_hover"], border_width=0, radius=16)
        # Keep exact-key state paint explicit so it does not mask kind states.
        player["play-pause-button"]["states"] = copy.deepcopy(button_states)
        player["play-pause-button"]["states"]["pressed"]["background"] = p["track"]
    if name == "pixelduck":
        theme["theme_assets"].update({
            "pixel-heart": {"file": "heart-pixel.png", "fit": "contain"},
            "pixel-heart-liked": {"file": "heart-pixel-liked.png", "fit": "contain"},
        })
        for section, key in (("player", "heart-button"), ("list", "heart-button-9")):
            components = sections[section]["components"]
            # Keep the original hit box and callbacks; only remove its card.
            components[key].update(replace_chrome=True, border_width=0, background="#00000000")
            components[key + "-icon"] = {
                "image": "pixel-heart", "pixelated": True,
                "states": {"active": {"image": "pixel-heart-liked"}},
            }
    # Keep the branded header when regenerating component rules.
    theme["theme_assets"]["brand-logo"] = {"file": "mochune-logo.png", "fit": "contain"}
    for number, size, scale in ((1, 22, 28 / 24), (2, 30, 32 / 28)):
        sections["header"]["components"][f"header-icon-{number}"].update(
            image="brand-logo", scale=scale)
        sections["header"]["components"][f"header-text-{number}"].update(
            font_family="Mochune Display", font_size=size, font_weight=700)
    theme["sections"] = sections
    assert theme["palette"] == original["palette"]
    expected_style = dict(original["style"], font_family=style["font_family"])
    assert theme["style"] == expected_style
    assert theme["assets"] == original["assets"]
    for key, meta in mapping.items():
        assert key in sections[meta["section"]]["components"], key
    encoded = (json.dumps(theme, ensure_ascii=False, indent=2) + "\n").encode()
    assert len(encoded) <= 65536, (name, len(encoded))
    files = {"skin.json": encoded}
    if name == "pixelduck":
        files["heart-pixel.png"] = pixel_heart(False)
        files["heart-pixel-liked.png"] = pixel_heart(True)
    for font in theme.get("fonts", []):
        for key in ("file", "license"):
            path = font[key]
            font_root = source if name == "groovyduck" else PROJECT / "ui"
            files[path] = (font_root / "fonts" / Path(path).name).read_bytes()
    if theme.get("fonts"):
        files["fonts/SOURCES.md"] = (source / "fonts/SOURCES.md").read_bytes()
    for filename in {a["file"] for a in original["assets"].values()}:
        files[filename] = (source / filename).read_bytes()
    files["README.md"] = (
        f"# {theme['name']}\n\n"
        "Self-contained appearance-only v2 Music and Video theme. "
        "The palette, style and legacy artwork declarations are preserved on regeneration.\n\n"
        f"Font: {style['font_family']} (game-font faces and license notices are supplied in this theme's fonts/ folder; no system install). "
        + ({
            "pixelduck": "DuckPixel is a locally renamed pixel font by Idrees Hassan; its license and attribution are included. "}.get(name, ""))
        + "Sources and licenses: `../../ui/fonts/GAME-FONTS.md`.\n\n"
        f"Covers all {len(mapping)} public Music and Video component keys and all shared "
        "primitive kinds across header, library, list and player. Nested controls "
        "inherit kind rules, including popup items, sliders, scrollbars and lyrics. "
        "Only supported fields apply; settings/video chrome and layout are not themed.\n\n"
        "The app discovers this folder through themes/<folder>/skin.json. "
        "Each section switch affects styling, not visibility. "
        "Legacy card animation remains experimental; disable animations for low CPU use.\n\n"
        "Regenerate with `rust-external python3 scripts/expand-music-themes.py`; "
        "verify reproducibility with `--check`.\n"
    ).encode()
    if name == "groovyduck":
        files["README.md"] = (source / "README.md").read_bytes()
    if name == "pixelduck":
        files["README.md"] += (
            "\n## Optional custom heart test\n\n"
            "The player and song-list Like controls use original transparent 16x16 "
            "pixel hearts: hollow when unliked, red when liked. Their surrounding "
            "button card/texture is removed using `replace_chrome`; the click area "
            "and accessible label remain unchanged. Other icons keep their defaults. "
            "The PNGs are original project artwork generated by `pixel_heart` in "
            "`../../scripts/expand-music-themes.py` and released under CC0-1.0. "
            "Remove the two heart `-icon` rules to restore the default glyphs; "
            "remove the parent `replace_chrome` overrides to restore button chrome.\n"
        ).encode()
    # This authored section documents the generated animation clips.
    readme = (source / "README.md").read_text()
    marker = "\n## Mochune card animations\n"
    if marker in readme and marker.encode() not in files["README.md"]:
        files["README.md"] += (marker + readme.split(marker, 1)[1]).encode()
    return files


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    subprocess.run([str(Path.home() / ".local/bin/rust-external"), "--paths"],
                   check=True, stdout=subprocess.DEVNULL)
    assert os.environ.get("RUST_EXTERNAL_ROOT"), "Run through rust-external"
    mapping = json.loads((THEMES / "component-map.json").read_text())
    for name in ("pastelduck", "pixelduck", "crt-full"):
        destination = THEMES / name
        files = generate(name, mapping)
        for filename, content in files.items():
            path = destination / filename
            if args.check:
                assert path.read_bytes() == content, f"Stale generated file: {path}"
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content)
        print(f"{'Checked' if args.check else 'Generated'} {destination}: "
              f"{len(mapping)} keys, {len(KINDS)} kinds, {len(files['skin.json'])} JSON bytes")


if __name__ == "__main__":
    main()