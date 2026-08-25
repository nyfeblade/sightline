#!/usr/bin/env python3
"""Draw Sightline's icon, at every size the platforms ask for.

    python3 scripts/make-icon.py

A slab receding, with a lit seam running along it into the distance.

The name is the brief. A sightline is the clear line you see along, and what
makes this an icon rather than a diagram is that the line has somewhere to go:
the seam narrows to a point, and the light arrives from there.

Three earlier marks failed, twice in the same way, which is worth recording so
it is not tried a fourth time. A crosshair of dots — a weapon sight, which is
what this used to be called, in a shape that belongs to any launcher. Then a
line with traces crossing it, which was *true* to the architecture and still
read as the universal sliders-and-filters glyph. Then a slot cut in a plate: an
object at last, but a static one, and small.

Being an accurate picture of the architecture is not what makes an icon. It
wants a subject, a light source and depth — and it has to survive being thirty
two pixels wide, which is where the pretty ones die.

Everything below is a fraction of the tile, so the mark scales exactly and the
raster and the vector come from one set of numbers. Drawn at three times the
final size and reduced, which is how the gradients and the glow come out clean
without a vector renderer on the machine.
"""

from PIL import Image, ImageDraw, ImageFilter

FINAL = 1024
S = FINAL * 3

TILE_TOP = (30, 30, 37)
TILE_BOTTOM = (8, 8, 10)

FACE_TOP = (96, 96, 110)      # the slab's upper surface, near
FACE_BOTTOM = (40, 40, 49)    # and where it recedes
END_TOP = (58, 58, 68)        # its near end, so it has thickness
END_BOTTOM = (24, 24, 30)
EDGE_TOP = (22, 22, 28)       # the chamfer down each long side
EDGE_BOTTOM = (12, 12, 16)
SEAM = (255, 255, 255)
BLOOM = (210, 226, 255)

VANISH = 0.365       # where the seam arrives
NEAR_Y = 0.845       # the near end of the slab
NEAR_HALF = 0.215    # its half-width there
FAR_HALF = 0.0295    # and at the far end
THICK = 0.052        # the visible end face
SEAM_NEAR, SEAM_FAR = 0.026, 0.0022

NEAR_BLUR, MID_BLUR = 0.048, 0.009
POINT_BLUR = 0.038


def faces():
    """Every surface of the mark, back to front, as (points, top, bottom).

    One list, used for the PNG and the SVG. Hand-writing the vector separately
    is how the two drift apart, and then only one of them is the icon anybody
    actually sees.
    """
    n, f, v, y = NEAR_HALF, FAR_HALF, VANISH, NEAR_Y
    out = [
        ([(0.5 - n, y), (0.5 + n, y), (0.5 + f, v), (0.5 - f, v)], FACE_TOP, FACE_BOTTOM),
        (
            [(0.5 - n, y), (0.5 + n, y), (0.5 + n, y + THICK), (0.5 - n, y + THICK)],
            END_TOP,
            END_BOTTOM,
        ),
    ]
    # A darker chamfer down each long edge, so the surface separates from the
    # tile without an outline drawn round it.
    for side in (-1, 1):
        out.append((
            [
                (0.5 + side * n, y),
                (0.5 + side * f, v),
                (0.5 + side * (f + 0.012), v),
                (0.5 + side * (n + 0.030), y + THICK * 0.5),
            ],
            EDGE_TOP,
            EDGE_BOTTOM,
        ))
    return out


def seam_points():
    return [
        (0.5 - SEAM_NEAR, NEAR_Y),
        (0.5 + SEAM_NEAR, NEAR_Y),
        (0.5 + SEAM_FAR, VANISH),
        (0.5 - SEAM_FAR, VANISH),
    ]


def ramp(top, bottom):
    strip = Image.new("RGB", (1, 256))
    for y in range(256):
        t = y / 255
        strip.putpixel((0, y), tuple(round(a + (b - a) * t) for a, b in zip(top, bottom)))
    return strip.resize((S, S), Image.BICUBIC).convert("RGBA")


def tile():
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    corners = Image.new("L", (S, S), 0)
    # 22.3% is the macOS rounded rectangle, near enough that this sits in a dock
    # without looking like it came from somewhere else.
    ImageDraw.Draw(corners).rounded_rectangle(
        [0, 0, S - 1, S - 1], radius=int(S * 0.223), fill=255
    )
    img.paste(ramp(TILE_TOP, TILE_BOTTOM), (0, 0), corners)
    return img


