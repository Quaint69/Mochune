#!/usr/bin/env python3
"""Build bounded icon sizes and tiny card GIFs from the saved mascot masters.

Requires Pillow. Pixel styles are normalized to a fixed 64px canvas and flat
palette; card animation uses integer offsets, not runtime transforms.
"""
from pathlib import Path
import io
import struct
from PIL import Image, ImageColor
from mochune_animation import generate

ROOT = Path(__file__).resolve().parent.parent
MASTERS = ROOT / 'design/mascot'
THEMES = {
    'pixelduck': ('mochune-pixel.png', ['#17220f', '#f0edda', '#626953', '#87b64b', '#b8cd75', '#ffffff']),
    'pastelduck': ('mochune-pastel-pixel.png', ['#4f3269', '#fff0fa', '#ec77bd', '#ba8ad7', '#e5b9f3', '#ffffff']),
    'crt-full': ('mochune-crt-pixel.png', ['#241505', '#ffe7a0', '#ffb000', '#a06600', '#ffcc33', '#fff2cc']),
}


def fitted(path, side, pixel=False, app_icon=False):
    image = Image.open(path).convert('RGBA')
    mask = image.getchannel('A').point(lambda a: 255 if a >= 128 else 0)
    bounds = mask.getbbox()
    if not bounds:
        raise ValueError(f'Empty logo: {path}')
    image = image.crop(bounds)
    # Small app icons need nearly the complete canvas. Export every size from
    # the original master, never from another reduced PNG.
    edge = side - (0 if side <= 20 else 2 if side <= 64 else round(side * .04)) if app_icon else round(side * .9)
    image.thumbnail((edge, edge),
                    Image.Resampling.NEAREST if pixel else Image.Resampling.LANCZOS)
    canvas = Image.new('RGBA', (side, side))
    canvas.alpha_composite(image, ((side - image.width) // 2, (side - image.height) // 2))
    return canvas


def pixels(image):
    return image.get_flattened_data() if hasattr(image, "get_flattened_data") else image.getdata()


def pixel_logo(path, colors):
    source = fitted(path, 64, True)
    rgb = [ImageColor.getrgb(c) for c in colors]
    output = Image.new('RGBA', source.size)
    output.putdata([
        (*min(rgb, key=lambda c: sum((c[k] - p[k]) ** 2 for k in range(3))), 255)
        if p[3] >= 128 else (0, 0, 0, 0)
        for p in pixels(source)
    ])
    return output


def main():
    master = MASTERS / 'mochune-front.png'
    sizes = (16, 20, 24, 28, 32, 40, 48, 64, 128, 256)
    encoded = []
    for side in (*sizes, 22, 96, 512):
        icon = fitted(master, side, app_icon=True)
        buffer = io.BytesIO()
        icon.save(buffer, format='PNG', optimize=True)
        data = buffer.getvalue()
        (ROOT / f'ui/assets/logo-{side}.png').write_bytes(data)
        if side in sizes:
            encoded.append((side, data))
    # PNG-backed ICO frames retain each independently rendered native size.
    directory = bytearray(struct.pack('<HHH', 0, 1, len(encoded)))
    offset = 6 + 16 * len(encoded)
    for side, data in encoded:
        directory += struct.pack('<BBBBHHII', side % 256, side % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    (ROOT / 'ui/assets/app.ico').write_bytes(directory + b''.join(data for _, data in encoded))
    for theme, (filename, colors) in THEMES.items():
        folder = ROOT / 'themes' / theme
        logo = pixel_logo(MASTERS / filename, colors)
        logo.save(folder / 'mochune-logo.png', optimize=True)
        generate(folder, logo, colors)
    # The normal appearance shares the same lazy card player; no custom skin.
    colors = ['#30201b', '#fff1da', '#ff9f1a', '#e85a13', '#ffbf55', '#ffffff']
    default = ROOT / 'design/mascot/default-animation'
    generate(default, pixel_logo(master, colors), colors)
    (ROOT / 'ui/assets/mochune-card.gif').write_bytes((default / 'mochune.gif').read_bytes())



if __name__ == '__main__':
    main()
