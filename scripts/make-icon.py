#!/usr/bin/env python3
"""Draw Sightline's icon, at every size the platforms ask for.

    python3 scripts/make-icon.py

The mark is the boundary.

Everything this program does happens at one place: an agent asks whether it may
do something, and Sightline answers before the thing happens. So the icon is
that — a vertical line with work crossing it. Two traces come in from the left
and continue out the right; the middle one reaches the line and stops there.
Nothing else in a dock looks like it, and it is the only picture of this product
that is also true about it.

What it replaced was a crosshair built out of dots. That mark had two problems.
It was a weapon sight, which is what the product used to be called and is no
longer; and a grid of evenly spaced dots is a shape that belongs to any
launcher, any grid, any anything. This one cannot be about another program.

It is deliberately flat. An earlier version was modelled — a lit aperture with a
machined ring and a cast shadow — and at any size above a dock it stopped being
a sight and started being an eye. A mark that has to work at sixteen pixels
wants a silhouette, not a rendering.

The boundary is drawn heavier than the traces because it is the subject: the
line is the thing doing the deciding, and the traces are what happens to be
crossing it. Drawn at three times the final size and reduced, which is how the
edges come out clean without a vector renderer on the machine.
"""

from PIL import Image, ImageDraw

FINAL = 1024
S = FINAL * 3

# A neutral dark tile, lit very slightly from the top the way macOS icons are.
# Neutral rather than navy: the accent is the only colour in the mark, and a
# blue ground takes the edge off the one blue that is supposed to carry meaning.
TILE_TOP = (36, 36, 42)
TILE_BOTTOM = (13, 13, 16)

BOUNDARY = (245, 245, 247)  # Sightline itself
TRACE = (10, 132, 255)      # the work crossing it — the system accent

# Everything below is a fraction of the tile, so the mark scales exactly and the
# vector and the raster are generated from one set of numbers.
LANES = (0.315, 0.500, 0.685)   # three, far enough apart to survive 32px
STOPPED = 1                     # the middle one is the subject
TRACE_W = 0.055
BOUNDARY_W = 0.086
# The line runs taller than the traces span and heavier than they are drawn.
# Both on purpose: three horizontal bars with a short perpendicular handle is
# the universal sliders-and-filters glyph, and the first version of this was one
# adjustment away from being it. A line that dominates the tile is a boundary;
# a stub crossing some rules is a control panel.
BOUNDARY_TOP, BOUNDARY_BOTTOM = 0.150, 0.850
IN_FROM, OUT_TO = 0.175, 0.775
# Where the refused trace gives up. Short of the line rather than against it:
# a bar that stops exactly at an edge reads as meeting it, and the gap is what
# says it did not get through.
HALT = 0.415


def bars():
    """The mark, as rounded bars: (x0, y0, x1, y1, radius, colour).

    One list, used to draw both the PNG and the SVG. Hand-writing the vector
    separately is how the two drift apart, and then only one of them is the
    icon anybody actually sees.
    """
    out = []
    half = TRACE_W / 2
    for i, y in enumerate(LANES):
        if i == STOPPED:
            # Arrives, and gets no further.
            out.append((IN_FROM, y - half, HALT, y + half, half, TRACE))
            continue
        # Through, and out the other side. Drawn as one bar rather than two so
        # the join cannot show at large sizes; the boundary is painted over it,
        # which is what makes it read as passing behind.
        out.append((IN_FROM, y - half, OUT_TO, y + half, half, TRACE))
    # Last, so it sits over the traces rather than under them: what they meet.
    bh = BOUNDARY_W / 2
    out.append(
        (0.5 - bh, BOUNDARY_TOP, 0.5 + bh, BOUNDARY_BOTTOM, bh, BOUNDARY)
    )
    return out


