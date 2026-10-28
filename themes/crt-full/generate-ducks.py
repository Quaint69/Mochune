"""More life for CRT's original amber pixel duck. No application changes."""
import json
import math
import random
from pathlib import Path
from PIL import Image, ImageDraw, ImageOps

ROOT=Path(__file__).resolve().parent
W,H=160,72
BG=(0,0,0,255)
GOLD='#FFC73F'
EDGE='#CF8500'
WING='#EDA000'
EYE='#392200'
DIM='#503300'
FAINT='#251800'
N=28


def duck(x,y,step=0,face=1,blink=False,flap=0,peck=0,angle=0,sleep=False):
    """Same block silhouette/colors as CRT; independently articulated pieces."""
    sprite=Image.new('RGBA',(48,48))
    d=ImageDraw.Draw(sprite)
    # Feet, then body: original duck's short little backward kick.
    for fx,phase in [(12,0),(26,math.pi)]:
        kick=round(3*math.sin(step+phase))
        d.line([(fx,31),(fx+kick,35),(fx+kick+3,35)],fill=EDGE,width=2)
    d.rectangle((5,20,27,31),fill=EDGE)
    d.rectangle((6,21,26,30),fill=GOLD)
    if flap:
        d.polygon([(11,24),(15,24-abs(flap)*9),(24,22),(19,26)],fill=WING)
    else: d.rectangle((10,23,19,27),fill=WING)
    # Head stays recognizable; pecking folds the neck downward.
    hx,hy=19+round(peck*4),12+round(peck*12)
    d.rectangle((hx,hy,hx+13,hy+12),fill=EDGE)
    d.rectangle((hx+1,hy+1,hx+12,hy+11),fill=GOLD)
    d.rectangle((hx+12,hy+6,hx+19,hy+10),fill=EDGE)
    if blink or sleep: d.line((hx+8,hy+5,hx+10,hy+5),fill=EYE,width=1)
    else: d.rectangle((hx+8,hy+3,hx+10,hy+5),fill=EYE)
    if face<0: sprite=ImageOps.mirror(sprite)
    if angle: sprite=sprite.rotate(angle,resample=Image.Resampling.NEAREST,expand=False,center=(24,25))
    return sprite,(round(x)-24,round(y)-31)


def draw_duck(im,x=80,y=51,**kwargs):
    sprite,pos=duck(x,y,**kwargs)
    im.alpha_composite(sprite,pos)


def note(d,x,y,kind=0):
    d.rectangle((x,y+5,x+3,y+7),fill=EDGE)
    d.line((x+3,y,x+3,y+6),fill=GOLD)
    if kind: d.line((x+3,y,x+7,y+2),fill=GOLD)


