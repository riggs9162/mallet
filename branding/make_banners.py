"""Draws Mallet's banners: the icon and name on a Hammer-style 2D grid, in several aspect ratios.

Every banner is laid out from one set of shapes, written once as an SVG and once as a PNG.
The icon comes from make_icons.py, so the two always match. The name and tagline are drawn as
outlines from Inter, so the SVGs look the same on machines without the font. Run from anywhere:

    python branding/make_banners.py

Writes banners/mallet-banner-<ratio>.svg and .png next to this file. Needs fontTools,
matplotlib, numpy and the Inter variable font (https://rsms.me/inter/) installed, or its path
in the MALLET_BANNER_FONT environment variable. make_assets.py builds the other brand assets
from the same drawing code.
"""

import math
import os
import sys
from pathlib import Path

import matplotlib

matplotlib.use("Agg")

import matplotlib.pyplot as plt
import numpy as np
from fontTools.pens.basePen import BasePen
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont
from matplotlib.patches import PathPatch
from matplotlib.path import Path as MplPath
from PIL import Image

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from make_icons import SHAPES, TILT, glyph_frame  # noqa: E402

OUT = HERE / "banners"

# (file name suffix, width, height). Common uses: 16:9 for video and Discord server banners,
# 21:9 for ultrawide headers, 2:1 for the GitHub social preview, 5:2 for Twitch, 17:6 for
# Discord profile banners, 3:1 for X headers and 4:1 for LinkedIn and forum headers.
BANNERS = [
    ("16x9", 1920, 1080),
    ("21x9", 2520, 1080),
    ("2x1", 1280, 640),
    ("5x2", 1500, 600),
    ("17x6", 1360, 480),
    ("3x1", 1500, 500),
    ("4x1", 1584, 396),
]
# Banners narrower than this stack the name under the icon; wider ones set it beside the icon.
STACK_BELOW_RATIO = 1.9

NAME = "Mallet"
TAGLINE = "A Hammer-style level editor for Source engine maps"
# The tagline broken in two, for portrait images where one line would be too small to read.
TAGLINE_LINES = ("A Hammer-style level editor", "for Source engine maps")

BACKGROUND = "#0e0e0e"
MINOR_GRID = "#1e1e1e"
MAJOR_GRID = "#2f2f2f"
SELECTION = "#6a6a6a"
HANDLE = "#e8e8e8"
CIRCLE = "#000000"
GLYPH = "#ffffff"
NAME_COLOUR = "#ffffff"
TAGLINE_COLOUR = "#8e8e8e"

# Grid cells per banner height, and how many cells sit between the brighter major lines,
# like Hammer's "highlight every 64 units" at a grid of 16.
GRID_CELLS_PER_HEIGHT = 20
MAJOR_EVERY = 4
# The grid fades into the background towards the edges: (distance from the centre, where 1 is
# the middle of an edge, opacity of the background drawn over the grid).
FADE_STOPS = [(0.0, 0.0), (0.45, 0.15), (1.0, 0.9)]

# Sizes in icon diameters. A row sets the text beside the icon, a stack sets it below.
# `leading` is the distance between tagline lines, as a share of the tagline's size.
ROW = {"gap": 0.26, "name": 0.66, "tagline": 0.125, "line_gap": 0.2, "leading": 1.45, "max_w": 0.76, "max_h": 0.46}
STACK = {"gap": 0.22, "name": 0.5, "tagline": 0.105, "line_gap": 0.16, "leading": 1.45, "max_w": 0.76, "max_h": 0.62}
# Letter spacing in ems: display sizes of Inter read better a little tighter.
NAME_TRACKING = -0.02
TAGLINE_TRACKING = 0.0
# The dashed selection box drawn around the logo, in icon diameters, like a selected brush in
# one of Hammer's 2D views.
SELECTION_PADDING = 0.16
HANDLE_SIZE = 0.05

FONT_FILES = ["Inter-VariableFont_opsz,wght.ttf", "InterVariable.ttf", "Inter.ttf"]
KAPPA = 0.5522847498


