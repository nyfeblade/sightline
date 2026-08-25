#!/usr/bin/env python3
"""Draw Sightline's icon, at every size the platforms ask for.

    python3 scripts/make-icon.py

A slit of light in a dark plate.

The name is the brief: a sightline is the clear line you see along, and this
program is the one place everything an agent does has to pass through. So the
mark is that line — luminous, narrow, with the glow of something lit from
behind, standing in a deep graphite tile.

Two earlier marks are worth recording because both failed the same way. A
crosshair of dots was a weapon sight, which is what this used to be called and
is not; and a grid of dots belongs to any launcher. Then a line with traces
crossing it, which was *true* — it is exactly what the boundary does — and still
wrong, because three horizontal bars and a perpendicular stroke is the universal
sliders-and-filters glyph. Being an accurate diagram did not save it.

That is the lesson this one is drawn from. An icon is not a diagram of the
architecture. It is an object with a silhouette, and it wants light and depth
rather than an explanation. What is left here is one form, lit.

Drawn at three times the final size and reduced, which is how the glow and the
edges come out clean without a vector renderer on the machine.
"""

from PIL import Image, ImageChops, ImageDraw, ImageFilter

FINAL = 1024
S = FINAL * 3

# Deep graphite, lit from the top, the way a physical plate would be.
TILE_TOP = (26, 26, 32)
TILE_BOTTOM = (6, 6, 8)

# The plate the slot is cut in. Lighter than the tile, so it reads as an object
# standing in it rather than as a panel painted on it.
PLATE_TOP = (78, 78, 90)
PLATE_BOTTOM = (24, 24, 30)

CORE = (255, 255, 255)       # the slot itself
BLOOM = (196, 216, 255)      # what it throws — the faintest cool cast

PLATE_INSET = 0.135
PLATE_R = 0.050
SLOT_W = 0.060
SLOT_TOP, SLOT_BOTTOM = 0.215, 0.785
# Three passes of light: a wide one that fills the tile behind the plate, a
# tight one that lights the lip of the cut, and the core.
BACK_BLUR, BACK_ALPHA = 0.150, 0.90
LIP_BLUR, LIP_ALPHA = 0.045, 0.85


def geometry():
    """The plate and the slot cut in it, in fractions of the tile."""
    plate = (PLATE_INSET, PLATE_INSET, 1 - PLATE_INSET, 1 - PLATE_INSET)
    half = SLOT_W / 2
    slot = (0.5 - half, SLOT_TOP, 0.5 + half, SLOT_BOTTOM)
    return plate, slot


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
    """A plate with a slot cut in it, lit from behind.

    The order is the whole trick, and it is why this is not a line drawn on a
    surface. The light goes down first, on the bare tile. Then the plate is laid
    over it with the slot punched *out* of its mask, so the only place the light
    survives is the cut. Then a tighter pass on top catches the lip of the cut
    the way an edge picks up light it is standing in front of.
    """
    plate, slot = geometry()
    img.alpha_composite(soft(slot, BACK_BLUR, BACK_ALPHA, BLOOM))

    # The plate, with the slot missing from it.
    strip = Image.new("RGB", (1, 256))
    for y in range(256):
        t = y / 255
        strip.putpixel(
            (0, y), tuple(round(a + (b - a) * t) for a, b in zip(PLATE_TOP, PLATE_BOTTOM))
        )
    mask = Image.new("L", (S, S), 0)
    cut = ImageDraw.Draw(mask)
    cut.rounded_rectangle([v * S for v in plate], radius=PLATE_R * S, fill=255)
    cut.rounded_rectangle([v * S for v in slot], radius=SLOT_W / 2 * S, fill=0)
    layer = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    layer.paste(strip.resize((S, S), Image.BICUBIC).convert("RGBA"), (0, 0), mask)
    img.alpha_composite(layer)

    img.alpha_composite(soft(slot, LIP_BLUR, LIP_ALPHA, BLOOM))

    # The core, with falloff along its length. A bar of even brightness reads as
    # a painted line; light through a cut is strongest in the middle and fades
    # towards the ends, and that gradient is most of the difference.
    core = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(core).rounded_rectangle(
        [v * S for v in slot], radius=SLOT_W / 2 * S, fill=CORE + (255,)
    )
    img.alpha_composite(fade(core, slot[1], slot[3]))
    return img


