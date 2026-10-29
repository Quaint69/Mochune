"""Render small, palette-bounded Mochune music scenes; development asset tool only."""
import json
import math
from PIL import Image, ImageColor, ImageDraw, ImageFont

SCENES = ('listen', 'dance', 'hop', 'sway', 'shuffle', 'spin', 'squish', 'peek', 'groove', 'nap')
SIZE = (160, 80)
FRAMES = 24
FRAME_MS = 90


def eye_boxes(sprite, ink):
    """Find the two isolated eye clusters, leaving the headphone outline alone."""
    remaining = {(x, y) for y in range(64) for x in range(64)
                 if sprite.getpixel((x, y)) == (*ink, 255)}
    eyes = []
    while remaining:
        point = remaining.pop()
        todo, cluster = [point], [point]
        while todo:
            x, y = todo.pop()
            for neighbor in ((x-1, y), (x+1, y), (x, y-1), (x, y+1)):
                if neighbor in remaining:
                    remaining.remove(neighbor); todo.append(neighbor); cluster.append(neighbor)
        xs, ys = zip(*cluster)
        if 3 <= len(cluster) <= 50 and min(xs) > 15 and max(xs) < 49 and min(ys) > 27:
            eyes.append((min(xs), min(ys), max(xs), max(ys)))
    if len(eyes) != 2:
        raise ValueError(f'Expected two isolated mascot eyes, found {len(eyes)}')
    return sorted(eyes)


def close_eyes(sprite, eyes, ink, face, wink=False):
    sprite = sprite.copy(); draw = ImageDraw.Draw(sprite)
    for left, top, right, bottom in eyes[:1] if wink else eyes:
        draw.rectangle((left, top, right, bottom), fill=face)
        mid = (top + bottom) // 2 + 1
        draw.line((left, mid, right, mid), fill=ink, width=1)
    return sprite


def note(draw, x, y, color):
    x, y = round(x), round(y)
    draw.rectangle((x, y+6, x+3, y+8), fill=color)
    draw.line((x+3, y, x+3, y+7), fill=color, width=1)
    draw.line((x+3, y, x+7, y+2), fill=color, width=1)