class OutlinePen(BasePen):
    """Collects a glyph's outline as path operations, shifted right by `offset` font units."""

    def __init__(self, glyph_set, offset):
        super().__init__(glyph_set)
        self.offset = offset
        self.ops = []

    def _point(self, point):
        return (point[0] + self.offset, point[1])

    def _moveTo(self, point):
        self.ops.append(("M", self._point(point)))

    def _lineTo(self, point):
        self.ops.append(("L", self._point(point)))

    def _qCurveToOne(self, control, point):
        self.ops.append(("Q", self._point(control), self._point(point)))

    def _curveToOne(self, control1, control2, point):
        self.ops.append(("C", self._point(control1), self._point(control2), self._point(point)))

    def _closePath(self):
        self.ops.append(("Z",))

    def _endPath(self):
        self.ops.append(("Z",))


def find_font():
    """The path of the Inter variable font, from MALLET_BANNER_FONT or the usual font folders."""
    override = os.environ.get("MALLET_BANNER_FONT")
    if override:
        return Path(override)
    folders = [
        Path(os.environ.get("LOCALAPPDATA", "")) / "Microsoft" / "Windows" / "Fonts",
        Path(os.environ.get("WINDIR", "C:/Windows")) / "Fonts",
        Path.home() / ".local" / "share" / "fonts",
        Path.home() / "Library" / "Fonts",
        Path("/usr/share/fonts/truetype/inter"),
        Path("/usr/share/fonts/inter"),
    ]
    for folder in folders:
        for name in FONT_FILES:
            if (folder / name).is_file():
                return folder / name
    sys.exit("Inter is not installed. Install it or set MALLET_BANNER_FONT to its .ttf file.")


def load_font(path, weight, optical_size):
    """A static instance of the variable font at the given weight and optical size."""
    return instantiateVariableFont(TTFont(path), {"wght": weight, "opsz": optical_size})


def load_fonts():
    """The bold name font and the regular tagline font."""
    path = find_font()
    return load_font(path, 700, 32), load_font(path, 400, 14)


def map_ops(ops, transform):
    """`ops` with every point passed through `transform`."""
    return [(op[0], *(transform(point) for point in op[1:])) for op in ops]


def place_shapes(shapes, scale, left, top):
    """`shapes` scaled by `scale` and then moved by (`left`, `top`)."""
    return [(map_ops(ops, lambda p: (left + p[0] * scale, top + p[1] * scale)), colour) for ops, colour in shapes]


def rect_ops(x0, y0, x1, y1):
    """A rectangle as path operations, wound the same way as every other rectangle."""
    return [("M", (x0, y0)), ("L", (x1, y0)), ("L", (x1, y1)), ("L", (x0, y1)), ("Z",)]


def rounded_rect_ops(x0, y0, x1, y1, radius):
    """A rectangle with rounded corners, drawn with cubic curves."""
    radius = min(radius, (x1 - x0) / 2, (y1 - y0) / 2)
    k = radius * (1 - KAPPA)
    return [
        ("M", (x0 + radius, y0)),
        ("L", (x1 - radius, y0)),
        ("C", (x1 - k, y0), (x1, y0 + k), (x1, y0 + radius)),
        ("L", (x1, y1 - radius)),
        ("C", (x1, y1 - k), (x1 - k, y1), (x1 - radius, y1)),
        ("L", (x0 + radius, y1)),
        ("C", (x0 + k, y1), (x0, y1 - k), (x0, y1 - radius)),
        ("L", (x0, y0 + radius)),
        ("C", (x0, y0 + k), (x0 + k, y0), (x0 + radius, y0)),
        ("Z",),
    ]


def circle_ops(cx, cy, r):
    """A circle drawn with four cubic curves."""
    k = r * KAPPA
    return [
        ("M", (cx, cy - r)),
        ("C", (cx + k, cy - r), (cx + r, cy - k), (cx + r, cy)),
        ("C", (cx + r, cy + k), (cx + k, cy + r), (cx, cy + r)),
        ("C", (cx - k, cy + r), (cx - r, cy + k), (cx - r, cy)),
        ("C", (cx - r, cy - k), (cx - k, cy - r), (cx, cy - r)),
        ("Z",),
    ]