def soft(box, blur, alpha, colour):
    """The shape, blurred, as its own layer of light."""
    g = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(g).rounded_rectangle(
        [v * S for v in box], radius=SLOT_W / 2 * S, fill=colour + (round(255 * alpha),)
    )
    return g.filter(ImageFilter.GaussianBlur(blur * S))


def fade(layer, y0, y1):
    """Brightest a little above centre, almost gone at the two ends."""
    ramp = Image.new("L", (1, 256))
    for i in range(256):
        d = abs(i / 255 - 0.46) / 0.54
        ramp.putpixel((0, i), round(255 * max(0.0, 1 - d ** 1.7)))
    top, bottom = round(y0 * S), round(y1 * S)
    f = Image.new("L", (S, S), 0)
    f.paste(ramp.resize((S, bottom - top), Image.BICUBIC), (0, top))
    layer.putalpha(ImageChops.multiply(layer.getchannel("A"), f))
    return layer


def svg(path):
    """The same mark as vector, from the same numbers, so the two cannot drift.

    The slot is a hole in the plate here too — a mask, not a lighter rectangle
    laid on top — so the light behind it is the same light in both renderings.
    """
    n = 512
    hexed = lambda c: "#%02x%02x%02x" % c
    plate, slot = geometry()
    box = lambda b, r: (
        f'x="{b[0]*n:.1f}" y="{b[1]*n:.1f}" width="{(b[2]-b[0])*n:.1f}" '
        f'height="{(b[3]-b[1])*n:.1f}" rx="{r*n:.1f}"'
    )
    open(path, "w").write(f"""<!-- Sightline. Generated by scripts/make-icon.py; edit that, not this. -->
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {n} {n}" width="{n}" height="{n}">
  <defs>
    <linearGradient id="tile" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="{hexed(TILE_TOP)}"/>
      <stop offset="1" stop-color="{hexed(TILE_BOTTOM)}"/>
    </linearGradient>
    <linearGradient id="plate" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="{hexed(PLATE_TOP)}"/>
      <stop offset="1" stop-color="{hexed(PLATE_BOTTOM)}"/>
    </linearGradient>
    <linearGradient id="along" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#fff" stop-opacity="0"/>
      <stop offset="0.46" stop-color="#fff" stop-opacity="1"/>
      <stop offset="1" stop-color="#fff" stop-opacity="0"/>
    </linearGradient>
    <filter id="back" x="-200%" y="-60%" width="500%" height="220%">
      <feGaussianBlur stdDeviation="{BACK_BLUR*n:.1f}"/>
    </filter>
    <filter id="lip" x="-200%" y="-40%" width="500%" height="180%">
      <feGaussianBlur stdDeviation="{LIP_BLUR*n:.1f}"/>
    </filter>
    <mask id="cut">
      <rect {box(plate, PLATE_R)} fill="#fff"/>
      <rect {box(slot, SLOT_W/2)} fill="#000"/>
    </mask>
    <clipPath id="edge"><rect width="{n}" height="{n}" rx="{n*0.223:.0f}"/></clipPath>
  </defs>
  <rect width="{n}" height="{n}" rx="{n*0.223:.0f}" fill="url(#tile)"/>
  <g clip-path="url(#edge)">
    <rect {box(slot, SLOT_W/2)} fill="{hexed(BLOOM)}" opacity="{BACK_ALPHA}" filter="url(#back)"/>
    <rect {box(plate, PLATE_R)} fill="url(#plate)" mask="url(#cut)"/>
    <rect {box(slot, SLOT_W/2)} fill="{hexed(BLOOM)}" opacity="{LIP_ALPHA}" filter="url(#lip)"/>
    <rect {box(slot, SLOT_W/2)} fill="url(#along)"/>
  </g>
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
