"""Build Overlook's portable SVG identity and theme tokens.

Requires fontTools (including brotli), Pillow, and rsvg-convert.
Run from any directory; fonts and artwork are local to the brand kit.
"""
from pathlib import Path
import json
import subprocess
from fontTools.ttLib import TTFont
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / "assets"
ASSETS.mkdir(exist_ok=True)
PALETTE = {
    "midnight": "#101C24", "ivory": "#F0EBDD", "paper": "#FAF8F2",
    "slate": "#365F73", "mist": "#C6D5D5", "pine": "#536E63", "amber": "#FF9D32",
}
THEMES = {
    "light": {
        "background": "#CDD1C9", "surface": "#E1E2DC", "sidebar": "#C4CBC1",
        "foreground": "#29332E", "muted": "#4B564D", "divider": "#A7B0A5",
        "controlBorder": "#657267", "hover": "#D5DAD1", "accent": "#3F6654",
        "action": "#3F6654", "onAction": "#FFFFFF", "selection": "#BDCDBF",
        "selectionText": "#29332E", "focus": "#92581D", "success": "#386449",
        "warning": "#805015", "error": "#A13E37", "brandAccent": "#92581D",
    },
    "dark": {
        "background": "#101A21", "surface": "#17242C", "sidebar": "#121F27",
        "foreground": "#F0EBDD", "muted": "#A8B7BA", "divider": "#30424B",
        "controlBorder": "#71878D", "hover": "#23353F", "accent": "#A9CDD9",
        "action": "#A9CDD9", "onAction": "#10232D", "selection": "#28404C",
        "selectionText": "#DCECEF", "focus": "#FFBA6B", "success": "#A8CEB1",
        "warning": "#F1BE78", "error": "#FFB0A6", "brandAccent": "#FF9D32",
    },
}

def svg(content, width=128, height=128, title="Overlook"):
    return f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" role="img" aria-label="{title}"><title>{title}</title>{content}</svg>\n'

def mark(ink, beacon=None, small=False):
    # The horizon sits below the aperture's optical centre to preserve open sky.
    ridge = 'M 21 79 L 48 59 L 72 77 L 92 64 L 109 78'
    return (f'<circle cx="64" cy="64" r="49" fill="none" stroke="{ink}" stroke-width="10"/>'
            f'<path d="{ridge}" fill="none" stroke="{ink}" stroke-width="8" stroke-linejoin="round" stroke-linecap="round"/>'
            + ('' if small else f'<circle cx="79" cy="42" r="5" fill="{beacon or ink}"/>'))

for name, ink, beacon in [
    ('light', PALETTE['midnight'], PALETTE['slate']),
    ('dark', PALETTE['ivory'], PALETTE['amber']),
    ('ink', PALETTE['midnight'], PALETTE['midnight']),
    ('ivory', PALETTE['ivory'], PALETTE['ivory']),
]:
    (ASSETS / f'overlook-mark-{name}.svg').write_text(svg(mark(ink, beacon)))
(ASSETS / 'overlook-symbolic.svg').write_text(svg(mark('currentColor', small=True)))

font = TTFont(ROOT / 'fonts/SourceSerif4Display-Regular.otf')
glyphs = font.getGlyphSet()
cmap = font.getBestCmap()
units = font['head'].unitsPerEm
scale = 80 / units
cursor = 0
pen = SVGPathPen(glyphs)
for char in 'Overlook':
    glyph_name = cmap[ord(char)]
    glyphs[glyph_name].draw(TransformPen(pen, (scale, 0, 0, -scale, cursor, 85)))
    cursor += glyphs[glyph_name].width * scale - 1.0