def glyph_ops():
    """The tilted mallet glyph alone, fitted to a unit square the way the icon fits it."""
    scale, centre_x, centre_y = glyph_frame()
    angle = math.radians(TILT)
    cos, sin = math.cos(angle), math.sin(angle)

    def place(point):
        x, y = point[0] - 128, point[1] - 128
        return ((128 + x * cos - y * sin) / 256, (128 + x * sin + y * cos) / 256)

    glyph = []
    for x0, y0, x1, y1, radius in SHAPES:
        glyph += rounded_rect_ops(
            (x0 - centre_x) * scale + 128,
            (y0 - centre_y) * scale + 128,
            (x1 - centre_x) * scale + 128,
            (y1 - centre_y) * scale + 128,
            radius * scale,
        )
    return map_ops(glyph, place)


def icon_ops(backing=CIRCLE, glyph=GLYPH, shape="circle"):
    """The icon in a unit square, as (operations, colour) pairs.

    `shape` is "circle" for the usual disc, "square" for a rounded square, or None for the
    glyph alone.
    """
    shapes = []
    if shape == "circle":
        shapes.append((circle_ops(0.5, 0.5, 0.5), backing))
    elif shape == "square":
        shapes.append((rounded_rect_ops(0, 0, 1, 1, 0.225), backing))
    shapes.append((glyph_ops(), glyph))
    return shapes


def text_ops(font, text, size, tracking):
    """`text` set at `size` with its baseline at y = 0, and the width it takes up."""
    glyph_set = font.getGlyphSet()
    cmap = font.getBestCmap()
    units = font["head"].unitsPerEm
    ops = []
    advance = 0
    for index, char in enumerate(text):
        glyph = glyph_set[cmap[ord(char)]]
        pen = OutlinePen(glyph_set, advance)
        glyph.draw(pen)
        ops += pen.ops
        advance += glyph.width
        if index < len(text) - 1:
            advance += tracking * units
    scale = size / units
    return map_ops(ops, lambda point: (point[0] * scale, -point[1] * scale)), advance * scale


def cap_height(font, size):
    """The height of a capital letter at `size`."""
    return font["OS/2"].sCapHeight * size / font["head"].unitsPerEm


def lockup(
    name_font,
    tagline_font,
    stacked,
    tagline_lines=(TAGLINE,),
    icon=True,
    name=True,
    name_colour=NAME_COLOUR,
    tagline_colour=TAGLINE_COLOUR,
    icon_shapes=None,
):
    """The icon, name and tagline with a unit icon, plus the size they fill: (shapes, (w, h)).

    Turn `icon` or `name` off, or pass no `tagline_lines`, to leave that part out.
    `icon_shapes` replaces the usual icon.
    """
    sizes = STACK if stacked else ROW
    shapes = (icon_shapes or icon_ops()) if icon else []
    if not name:
        return shapes, (1, 1)

    name_ops, name_width = text_ops(name_font, NAME, sizes["name"], NAME_TRACKING)
    taglines = [text_ops(tagline_font, line, sizes["tagline"], TAGLINE_TRACKING) for line in tagline_lines]
    name_cap = cap_height(name_font, sizes["name"])
    tagline_cap = cap_height(tagline_font, sizes["tagline"])
    leading = sizes["tagline"] * sizes["leading"]
    tagline_height = sizes["line_gap"] + tagline_cap + leading * (len(taglines) - 1) if taglines else 0
    text_width = max([name_width] + [width for _, width in taglines])

    if not icon:
        text_left, name_base, width = 0, name_cap, text_width
    elif stacked:
        width = max(1, text_width)
        shapes = place_shapes(shapes, 1, (width - 1) / 2, 0)
        text_left = (width - text_width) / 2
        name_base = 1 + sizes["gap"] + name_cap
    else:
        text_left = 1 + sizes["gap"]
        width = text_left + text_width
        name_base = 0.5 - (name_cap + tagline_height) / 2 + name_cap

    def line_left(line_width):
        return text_left + (text_width - line_width) / 2 if stacked else text_left

    shapes.append((map_ops(name_ops, lambda p: (p[0] + line_left(name_width), p[1] + name_base)), name_colour))
    base = name_base + sizes["line_gap"] + tagline_cap
    for ops, line_width in taglines:
        left = line_left(line_width)
        shapes.append((map_ops(ops, lambda p, left=left, base=base: (p[0] + left, p[1] + base)), tagline_colour))
        base += leading

    bottom = name_base + tagline_height
    height = max(bottom, 1) if icon else bottom
    return shapes, (width, height)