def tile():
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ramp = Image.new("RGB", (1, 256))
    for y in range(256):
        t = y / 255
        ramp.putpixel((0, y), tuple(round(a + (b - a) * t) for a, b in zip(TILE_TOP, TILE_BOTTOM)))
    corners = Image.new("L", (S, S), 0)
    # 22.3% is the macOS rounded-rectangle, near enough that the icon sits in a
    # dock without looking like it came from somewhere else.
    ImageDraw.Draw(corners).rounded_rectangle(
        [0, 0, S - 1, S - 1], radius=int(S * 0.223), fill=255
    )
    img.paste(ramp.resize((S, S), Image.BICUBIC).convert("RGBA"), (0, 0), corners)
    return img


def mark(img):
    draw = ImageDraw.Draw(img)
    for x0, y0, x1, y1, r, colour in bars():
        draw.rounded_rectangle(
            [x0 * S, y0 * S, x1 * S, y1 * S], radius=r * S, fill=colour + (255,)
        )
    return img


def svg(path):
    """The same mark as vector, from the same numbers, so the two cannot drift."""
    n = 512
    hexed = lambda c: "#%02x%02x%02x" % c
    shapes = "\n".join(
        f'  <rect x="{x0 * n:.1f}" y="{y0 * n:.1f}" '
        f'width="{(x1 - x0) * n:.1f}" height="{(y1 - y0) * n:.1f}" '
        f'rx="{r * n:.1f}" fill="{hexed(c)}"/>'
        for x0, y0, x1, y1, r, c in bars()
    )
    open(path, "w").write(f"""<!-- Sightline. Generated by scripts/make-icon.py; edit that, not this. -->
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {n} {n}" width="{n}" height="{n}">
  <defs>
    <linearGradient id="tile" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="{hexed(TILE_TOP)}"/>
      <stop offset="1" stop-color="{hexed(TILE_BOTTOM)}"/>
    </linearGradient>
  </defs>
  <rect width="{n}" height="{n}" rx="{n * 0.223:.0f}" fill="url(#tile)"/>
{shapes}
</svg>
""")


def icns(path, img):
    """Write the macOS icon set.

    By hand, because no icns tool is installed on the machine this is built on
    and the alternative was leaving the file stale — which is worse than it
    sounds: it is the icon macOS would actually show, so the one platform whose
    conventions this mark was drawn for would have been the one still shipping
    the old one.

    The format is a magic word, a total length, and then typed chunks. Every
    type below takes a PNG directly, which modern macOS has read for years.
    """
    import struct
    from io import BytesIO

    # (type, pixel size). The @2x types carry the same pixels as the size above
    # them, which is what the format expects rather than a separate rendering.
    kinds = [
        (b"ic07", 128), (b"ic08", 256), (b"ic09", 512), (b"ic10", 1024),
        (b"ic11", 32), (b"ic12", 64), (b"ic13", 256), (b"ic14", 512),
    ]
    chunks = []
    for kind, size in kinds:
        buf = BytesIO()
        img.resize((size, size), Image.LANCZOS).save(buf, format="PNG")
        data = buf.getvalue()
        chunks.append(kind + struct.pack(">I", len(data) + 8) + data)
    body = b"".join(chunks)
    open(path, "wb").write(b"icns" + struct.pack(">I", len(body) + 8) + body)


def build():
    img = mark(tile()).resize((FINAL, FINAL), Image.LANCZOS)
    here = __file__.rsplit("/", 2)[0] + "/crates/gui/icons"
    svg(f"{here}/icon.svg")
    for name, size in [("32x32.png", 32), ("128x128.png", 128),
                       ("128x128@2x.png", 256), ("icon.png", 512)]:
        img.resize((size, size), Image.LANCZOS).save(f"{here}/{name}")
    img.resize((256, 256), Image.LANCZOS).save(
        f"{here}/icon.ico", sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)]
    )
    icns(f"{here}/icon.icns", img)
    img.save(f"{here}/icon-master.png")
    print("wrote", here)


if __name__ == "__main__":
    build()
