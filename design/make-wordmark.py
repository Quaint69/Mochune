#!/usr/bin/env python3
"""Original rounded, banner-inspired Mochune wordmark font (not a full UI family). Requires fontTools."""
from pathlib import Path
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.svgLib.path import parse_path
from fontTools.feaLib.builder import addOpenTypeFeaturesFromString
ROOT=Path(__file__).resolve().parent.parent
paths={
'M':('M60 55V645Q60 700 115 700H175Q202 700 217 674L350 438L483 674Q498 700 528 700H590Q645 700 645 645V55Q645 0 590 0H555Q500 0 500 55V385L411 230Q393 200 370 200H330Q308 200 289 230L205 385V55Q205 0 150 0H115Q60 0 60 55Z',705),
'o':('M40 260Q40 530 290 530Q540 530 540 260Q540 -10 290 -10Q40 -10 40 260Z M205 260Q205 140 290 140Q375 140 375 260Q375 380 290 380Q205 380 205 260Z',580),
'c':('M505 438Q525 410 500 388L454 340Q434 320 408 342Q364 383 300 383Q200 383 200 260Q200 137 300 137Q364 137 408 178Q434 200 454 180L500 132Q525 110 505 82Q430 -10 300 -10Q40 -10 40 260Q40 530 300 530Q430 530 505 438Z',550),
'h':('M60 55V665Q60 720 115 720H155Q210 720 210 665V472Q259 530 337 530Q520 530 520 330V55Q520 0 465 0H425Q370 0 370 55V297Q370 385 300 385Q210 385 210 285V55Q210 0 155 0H115Q60 0 60 55Z',580),
'u':('M60 480Q60 530 110 530H160Q210 530 210 480V220Q210 135 290 135Q370 135 370 220V480Q370 530 420 530H470Q520 530 520 480V55Q520 0 470 0H435Q389 0 380 47Q340 -10 260 -10Q60 -10 60 220Z',580),
'n':('M60 55V465Q60 520 115 520H150Q196 520 205 480Q255 530 337 530Q520 530 520 330V55Q520 0 465 0H425Q370 0 370 55V297Q370 385 300 385Q210 385 210 285V55Q210 0 155 0H115Q60 0 60 55Z',580),
'e':('M525 225H203Q220 125 310 125Q377 125 425 161Q450 180 468 158L505 112Q524 90 503 80Q420 -10 300 -10Q40 -10 40 260Q40 530 295 530Q540 530 540 285V260Q540 225 525 225Z M205 330H377Q360 400 295 400Q225 400 205 330Z',580),
}
fb=FontBuilder(1000,isTTF=True);order=['.notdef','space']+list(paths);fb.setupGlyphOrder(order)
glyphs={};metrics={}
for name in order:
 pen=TTGlyphPen(None)
 if name in paths:data,advance=paths[name];parse_path(data,pen)
 else:advance=300
 glyphs[name]=pen.glyph();metrics[name]=(advance,0)
fb.setupCharacterMap({ord(c):c for c in paths}|{32:'space'});fb.setupGlyf(glyphs);fb.setupHorizontalMetrics(metrics);fb.setupHorizontalHeader(ascent=800,descent=-200)
fb.setupNameTable({'familyName':'Mochune Display','styleName':'Bold','uniqueFontIdentifier':'MochuneDisplay-Bold-1.1','fullName':'Mochune Display Bold','psName':'MochuneDisplay-Bold','version':'Version 1.1','copyright':'Original banner-inspired Mochune wordmark outlines. 2026. MIT License.'})
fb.setupOS2(sTypoAscender=800,sTypoDescender=-200,usWinAscent=800,usWinDescent=200,sxHeight=530,sCapHeight=700,usWeightClass=750);fb.setupPost();fb.setupMaxp()
addOpenTypeFeaturesFromString(fb.font,'feature kern { pos M o -25; pos o c -20; pos c h -10; pos h u -10; pos u n -15; pos n e -15; } kern;')
fb.save(ROOT/'ui/fonts/MochuneDisplay.ttf')