def grid_ops(width, height):
    """The minor and major grid lines as thin bars centred on the image."""
    cell = max(8, round(height / GRID_CELLS_PER_HEIGHT))
    thickness = max(1, round(min(width, height) / 1440))
    centre_x, centre_y = width // 2, height // 2
    minor, major = [], []
    for centre, extent, vertical in ((centre_x, width, True), (centre_y, height, False)):
        steps = extent // (2 * cell) + 1
        for step in range(-steps, steps + 1):
            at = centre + step * cell
            if not 0 <= at < extent:
                continue
            bar = rect_ops(at, 0, at + thickness, height) if vertical else rect_ops(0, at, width, at + thickness)
            (major if step % MAJOR_EVERY == 0 else minor).extend(bar)
    return [(minor, MINOR_GRID), (major, MAJOR_GRID)]


def selection_ops(box, icon_size, height):
    """A dashed box around `box` (x0, y0, x1, y1) with Hammer's eight square resize handles."""
    thickness = max(1, round(height / 1080))
    dash, gap = 7 * thickness, 5 * thickness
    pad = round(SELECTION_PADDING * icon_size)
    x0, y0 = math.floor(box[0]) - pad, math.floor(box[1]) - pad
    x1, y1 = math.ceil(box[2]) + pad, math.ceil(box[3]) + pad
    dashes = []
    for start in range(x0, x1, dash + gap):
        end = min(start + dash, x1)
        dashes += rect_ops(start, y0, end, y0 + thickness) + rect_ops(start, y1 - thickness, end, y1)
    for start in range(y0, y1, dash + gap):
        end = min(start + dash, y1)
        dashes += rect_ops(x0, start, x0 + thickness, end) + rect_ops(x1 - thickness, start, x1, end)

    size = max(7, round(HANDLE_SIZE * icon_size))
    handles = []
    for hx in (x0, (x0 + x1) // 2, x1):
        for hy in (y0, (y0 + y1) // 2, y1):
            if hx == (x0 + x1) // 2 and hy == (y0 + y1) // 2:
                continue
            left, top = hx - size // 2, hy - size // 2
            handles += rect_ops(left, top, left + size, top + size)
    return [(dashes, SELECTION), (handles, HANDLE)]


def layout(width, height, name_font, tagline_font, stacked=None, fill=1.0, selection=True, **parts):
    """Everything drawn above the fade, in pixels: a list of (operations, colour).

    `stacked` defaults to stacking on images narrower than STACK_BELOW_RATIO. `fill` shrinks
    the logo below its usual share of the image. `parts` go to lockup().
    """
    if stacked is None:
        stacked = width / height < STACK_BELOW_RATIO
    sizes = STACK if stacked else ROW
    shapes, (unit_width, unit_height) = lockup(name_font, tagline_font, stacked, **parts)
    icon = fill * min(sizes["max_w"] * width / unit_width, sizes["max_h"] * height / unit_height)
    left = (width - unit_width * icon) / 2
    top = (height - unit_height * icon) / 2
    placed = place_shapes(shapes, icon, left, top)
    if not selection:
        return placed
    box = (left, top, left + unit_width * icon, top + unit_height * icon)
    return selection_ops(box, icon, height) + placed


def number(value):
    """A coordinate written as briefly as SVG allows."""
    text = f"{value:.2f}".rstrip("0").rstrip(".")
    return "0" if text == "-0" else text


def svg_path(ops):
    """Path operations as SVG path data."""
    parts = []
    for op in ops:
        points = " ".join(f"{number(x)} {number(y)}" for x, y in op[1:])
        parts.append(f"{op[0]}{points}")
    return "".join(parts)


def write_svg(path, width, height, below, above, background=BACKGROUND, fade=True):
    """Writes an image as an SVG: the background, the shapes `below`, the fade, then `above`.

    With `background` None the image is transparent; with `fade` off nothing is drawn between
    the two layers.
    """
    lines = []
    if fade:
        stops = "\n".join(
            f'      <stop offset="{offset}" stop-color="{background}" stop-opacity="{opacity}"/>'
            for offset, opacity in FADE_STOPS
        )
        lines += ["  <defs>", '    <radialGradient id="fade" cx="0.5" cy="0.5" r="0.5">', stops, "    </radialGradient>", "  </defs>"]
    if background:
        lines.append(f'  <rect width="{width}" height="{height}" fill="{background}"/>')
    lines += [f'  <path fill="{colour}" d="{svg_path(ops)}"/>' for ops, colour in below if ops]
    if fade:
        lines.append(f'  <rect width="{width}" height="{height}" fill="url(#fade)"/>')
    lines += [f'  <path fill="{colour}" d="{svg_path(ops)}"/>' for ops, colour in above if ops]
    path.write_text(
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {number(width)} {number(height)}" '
        f'width="{number(width)}" height="{number(height)}">\n'
        "  <title>Mallet</title>\n" + "\n".join(lines) + "\n</svg>\n",
        encoding="utf-8",
    )


def mpl_path(ops):
    """Path operations as a matplotlib path."""
    codes = {"M": [MplPath.MOVETO], "L": [MplPath.LINETO], "Q": [MplPath.CURVE3] * 2, "C": [MplPath.CURVE4] * 3}
    vertices, kinds = [], []
    start = (0, 0)
    for op in ops:
        if op[0] == "Z":
            vertices.append(start)
            kinds.append(MplPath.CLOSEPOLY)
            continue
        if op[0] == "M":
            start = op[1]
        vertices.extend(op[1:])
        kinds.extend(codes[op[0]])
    return MplPath(vertices, kinds)


def hex_rgb(colour):
    """A "#rrggbb" colour as three floats from 0 to 1."""
    return [int(colour[i : i + 2], 16) / 255 for i in (1, 3, 5)]


def fade_image(width, height, background):
    """The background colour with the fade's opacity, matching the SVG's radial gradient."""
    ys, xs = np.mgrid[0:height, 0:width]
    distance = np.hypot((xs + 0.5 - width / 2) / (width / 2), (ys + 0.5 - height / 2) / (height / 2))
    alpha = np.interp(distance, [s[0] for s in FADE_STOPS], [s[1] for s in FADE_STOPS])
    image = np.empty((height, width, 4))
    image[..., :3] = hex_rgb(background)
    image[..., 3] = alpha
    return image


def render(width, height, below, above, background=BACKGROUND, fade=True, scale=1):
    """Draws the same layers as write_svg() into a Pillow image `scale` times as large."""
    pixel_width, pixel_height = round(width * scale), round(height * scale)
    face = (*hex_rgb(background), 1) if background else (0, 0, 0, 0)
    figure = plt.figure(figsize=(pixel_width / 100, pixel_height / 100), dpi=100, facecolor=face)
    axes = figure.add_axes((0, 0, 1, 1))
    axes.set_xlim(0, width)
    axes.set_ylim(height, 0)
    axes.set_axis_off()
    axes.patch.set_alpha(0)
    order = 0
    for ops, colour in below:
        if ops:
            axes.add_patch(PathPatch(mpl_path(ops), facecolor=colour, edgecolor="none", lw=0, zorder=order))
            order += 1
    if fade:
        image = fade_image(pixel_width, pixel_height, background)
        axes.imshow(image, extent=(0, width, height, 0), interpolation="nearest", zorder=order)
        axes.set_xlim(0, width)
        axes.set_ylim(height, 0)
        order += 1
    for ops, colour in above:
        if ops:
            axes.add_patch(PathPatch(mpl_path(ops), facecolor=colour, edgecolor="none", lw=0, zorder=order))
            order += 1
    figure.canvas.draw()
    image = Image.frombuffer("RGBA", figure.canvas.get_width_height(), figure.canvas.buffer_rgba()).copy()
    plt.close(figure)
    if image.size != (pixel_width, pixel_height):
        image = image.resize((pixel_width, pixel_height), Image.LANCZOS)
    return image.convert("RGB") if background else image


def write_png(path, width, height, below, above, background=BACKGROUND, fade=True, scale=1):
    """Writes an image as a PNG drawn from the same layers as write_svg()."""
    render(width, height, below, above, background, fade, scale).save(path, optimize=True)


def main():
    name_font, tagline_font = load_fonts()
    OUT.mkdir(exist_ok=True)
    for ratio, width, height in BANNERS:
        below = grid_ops(width, height)
        above = layout(width, height, name_font, tagline_font)
        write_svg(OUT / f"mallet-banner-{ratio}.svg", width, height, below, above)
        write_png(OUT / f"mallet-banner-{ratio}.png", width, height, below, above)
        print(f"wrote mallet-banner-{ratio} ({width} x {height})")


if __name__ == "__main__":
    main()
