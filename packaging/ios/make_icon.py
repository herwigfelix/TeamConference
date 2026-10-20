"""Erzeugt das iOS-App-Icon (1024×1024, ohne Alphakanal) für TeamConference.

Aufruf: python3 packaging/ios/make_icon.py <ausgabe.png>
"""
import sys
from PIL import Image, ImageDraw

S = 4  # Überabtastung für glatte Kanten
N = 1024 * S
BG_TOP, BG_BOTTOM = (34, 40, 74), (20, 20, 38)
FG = (240, 244, 255)
ACCENT = (76, 201, 240)

img = Image.new("RGB", (N, N))
d = ImageDraw.Draw(img)
for y in range(N):
    t = y / (N - 1)
    d.line([(0, y), (N, y)], fill=tuple(round(a + (b - a) * t) for a, b in zip(BG_TOP, BG_BOTTOM)))

cx = N // 2
u = N / 1024
dy = 40  # Motiv vertikal mittig setzen
# Mikrofonkapsel
w, top, bottom = 200 * u, (210 + dy) * u, (590 + dy) * u
d.rounded_rectangle([cx - w / 2, top, cx + w / 2, bottom], radius=w / 2, fill=FG)
# Bügel
lw = round(34 * u)
r = 165 * u
cy = (450 + dy) * u
d.arc([cx - r, cy - r, cx + r, cy + r], start=0, end=180, fill=FG, width=lw)
d.line([(cx - r + lw / 2 - 1, cy), (cx - r + lw / 2 - 1, cy - 40 * u)], fill=FG, width=lw)
d.line([(cx + r - lw / 2, cy), (cx + r - lw / 2, cy - 40 * u)], fill=FG, width=lw)
# Stiel und Fuß
d.line([(cx, cy + r), (cx, (760 + dy) * u)], fill=FG, width=lw)
d.rounded_rectangle([cx - 130 * u, (745 + dy) * u, cx + 130 * u, (745 + dy) * u + lw], radius=lw / 2, fill=FG)
# Schallwellen links und rechts
for i, rr in enumerate((280, 370)):
    rr *= u
    box = [cx - rr, (400 + dy) * u - rr, cx + rr, (400 + dy) * u + rr]
    width = round((30 - 6 * i) * u)
    d.arc(box, start=-40, end=40, fill=ACCENT, width=width)
    d.arc(box, start=140, end=220, fill=ACCENT, width=width)

img.resize((1024, 1024), Image.LANCZOS).save(sys.argv[1])
