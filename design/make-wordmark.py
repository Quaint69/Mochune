#!/usr/bin/env python3
"""Regenerate the original 1.7 KiB title font. Requires fontTools (development only)."""
from pathlib import Path
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.svgLib.path import parse_path
root=Path(__file__).resolve().parent.parent
# The display face is drawn from original geometric outlines, not a modified system font.
paths={
'M':('M65 0V700H190L375 240L560 700H685V0H575V495L420 100H330L175 495V0Z',750),
'o':('M45 255Q45 520 280 520Q515 520 515 255Q515 -10 280 -10Q45 -10 45 255Z M150 255Q150 80 280 80Q410 80 410 255Q410 430 280 430Q150 430 150 255Z',560),
'h':('M65 0V720H165V455Q225 520 320 520Q510 520 510 320V0H410V305Q410 430 290 430Q165 430 165 300V0Z',570),
'n':('M65 0V510H155V450Q220 520 315 520Q510 520 510 320V0H410V305Q410 430 290 430Q165 430 165 300V0Z',570),
'D':('M70 0V700H300Q610 700 610 350Q610 0 300 0Z M175 100H300Q500 100 500 350Q500 600 300 600H175Z',650),
'u':('M60 510H160V195Q160 90 270 90Q380 90 380 195V510H480V0H390V55Q335 -10 250 -10Q60 -10 60 190Z',540),
'c':('M470 425L390 365Q345 430 260 430Q150 430 150 255Q150 80 260 80Q345 80 390 145L470 85Q395 -10 260 -10Q45 -10 45 255Q45 520 260 520Q395 520 470 425Z',515),
'k':('M65 0V720H165V280L350 510H475L280 275L495 0H365L165 255V0Z',525),
'T':('M25 600V700H605V600H370V0H260V600Z',630),
'b':('M65 0V720H165V465Q220 520 310 520Q510 520 510 255Q510 -10 310 -10Q220 -10 155 55V0Z M165 255Q165 80 285 80Q405 80 405 255Q405 430 285 430Q165 430 165 255Z',560),
'e':('M485 220H150Q160 80 285 80Q365 80 410 145L485 85Q415 -10 280 -10Q45 -10 45 255Q45 520 275 520Q490 520 490 275Z M150 305H385Q375 430 275 430Q165 430 150 305Z',535),
}
fb=FontBuilder(1000,isTTF=True); order=['.notdef','space']+list(paths);fb.setupGlyphOrder(order)
glyphs={};metrics={}
for name in order:
 pen=TTGlyphPen(None)
 if name in paths:
  data,advance=paths[name];parse_path(data,pen)
 else:
  advance=300
  if name=='.notdef':parse_path('M40 0V700H260V0Z M80 40H220V660H80Z',pen)
 glyphs[name]=pen.glyph();metrics[name]=(advance,0)
fb.setupCharacterMap({ord(c):c for c in paths}|{32:'space'})
fb.setupGlyf(glyphs);fb.setupHorizontalMetrics(metrics);fb.setupHorizontalHeader(ascent=800,descent=-200)
fb.setupNameTable({'familyName':'Mochune Display','styleName':'Regular','uniqueFontIdentifier':'MochuneDisplay-Regular-1.0','fullName':'Mochune Display Regular','psName':'MochuneDisplay-Regular','version':'Version 1.0','copyright':'Original Mochune display lettering. 2026. MIT License.'})
fb.setupOS2(sTypoAscender=800,sTypoDescender=-200,usWinAscent=800,usWinDescent=200);fb.setupPost();fb.setupMaxp()
fb.save(root/'ui/fonts/MochuneDisplay.ttf')