def scene(name):
    result=[]
    for i in range(N):
        t=i/(N-1)
        a=math.tau*t
        im=Image.new('RGBA',(W,H),BG)
        d=ImageDraw.Draw(im)
        # A tiny grounding shadow, not a full scene border or new UI chrome.
        d.line((49,53,110,53),fill=FAINT)
        blink=i in [7,8]
        if name=='waddle':
            x=44+72*t if t<.5 else 116-72*(t-.5)
            draw_duck(im,x=x,y=51-(i%4==0),step=a*4,face=1 if t<.5 else -1,blink=blink)
            for k in range(3):
                xx=round(x-25-k*6) if t<.5 else round(x+25+k*6)
                d.point((xx,54+k%2),fill=DIM)
        elif name=='snack':
            for k in range(5):
                if i<9+k*3: d.rectangle((104+k*3,51-k%2,105+k*3,52-k%2),fill=EDGE)
            bend=max(0,math.sin(a*3)) if .15<t<.85 else 0
            draw_duck(im,x=78,y=51,peck=bend,blink=blink)
            if i in [12,13,19,20]:
                d.point((115,40),fill=GOLD); d.point((119,43),fill=EDGE)
        elif name=='hop':
            jump=abs(math.sin(a*2))**.8*15
            draw_duck(im,x=80+math.sin(a)*15,y=51-jump,step=a*2,flap=jump/15,blink=blink)
            if i in [0,7,14,21,27]:
                for dx in [-17,17]: d.line((80+dx,52,80+dx*1.25,49),fill=EDGE)
        elif name=='butterfly':
            bx=80+math.cos(a)*34; by=22+math.sin(a*2)*8
            draw_duck(im,x=80+math.sin(a-.8)*23,y=51,step=a*5,face=1 if math.cos(a)>0 else -1,blink=blink)
            width=1+round(abs(math.sin(a*5))*3)
            d.rectangle((bx-width-1,by-2,bx-1,by+1),fill=GOLD)
            d.rectangle((bx+1,by-2,bx+width+1,by+1),fill=EDGE)
            d.line((bx,by-1,bx,by+3),fill=WING)
        elif name=='pond':
            # Reflected duck and layered ripples; feet genuinely disappear.
            draw_duck(im,x=80+math.sin(a)*18,y=52+round(math.sin(a*2)),blink=blink)
            d=ImageDraw.Draw(im)
            d.rectangle((0,51,W,71),fill=BG)
            for k in range(4):
                yy=51+k*4; span=22+k*8
                xx=80+round(math.sin(a+k)*7)
                d.line((xx-span,yy,xx+span,yy),fill=EDGE if k==0 else DIM)
                d.line((xx-span-8,yy+1,xx-span-3,yy+1),fill=DIM)
            d.rectangle((72+math.sin(a)*18,54,89+math.sin(a)*18,55),fill='#5D3B00')
        elif name=='shake':
            wiggle=round(math.sin(a*7)*2) if .15<t<.85 else 0
            draw_duck(im,x=80+wiggle,y=51,flap=abs(math.sin(a*5)),blink=.2<t<.8)
            for k in range(6):
                q=(t*2+k/6)%1
                x=80+math.cos(k*math.tau/6)*(15+q*20)
                y=35+math.sin(k*math.tau/6)*(8+q*12)
                if .2<t<.8: d.line((x,y,x+1,y-2),fill=EDGE)
        elif name=='dance':
            draw_duck(im,x=80+math.sin(a*2)*5,y=51-abs(math.sin(a*4))*4,
                      step=a*5,flap=abs(math.sin(a*3)),angle=math.sin(a*2)*9,blink=blink)
            for k in range(3):
                q=(t+k/3)%1
                note(d,45+k*30,round(38-q*24),k%2)
        elif name=='nap':
            # Ease down, close eyes, breathe, wake up instead of hard-cutting.
            rest=math.sin(math.pi*t)**.5
            draw_duck(im,x=80,y=51+round(rest*3),sleep=.12<t<.87,
                      angle=rest*-7,flap=0)
            if .18<t<.82:
                for k in range(3):
                    q=(t*1.2+k*.25)%1; x=107+round(q*15); y=28-round(q*16)
                    d.line([(x,y),(x+3,y),(x,y+3),(x+3,y+3)],fill=EDGE if k==0 else DIM)
            if t>.9:
                d.line((112,24,112,28),fill=GOLD); d.point((112,30),fill=GOLD)
        elif name=='quack':
            draw_duck(im,x=76,y=51,blink=blink,flap=.3 if 7<i<22 else 0)
            if 7<i<22:
                # Open bill and expanding little quack rings.
                d.rectangle((94,31,101,32),fill=BG)
                for k in range(3):
                    radius=7+k*4+(i%4)
                    d.arc((98-radius,33-radius,98+radius,33+radius),-42,42,fill=EDGE if k==0 else DIM)
        elif name=='tumble':
            turn=max(0,min(1,(t-.15)/.65))
            draw_duck(im,x=69+turn*23,y=51-math.sin(math.pi*turn)*11,
                      angle=-360*turn,flap=.8 if 0<turn<1 else 0,blink=0<turn<1)
            if .8<t<.95:
                for k in range(3):
                    x=81+k*10; y=18+(k%2)*3
                    d.line((x-2,y,x+2,y),fill=EDGE); d.line((x,y-2,x,y+2),fill=EDGE)
        result.append(im.convert('RGB'))
    return result


names=['waddle','snack','hop','butterfly','pond','shake','dance','nap','quack','tumble']
scenes={name:scene(name) for name in names}
# One exact shared palette; no adaptive color shimmer or transparent GIF halos.
colors=[(0,0,0),(255,199,63),(207,133,0),(237,160,0),(57,34,0),
        (80,51,0),(37,24,0),(93,59,0)]
pal=Image.new('P',(1,1))
pal.putpalette([v for c in colors for v in c]+[0]*(768-len(colors)*3))


def write_gif(filename,frames):
    indexed=[im.quantize(palette=pal,dither=Image.Dither.NONE) for im in frames]
    indexed[0].save(ROOT/filename,save_all=True,append_images=indexed[1:],
                    duration=120,loop=0,disposal=2,optimize=False)


order=names[:]
random.Random(73).shuffle(order)
write_gif('ducklife.gif',[frame for name in order for frame in scenes[name]])
for name in names: write_gif('duck-'+name+'.gif',scenes[name])
sheet=Image.new('RGB',(640,720))
d=ImageDraw.Draw(sheet)
for k,name in enumerate(names):
    x=(k%2)*320; y=(k//2)*144
    still=scenes[name][13].resize((320,144),Image.Resampling.NEAREST)
    sheet.paste(still,(x,y))
    d.text((x+8,y+8),name,fill=GOLD)
sheet.save(ROOT/'scenes.png',optimize=True)
preview=[frame.resize((640,288),Image.Resampling.NEAREST) for name in order for frame in scenes[name]]
write_gif('preview.gif',preview)
(ROOT/'scene-order.json').write_text(json.dumps({'order':order,'frames_per_scene':N,'frame_ms':120,'random_at_runtime':False},indent=2)+'\n')