def mark(img):
    for points, top, bottom in faces():
        mask = Image.new("L", (S, S), 0)
        ImageDraw.Draw(mask).polygon([(x * S, y * S) for x, y in points], fill=255)
        layer = Image.new("RGBA", (S, S), (0, 0, 0, 0))
        layer.paste(ramp(top, bottom), (0, 0), mask)
        img.alpha_composite(layer)

    # The seam, three times: a wide throw, a tight one, and the hard core. Light
    # built from blurred copies of the shape emitting it rather than a gradient
    # behind it — the falloff then has the shape of the source.
    seam = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(seam).polygon(
        [(x * S, y * S) for x, y in seam_points()], fill=SEAM + (255,)
    )
    img.alpha_composite(seam.filter(ImageFilter.GaussianBlur(NEAR_BLUR * S)))
    img.alpha_composite(seam.filter(ImageFilter.GaussianBlur(MID_BLUR * S)))
    img.alpha_composite(seam)

    point = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(point).ellipse(
        [(0.5 - 0.048) * S, (VANISH - 0.038) * S, (0.5 + 0.048) * S, (VANISH + 0.038) * S],
        fill=BLOOM + (225,),
    )
    img.alpha_composite(point.filter(ImageFilter.GaussianBlur(POINT_BLUR * S)))
    return img


def svg(path):
    """The same mark as vector, from the same numbers, so the two cannot drift."""
    n = 512
    hexed = lambda c: "#%02x%02x%02x" % c
    defs, shapes = [], []
    for i, (points, top, bottom) in enumerate(faces()):
        ys = [p[1] for p in points]
        defs.append(
            '<linearGradient id="g%d" gradientUnits="userSpaceOnUse" '
            'x1="0" y1="%.1f" x2="0" y2="%.1f">'
            '<stop offset="0" stop-color="%s"/>'
            '<stop offset="1" stop-color="%s"/></linearGradient>'
            % (i, min(ys) * n, max(ys) * n, hexed(top), hexed(bottom))
        )
        pts = " ".join("%.1f,%.1f" % (x * n, y * n) for x, y in points)
        shapes.append('<polygon points="%s" fill="url(#g%d)"/>' % (pts, i))
    seam = " ".join("%.1f,%.1f" % (x * n, y * n) for x, y in seam_points())
    open(path, "w").write(
        '<!-- Sightline. Generated by scripts/make-icon.py; edit that, not this. -->\n'
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 %d %d" width="%d" height="%d">\n'
        '  <defs>\n'
        '    <linearGradient id="tile" x1="0" y1="0" x2="0" y2="1">\n'
        '      <stop offset="0" stop-color="%s"/><stop offset="1" stop-color="%s"/>\n'
        '    </linearGradient>\n'
        '    %s\n'
        '    <radialGradient id="point">\n'
        '      <stop offset="0" stop-color="%s" stop-opacity=".88"/>\n'
        '      <stop offset="1" stop-color="%s" stop-opacity="0"/>\n'
        '    </radialGradient>\n'
        '    <filter id="throw" x="-150%%" y="-80%%" width="400%%" height="260%%">\n'
        '      <feGaussianBlur stdDeviation="%.1f"/></filter>\n'
        '    <filter id="tight" x="-80%%" y="-40%%" width="260%%" height="180%%">\n'
        '      <feGaussianBlur stdDeviation="%.1f"/></filter>\n'
        '    <clipPath id="edge"><rect width="%d" height="%d" rx="%.0f"/></clipPath>\n'
        '  </defs>\n'
        '  <rect width="%d" height="%d" rx="%.0f" fill="url(#tile)"/>\n'
        '  <g clip-path="url(#edge)">\n'
        '    %s\n'
        '    <polygon points="%s" fill="%s" filter="url(#throw)"/>\n'
        '    <polygon points="%s" fill="%s" filter="url(#tight)"/>\n'
        '    <polygon points="%s" fill="%s"/>\n'
        '    <ellipse cx="%.1f" cy="%.1f" rx="%.1f" ry="%.1f" fill="url(#point)"/>\n'
        '  </g>\n</svg>\n'
        % (n, n, n, n, hexed(TILE_TOP), hexed(TILE_BOTTOM), "".join(defs),
           hexed(BLOOM), hexed(BLOOM), NEAR_BLUR * n, MID_BLUR * n,
           n, n, n * 0.223, n, n, n * 0.223, "".join(shapes),
           seam, hexed(SEAM), seam, hexed(SEAM), seam, hexed(SEAM),
           0.5 * n, VANISH * n, 0.10 * n, 0.085 * n)
    )


def icns(path, img):
    """Write the macOS icon set.

    By hand, because no icns tool is installed on the machine this is built on
    and the alternative was leaving the file stale — which is the icon macOS
    would actually show, so the one platform whose conventions this mark is
    drawn for would be the only one still shipping the old one.
    """
    import struct
    from io import BytesIO

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
    svg(here + "/icon.svg")
    for name, size in [("32x32.png", 32), ("128x128.png", 128),
                       ("128x128@2x.png", 256), ("icon.png", 512)]:
        img.resize((size, size), Image.LANCZOS).save(here + "/" + name)
    img.resize((256, 256), Image.LANCZOS).save(
        here + "/icon.ico", sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)]
    )
    icns(here + "/icon.icns", img)
    img.save(here + "/icon-master.png")
    print("wrote", here)


if __name__ == "__main__":
    build()
