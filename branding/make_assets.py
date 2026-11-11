"""Draws Mallet's brand assets other than the icon and the banners.

Uses the drawing code of make_banners.py, so everything matches the icon and banners. Run
from anywhere:

    python branding/make_assets.py

Writes, next to this file:

- logos/: the icon with the name beside or below it, the tagline lockup and the name alone,
  each on a transparent background in a version for dark and one for light backgrounds.
- icons/: the inverted icon, rounded-square app icons and the bare glyph in white and black.
- social/: a profile picture, an Open Graph image, a portrait story image and wallpapers.
- badges/: a "made with Mallet" badge and an 88 x 31 web button.

Every asset is an SVG and a PNG. Needs the same packages and font as make_banners.py.
"""

import math

from make_banners import (
    BACKGROUND,
    CIRCLE,
    GLYPH,
    HERE,
    TAGLINE_LINES,
    cap_height,
    glyph_ops,
    grid_ops,
    icon_ops,
    layout,
    load_fonts,
    lockup,
    place_shapes,
    rect_ops,
    rounded_rect_ops,
    text_ops,
    write_png,
    write_svg,
)

# Name and tagline colours on dark and on light backgrounds.
ON_DARK = {"name_colour": "#ffffff", "tagline_colour": "#8e8e8e"}
ON_LIGHT = {"name_colour": "#000000", "tagline_colour": "#5c5c5c"}
# Logos are drawn with a 256-unit icon, with this much clear space around them in icon
# diameters, and their PNGs at twice that size.
LOGO_ICON = 256
LOGO_MARGIN = 0.12
LOGO_PNG_SCALE = 2
ICON_SIZE = 1024
# Clear space around the bare glyph, as a share of its image.
GLYPH_MARGIN = 0.04

# (file name, width, height, layout options) for the images on the grid background.
SOCIAL = [
    ("mallet-avatar", 1024, 1024, {"stacked": True, "name": False, "selection": False, "fill": 1.25}),
    ("mallet-og-image", 1200, 630, {}),
    ("mallet-story", 1080, 1920, {"stacked": True, "tagline_lines": TAGLINE_LINES, "fill": 0.84}),
    ("mallet-wallpaper-1440p", 2560, 1440, {"tagline_lines": (), "fill": 0.6}),
    ("mallet-wallpaper-4k", 3840, 2160, {"tagline_lines": (), "fill": 0.6}),
    ("mallet-wallpaper-phone", 1170, 2532, {"stacked": True, "tagline_lines": TAGLINE_LINES, "fill": 0.8}),
]

BADGE_LABEL = "made with"
BADGE_LABEL_COLOUR = "#3d3d3d"
BADGE_VALUE_COLOUR = "#000000"
BADGE_TEXT = "#ffffff"
BUTTON_BORDER = "#3a3a3a"
BUTTON_GRID = "#1a1a1a"
BUTTON_LABEL = "#8e8e8e"


def save(folder, stem, width, height, below, above, background=None, fade=False, png_scale=1):
    """Writes `stem`.svg and `stem`.png into `folder` and reports them."""
    folder.mkdir(exist_ok=True)
    write_svg(folder / f"{stem}.svg", width, height, below, above, background, fade)
    write_png(folder / f"{stem}.png", width, height, below, above, background, fade, png_scale)
    print(f"wrote {folder.name}/{stem} ({round(width * png_scale)} x {round(height * png_scale)})")


def make_logos(name_font, tagline_font):
    """The logo lockups on transparent backgrounds, for dark and for light backgrounds."""
    kinds = {
        "horizontal": {"stacked": False, "tagline_lines": ()},
        "horizontal-tagline": {"stacked": False},
        "stacked": {"stacked": True, "tagline_lines": ()},
        "stacked-tagline": {"stacked": True, "tagline_lines": TAGLINE_LINES},
        "wordmark": {"stacked": False, "tagline_lines": (), "icon": False},
    }
    for kind, parts in kinds.items():
        for background, colours in (("on-dark", ON_DARK), ("on-light", ON_LIGHT)):
            shapes, (width, height) = lockup(name_font, tagline_font, **parts, **colours)
            margin = LOGO_MARGIN * LOGO_ICON
            placed = place_shapes(shapes, LOGO_ICON, margin, margin)
            size = (math.ceil(width * LOGO_ICON + 2 * margin), math.ceil(height * LOGO_ICON + 2 * margin))
            save(HERE / "logos", f"mallet-logo-{kind}-{background}", *size, [], placed, png_scale=LOGO_PNG_SCALE)


