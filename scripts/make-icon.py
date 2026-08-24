#!/usr/bin/env python3
"""Draw Sightline's icon, at every size the platforms ask for.

    python3 scripts/make-icon.py

A crosshair built out of dots. The arms are three dots thick at the middle and
taper to one at the tips, and every dot shrinks as it gets further from the
centre, so the mark reads as converging on a point rather than as a plus sign.
The point itself is the only thing in the accent colour.

It is deliberately flat. An earlier version was modelled — a lit aperture with
a machined ring and a cast shadow — and at any size above a dock it stopped
being a sight and started being an eye. A mark that has to work at sixteen
pixels wants a silhouette, not a rendering.

Drawn at three times the final size and reduced, which is how the circles come
out clean without a vector renderer on the machine.
"""

from PIL import Image, ImageDraw, ImageFilter

FINAL = 1024
S = FINAL * 3

TILE_TOP = (30, 41, 59)        # slate, lifted at the top
TILE_BOTTOM = (11, 15, 26)
DOT = (226, 232, 240)          # the arms
# The aiming point has to be the brightest thing in the mark. Accent blue was
# tried and is a third darker than the dots around it, so the middle read as a
# hole punched in the crosshair. The accent survives as the glow behind it.
CENTRE = (240, 249, 255)
GLOW = (96, 165, 250)

CELLS = 6                      # dots from the middle to the tip of an arm
SPACING = 0.0570               # of the tile, between dot centres
DOT_R = 0.0285                 # the middle dot's radius


def thickness(step):
    """How many dots either side of the arm's centre line, this far out.

    Three across near the middle, one across from there on. The taper is what
    makes it converge rather than sit there as a plus sign.
    """
    return 1 if step <= 2 else 0


def tile():
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ramp = Image.new("RGB", (1, 256))
    for y in range(256):
        t = y / 255
        ramp.putpixel((0, y), tuple(round(a + (b - a) * t) for a, b in zip(TILE_TOP, TILE_BOTTOM)))
    corners = Image.new("L", (S, S), 0)
    ImageDraw.Draw(corners).rounded_rectangle(
        [0, 0, S - 1, S - 1], radius=int(S * 0.223), fill=255
    )
    img.paste(ramp.resize((S, S), Image.BICUBIC).convert("RGBA"), (0, 0), corners)
    return img


def crosshair(img):
    d = ImageDraw.Draw(img)
    cx = cy = S / 2
    step = S * SPACING

    # The glow, first, so the dots sit on it: it is what carries the accent
    # colour now that the middle dot itself is nearly white.
    halo = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    hr = S * DOT_R * 2.6
    ImageDraw.Draw(halo).ellipse([cx - hr, cy - hr, cx + hr, cy + hr], fill=GLOW + (170,))
    img.alpha_composite(halo.filter(ImageFilter.GaussianBlur(S * DOT_R * 0.85)))

    def dot(ix, iy):
        """One dot of the matrix, at grid position (ix, iy)."""
        out = max(abs(ix), abs(iy))
        # Smaller the further out, so the arms point inwards.
        scale = 1 - 0.56 * (out / CELLS) ** 1.05
        r = S * DOT_R * scale
        x, y = cx + ix * step, cy + iy * step
        if out == 0:
            colour, r = CENTRE, r * 1.06
        else:
            colour = DOT
        d.ellipse([x - r, y - r, x + r, y + r], fill=colour + (255,))

    for n in range(0, CELLS + 1):
        spread = range(-thickness(n), thickness(n) + 1)
        if n == 0:
            dot(0, 0)
            continue
        # The corners (±n, ±n) are produced by both the vertical and the
        # horizontal arm; draw each cell once so a future per-dot tweak cannot
        # double-apply there.
        seen = set()
        for off in spread:
            for cell in ((n, off), (-n, off), (off, n), (off, -n)):
                if cell not in seen:
                    seen.add(cell)
                    dot(*cell)
    return img


def positions():
    """Every dot as (x, y, radius, colour), in fractions of the tile."""
    out = []
    for n in range(0, CELLS + 1):
        if n == 0:
            cells = [(0, 0)]
        else:
            cells = []
            seen = set()
            for off in range(-thickness(n), thickness(n) + 1):
                for cell in ((n, off), (-n, off), (off, n), (off, -n)):
                    if cell not in seen:
                        seen.add(cell)
                        cells.append(cell)
        for ix, iy in cells:
            far = max(abs(ix), abs(iy))
            r = DOT_R * (1 - 0.56 * (far / CELLS) ** 1.05)
            if far == 0:
                r *= 1.06
            out.append((0.5 + ix * SPACING, 0.5 + iy * SPACING, r,
                        CENTRE if far == 0 else DOT))
    return out


def svg(path):
    """The same mark as vector, generated from the same numbers so the two
    cannot drift apart — which is what happens when one is hand-written."""
    n = 512
    hexed = lambda c: "#%02x%02x%02x" % c
    dots = "\n".join(
        f'  <circle cx="{x * n:.1f}" cy="{y * n:.1f}" r="{r * n:.1f}" fill="{hexed(c)}"/>'
        for x, y, r, c in positions()
    )
    open(path, "w").write(f"""<!-- Sightline. Generated by scripts/make-icon.py; edit that, not this. -->
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {n} {n}" width="{n}" height="{n}">
  <defs>
    <linearGradient id="tile" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="{hexed(TILE_TOP)}"/>
      <stop offset="1" stop-color="{hexed(TILE_BOTTOM)}"/>
    </linearGradient>
    <radialGradient id="glow">
      <stop offset="0" stop-color="{hexed(GLOW)}" stop-opacity=".75"/>
      <stop offset="1" stop-color="{hexed(GLOW)}" stop-opacity="0"/>
    </radialGradient>
  </defs>
  <rect width="{n}" height="{n}" rx="{n * 0.223:.0f}" fill="url(#tile)"/>
  <circle cx="{n / 2:.0f}" cy="{n / 2:.0f}" r="{DOT_R * 3.4 * n:.1f}" fill="url(#glow)"/>
{dots}
</svg>
""")


def build():
    img = crosshair(tile()).resize((FINAL, FINAL), Image.LANCZOS)
    here = __file__.rsplit("/", 2)[0] + "/crates/gui/icons"
    svg(f"{here}/icon.svg")
    for name, size in [("32x32.png", 32), ("128x128.png", 128),
                       ("128x128@2x.png", 256), ("icon.png", 512)]:
        img.resize((size, size), Image.LANCZOS).save(f"{here}/{name}")
    img.resize((256, 256), Image.LANCZOS).save(
        f"{here}/icon.ico", sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)]
    )
    img.save(f"{here}/icon-master.png")
    print("wrote", here)


if __name__ == "__main__":
    build()
