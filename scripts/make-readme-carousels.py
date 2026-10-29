#!/usr/bin/env python3
"""Compose bounded README slideshows from real captures; never alter UI content."""
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / 'docs/assets'
SIZE = (960, 568)
VIEW = (960, 522)
BG = '#0d1117'
FONT_PATH = Path('/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf')
FONT = ImageFont.truetype(str(FONT_PATH), 15) if FONT_PATH.exists() else ImageFont.load_default()


def slide(source, label, index, total):
    canvas = Image.new('RGB', SIZE, BG)
    photo = source.convert('RGB')
    photo.thumbnail(VIEW, Image.Resampling.LANCZOS)
    canvas.paste(photo, ((VIEW[0] - photo.width) // 2, (VIEW[1] - photo.height) // 2))
    draw = ImageDraw.Draw(canvas)
    draw.line((0, 532, SIZE[0], 532), fill='#30363d')
    draw.text((18, 544), label, fill='#e6edf3', font=FONT)
    for dot in range(total):
        x = SIZE[0] - (total * 17 + 15) + dot * 17
        draw.ellipse((x, 549, x + 6, 555), fill='#f4a62a' if dot == index else '#484f58')
    return canvas


def save(name, frames, durations):
    # Separate static scenes need their own palettes to preserve photo colors.
    images = [frame.quantize(colors=256, dither=Image.Dither.FLOYDSTEINBERG)
              for frame in frames]
    images[0].save(ASSETS / name, save_all=True, append_images=images[1:],
                   duration=durations, loop=0, optimize=True, disposal=1)
    output = ASSETS / name
    if output.stat().st_size > 3 * 1024 * 1024:
        raise RuntimeError(f'{name} exceeds the 3 MiB presentation budget')
    print(f'{name}: {output.stat().st_size:,} bytes')


def main():
    themes = []
    for i, (file, label) in enumerate([
        ('music-crt.png', 'Mochune CRT'),
        ('music-pastelduck.png', 'Mochune Pastel'),
        ('music-pixelduck.png', 'Mochune Pixel'),
    ]):
        with Image.open(ASSETS / file) as image:
            themes.append(slide(image, label, i, 3))
    save('themes-carousel.gif', themes, [4000] * len(themes))


if __name__ == '__main__':
    main()