wordpath = pen.getCommands()
wordwidth = round(cursor + 4)
for name, ink, beacon in [('light',PALETTE['midnight'],PALETTE['slate']), ('dark',PALETTE['ivory'],PALETTE['amber']), ('mono',PALETTE['midnight'],PALETTE['midnight'])]:
    word = f'<path d="{wordpath}" fill="{ink}"/>'
    (ASSETS / f'overlook-wordmark-{name}.svg').write_text(svg(word, wordwidth, 106))
    lockup = f'<g transform="translate(0 6) scale(.78)">{mark(ink,beacon)}</g><g transform="translate(116 0)">{word}</g>'
    (ASSETS / f'overlook-lockup-{name}.svg').write_text(svg(lockup, wordwidth+120, 110))

app = '<rect x="4" y="4" width="120" height="120" rx="28" fill="#101C24"/><g transform="translate(10 10) scale(.84375)">'+mark('#F0EBDD','#FF9D32')+'</g>'
(ASSETS / 'overlook-app-icon.svg').write_text(svg(app))
for size in (16, 24, 32, 48, 64, 128, 256, 512):
    content = app if size >= 32 else '<rect x="4" y="4" width="120" height="120" rx="28" fill="#101C24"/><g transform="translate(10 10) scale(.84375)">'+mark('#F0EBDD',small=True)+'</g>'
    subprocess.run(['rsvg-convert','-w',str(size),'-h',str(size),'-o',str(ASSETS/f'overlook-icon-{size}.png')],input=svg(content).encode(),check=True)
for name in ('light','dark','mono'):
    subprocess.run(['rsvg-convert','-w','1600','-o',str(ASSETS/f'overlook-lockup-{name}.png'),str(ASSETS/f'overlook-lockup-{name}.svg')],check=True)

# Locally bundled display font and interchange-friendly app font.
font.flavor = 'woff2'
font.save(ROOT / 'fonts/SourceSerif4Display-Regular.woff2')
ui = TTFont(ROOT / 'fonts/InterVariable.woff2')
ui.flavor = None
ui.save(ROOT / 'fonts/InterVariable.ttf')
(ROOT / 'tokens.json').write_text(json.dumps({'brand':'Overlook','version':'1.0','palette':PALETTE,'themes':THEMES,'typography':{'display':'Source Serif 4 Display','interface':'Inter','uiFallback':'Noto Sans, sans-serif'}},indent=2)+'\n')
css = [':root {'] + [f'  --overlook-{key}: {value};' for key,value in PALETTE.items()] + ['}']
for theme, values in THEMES.items():
    css += [f'[data-overlook-theme="{theme}"] {{', f'  color-scheme: {theme};']
    css += [f'  --ol-{key}: {value};' for key,value in values.items()]
    css += ['}']
(ROOT / 'tokens.css').write_text('\n'.join(css)+'\n')

def luminance(color):
    channels = [int(color[i:i+2],16)/255 for i in (1,3,5)]
    rgb = [c/12.92 if c<=.04045 else ((c+.055)/1.055)**2.4 for c in channels]
    return sum(a*b for a,b in zip(rgb,(.2126,.7152,.0722)))
def contrast(a,b):
    hi,lo = sorted((luminance(a),luminance(b)),reverse=True)
    return (hi+.05)/(lo+.05)
report = []
for theme,t in THEMES.items():
    pairs = [('foreground','surface',4.5),('muted','surface',4.5),('muted','sidebar',4.5),('onAction','action',4.5),('selectionText','selection',4.5),('accent','surface',4.5),('focus','surface',3),('controlBorder','surface',3),('success','surface',4.5),('warning','surface',4.5),('error','surface',4.5)]
    for fg,bg,target in pairs:
        ratio = contrast(t[fg],t[bg]); assert ratio>=target,(theme,fg,bg,ratio)
        report.append({'theme':theme,'foreground':fg,'background':bg,'ratio':round(ratio,2),'minimum':target})
(ROOT/'contrast-check.json').write_text(json.dumps(report,indent=2)+'\n')
print(f'Built {len(list(ASSETS.iterdir()))} brand assets; {len(report)} contrast pairs passed.')