def pose(sprite, width, height, angle):
    image = sprite.resize((max(1, round(width)), max(1, round(height))), Image.Resampling.NEAREST)
    stage = Image.new('RGBA', (112, 112))
    stage.alpha_composite(image, ((112-image.width)//2, (112-image.height)//2))
    if angle:
        stage = stage.rotate(angle, resample=Image.Resampling.NEAREST)
    return stage.crop(stage.getbbox())


def indexed(frame, palette):
    colors = [(0, 0, 0)] + palette
    lookup = {c: i for i, c in enumerate(colors)}
    out = Image.new('P', frame.size)
    out.putpalette([v for c in colors for v in c] + [0] * (768 - len(colors)*3))
    data = frame.get_flattened_data() if hasattr(frame, 'get_flattened_data') else frame.getdata()
    out.putdata([lookup[p[:3]] if p[3] >= 128 else 0 for p in data])
    out.info['transparency'] = 0
    return out


def save_gif(path, frames, palette):
    converted = [indexed(frame, palette) for frame in frames]
    converted[0].save(path, save_all=True, append_images=converted[1:], duration=FRAME_MS,
                      loop=0, disposal=2, transparency=0, optimize=True)
    # Decode the complete loop here, not at runtime. Validate every frame.
    with Image.open(path) as image:
        assert image.size == SIZE and 1 < image.n_frames <= 300
        for index in range(image.n_frames):
            image.seek(index); image.convert('RGBA').load()


def generate(folder, sprite, colors, contact=True):
    folder.mkdir(parents=True, exist_ok=True)
    palette = [ImageColor.getrgb(c) for c in colors]
    ink, face, accent, secondary, bright, highlight = [(*c, 255) for c in palette]
    eyes = eye_boxes(sprite, palette[0])
    combined, previews = [], []
    for scene in SCENES:
        frames = []
        for index in range(FRAMES):
            t = index/(FRAMES-1); a = math.tau*t; envelope = math.sin(math.pi*t)**2
            frame = Image.new('RGBA', SIZE); draw = ImageDraw.Draw(frame)
            width = height = 64; angle = 0.; x = 80.; floor = 69.; center = False
            closed = scene == 'nap' or index in (7, 8)
            art = close_eyes(sprite, eyes, ink, face) if closed else sprite
            if scene == 'listen':
                height += 3*math.sin(a*2); angle = 3*math.sin(a)
                if index > 2 and index < 20:
                    note(draw, 115+math.sin(a)*4, 29-12*t, accent)
            elif scene == 'dance':
                x += 17*math.sin(a*2)*envelope; angle = 13*math.sin(a*3)*envelope
                floor -= 5*abs(math.sin(a*2))*envelope
                if 5 < index < 19:
                    note(draw, 36, 18+math.cos(a)*3, secondary)
                    note(draw, 118, 26+math.sin(a)*4, accent)
            elif scene == 'hop':
                jump = math.sin(math.pi*t)
                floor -= 15*jump; width = 64 + 8*math.cos(a)*envelope
                height = 64 - 6*math.cos(a)*envelope
            elif scene == 'sway':
                x += 12*math.sin(a); angle = -10*math.sin(a)
            elif scene == 'shuffle':
                x += 26*math.sin(a); floor -= 3*abs(math.sin(a*3))
                width += 3*math.sin(a*3)
            elif scene == 'spin':
                angle = 360*t; floor = 40; center = True
                if 8 < index < 16:
                    for xx in (34, 123):
                        draw.line((xx-2, 21, xx+2, 21), fill=bright)
                        draw.line((xx, 19, xx, 23), fill=bright)
            elif scene == 'squish':
                amount = math.sin(a*2)*envelope
                width += 13*amount; height -= 14*amount
            elif scene == 'peek':
                floor += 47*(1-envelope)
            elif scene == 'groove':
                x = 80-14*envelope; angle = 6*math.sin(a*2)*envelope
                for bar in range(5):
                    h = round((5+14*abs(math.sin(a*2+bar)))*envelope)
                    if h: draw.rectangle((115+bar*5, 67-h, 117+bar*5, 67), fill=accent if bar%2 else secondary)
            elif scene == 'nap':
                height -= 3*(1-math.cos(a))/2
                # Tiny pixel Zs drift upward as the headphones rest.
                for step in range(2):
                    xx=110+step*9; yy=28-step*10-round(3*math.sin(math.pi*t))
                    draw.line((xx,yy,xx+4,yy,xx,yy+4,xx+4,yy+4),fill=secondary)
            character = pose(art, width, height, angle)
            y = round(floor-character.height/2) if center else round(floor-character.height)
            frame.alpha_composite(character, (round(x-character.width/2), y))
            frames.append(frame)
        save_gif(folder/f'mochune-{scene}.gif', frames, palette)
        combined.extend(frames)
        previews.append(frames[FRAMES//2])
    save_gif(folder/'mochune.gif', combined, palette)
    (folder/'scene-order.json').write_text(json.dumps({'mascot':'Mochune','order':list(SCENES),
        'source_frames_per_scene':FRAMES,'frame_ms':FRAME_MS,'runtime_shuffle':False},indent=2)+'\n')
    if contact:
        sheet=Image.new('RGB',(3*SIZE[0],4*(SIZE[1]+18)), '#131211')
        draw=ImageDraw.Draw(sheet); font=ImageFont.load_default()
        for i,(name,preview) in enumerate(zip(SCENES,previews)):
            xx=(i%3)*SIZE[0]; yy=(i//3)*(SIZE[1]+18)
            sheet.paste(preview,(xx,yy),preview)
            draw.text((xx+8,yy+SIZE[1]),name,font=font,fill='#fff1da')
        sheet.save(folder/'scenes.png',optimize=True)
    print(f'{folder.name}: {len(SCENES)} scenes, {len(combined)} source frames, {(folder/"mochune.gif").stat().st_size:,} bytes')