def make_icons():
    """Icon variants: inverted, rounded square, and the glyph alone in white and in black."""
    variants = {
        "mallet-icon-inverted": icon_ops(backing=GLYPH, glyph=CIRCLE),
        "mallet-icon-square": icon_ops(shape="square"),
        "mallet-icon-square-inverted": icon_ops(backing=GLYPH, glyph=CIRCLE, shape="square"),
    }
    for stem, shapes in variants.items():
        save(HERE / "icons", stem, ICON_SIZE, ICON_SIZE, [], place_shapes(shapes, ICON_SIZE, 0, 0))
    for colour_name, colour in (("white", "#ffffff"), ("black", "#000000")):
        save(HERE / "icons", f"mallet-glyph-{colour_name}", ICON_SIZE, ICON_SIZE, [], fitted_glyph(colour))


def fitted_glyph(colour):
    """The glyph alone, scaled up to fill the icon's square with a small margin."""
    points = [point for op in glyph_ops() for point in op[1:]]
    x0, x1 = min(p[0] for p in points), max(p[0] for p in points)
    y0, y1 = min(p[1] for p in points), max(p[1] for p in points)
    scale = ICON_SIZE * (1 - 2 * GLYPH_MARGIN) / max(x1 - x0, y1 - y0)
    left = (ICON_SIZE - (x1 - x0) * scale) / 2 - x0 * scale
    top = (ICON_SIZE - (y1 - y0) * scale) / 2 - y0 * scale
    return place_shapes([(glyph_ops(), colour)], scale, left, top)


def make_social(name_font, tagline_font):
    """The profile picture, Open Graph image, story image and wallpapers on the grid."""
    for stem, width, height, options in SOCIAL:
        below = grid_ops(width, height)
        above = layout(width, height, name_font, tagline_font, **options)
        save(HERE / "social", stem, width, height, below, above, BACKGROUND, fade=True)


def make_badge(name_font, tagline_font):
    """A two-part "made with | Mallet" badge in the 20-pixel style of shields.io."""
    height, size, pad, radius, icon = 20, 11, 6, 3, 13
    label, label_width = text_ops(tagline_font, BADGE_LABEL, size, 0)
    value, value_width = text_ops(name_font, "Mallet", size, 0)
    base = (height + cap_height(name_font, size)) / 2
    split = label_width + 2 * pad
    width = split + pad + icon + 4 + value_width + pad
    below = [
        (rounded_rect_ops(0, 0, width, height, radius), BADGE_LABEL_COLOUR),
        (rounded_rect_ops(split, 0, width, height, radius), BADGE_VALUE_COLOUR),
        (rect_ops(split, 0, split + radius, height), BADGE_VALUE_COLOUR),
    ]
    glyph_left = split + pad - 1
    above = place_shapes([(glyph_ops(), BADGE_TEXT)], icon + 2, glyph_left, (height - icon - 2) / 2)
    above += place_shapes([(label, BADGE_TEXT)], 1, pad, base)
    above += place_shapes([(value, BADGE_TEXT)], 1, split + pad + icon + 4, base)
    save(HERE / "badges", "made-with-mallet", width, height, below, above, png_scale=4)


def make_button(name_font, tagline_font):
    """An 88 x 31 web button in the old style, with the glyph on a patch of grid."""
    width, height = 88, 31
    grid = [(ops, BUTTON_GRID) for ops, _ in grid_ops(width, height)]
    below = [(rect_ops(0, 0, width, height), BUTTON_BORDER), (rect_ops(1, 1, width - 1, height - 1), BACKGROUND)]
    below += place_shapes(grid, 1, 0, 0)
    below.append((rect_ops(0, 0, width, 1) + rect_ops(0, height - 1, width, height), BUTTON_BORDER))
    below.append((rect_ops(0, 0, 1, height) + rect_ops(width - 1, 0, width, height), BUTTON_BORDER))
    label, _ = text_ops(tagline_font, BADGE_LABEL, 7.5, 0.01)
    name, _ = text_ops(name_font, "Mallet", 14, -0.01)
    above = place_shapes([(glyph_ops(), GLYPH)], 27, 2, 2)
    above += place_shapes([(label, BUTTON_LABEL)], 1, 32, 11.5)
    above += place_shapes([(name, GLYPH)], 1, 31.5, 25)
    for scale, suffix in ((1, ""), (2, "@2x"), (4, "@4x")):
        stem = f"mallet-88x31{suffix}"
        if scale == 1:
            save(HERE / "badges", stem, width, height, below, above, BACKGROUND)
        else:
            write_png(HERE / "badges" / f"{stem}.png", width, height, below, above, BACKGROUND, False, scale)
            print(f"wrote badges/{stem}.png ({width * scale} x {height * scale})")


def main():
    name_font, tagline_font = load_fonts()
    make_logos(name_font, tagline_font)
    make_icons()
    make_social(name_font, tagline_font)
    make_badge(name_font, tagline_font)
    make_button(name_font, tagline_font)


if __name__ == "__main__":
    main()
