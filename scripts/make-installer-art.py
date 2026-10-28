#!/usr/bin/env python3
"""Regenerate small installer bitmaps from the existing app icon (Pillow)."""
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont
ROOT = Path(__file__).resolve().parent.parent
FONT = ROOT / "ui/fonts/DejaVuSans.ttf"
BOLD = ROOT / "ui/fonts/DejaVuSans-Bold.ttf"
SCALE = 2
def point(x, y): return (x * SCALE, y * SCALE)
def box(*values): return tuple(v * SCALE for v in values)

def font(size, bold=False):
    return ImageFont.truetype(str(BOLD if bold else FONT), size * SCALE)

def canvas(size):
    return Image.new("RGB", tuple(v * SCALE for v in size), "#131211")

def duck(image, xy, size):
    logo = Image.open(ROOT / "ui/assets/logo-256.png").convert("RGBA")
    logo.thumbnail((size * SCALE, size * SCALE), Image.Resampling.LANCZOS)
    image.paste(logo, tuple(v * SCALE for v in xy), logo)

sidebar = canvas((164, 314))
draw = ImageDraw.Draw(sidebar)
draw.rounded_rectangle(box(15, 37, 149, 171), radius=28 * SCALE, fill="#24211c")
duck(sidebar, (26, 47), 112)
draw.text(point(17, 190), "Mochune", font=font(23, True), fill="#f7f1e7")
sidebar.save(ROOT / "packaging/installer-sidebar.bmp")
header = canvas((150, 57))
duck(header, (99, 5), 46)
header.save(ROOT / "packaging/installer-header.bmp")

# RichEdit receives explicit paper/text colors instead of black text on a dark box.
# Escape the bundled license verbatim; do not modify its wording.
def rtf_escape(text):
    out = []
    for char in text:
        if char in "\\{}": out.append("\\" + char)
        elif char == "\n": out.append("\\par\n")
        elif char == "\r": pass
        elif ord(char) > 127:
            for offset in range(0, len(char.encode("utf-16le")), 2):
                code = int.from_bytes(char.encode("utf-16le")[offset:offset+2], "little")
                out.append("\\u" + str(code if code < 32768 else code - 65536) + "?")
        else: out.append(char)
    return "".join(out)
license_rtf = r"{\rtf1\ansi\deff0{\fonttbl{\f0 Segoe UI;}}{\colortbl;\red44\green33\blue20;}\viewkind4\uc1\pard\cf1\f0\fs20 " + rtf_escape((ROOT / "LICENSE").read_text()) + "}"
(ROOT / "packaging/installer-license.rtf").write_text(license_rtf)
