//! Text standing in the map: Hammer++'s `editor_text`, and any class whose FGD entry has
//! the `worldtext()` helper, such as the game's point_worldtext.
//!
//! The text is laid out as the game's `C_PointWorldText::DrawModel` lays it out: the first
//! line's baseline starts at the entity's origin and runs to the entity's right, further
//! lines follow below it one text size apart, and the text reads the right way round for
//! someone looking the way the angles point. Every character is one cell of a picture of the whole typeface, drawn
//! white with a black outline so a tint colours the letters and leaves the outline black.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::LazyLock;

use ab_glyph::{point, Font, FontRef, PxScale};
use eframe::egui::FontDefinitions;
use glam::{DMat3, DVec2, DVec3};
use mallet_content::fgd::EDITOR_TEXT_CLASS;
use mallet_content::vtf::{Level, Mips};
use mallet_content::EntityClass;
use mallet_vmf::{parse_vec3, Entity};

/// The key of the typeface's picture among the 3D view's textures. No material has a name
/// that starts with a colon.
pub const TEXTURE_KEY: &str = ":mallet/world_text";

/// The name egui gives its built-in monospace typeface.
const TYPEFACE: &str = "Hack";
/// The size of an em in the typeface's picture, in pixels.
const EM_PIXELS: f32 = 64.0;
/// How far the black outline reaches from a letter, in pixels of the picture.
const OUTLINE_PIXELS: f32 = 3.0;
/// The clear border of every cell, in pixels: room for the outline and for the blur of
/// the picture's smaller sizes.
const CELL_BORDER: u32 = 5;
/// How many cells the picture has in a row.
const COLUMNS: u32 = 32;
/// The text size an entity without a `textsize` has, as in the game.
const DEFAULT_SIZE: f64 = 10.0;
/// How many spaces a tab is as wide as.
const TAB_WIDTH: usize = 4;
/// The character drawn for one the typeface lacks.
const UNKNOWN: char = '?';
/// Characters drawn besides ASCII, Latin and Cyrillic letters, where the typeface has
/// them.
const EXTRA_CHARACTERS: &str = "\u{2013}\u{2014}\u{2018}\u{2019}\u{201c}\u{201d}\u{2022}\u{2026}\u{20ac}\u{2116}\u{2122}\u{2190}\u{2191}\u{2192}\u{2193}";

/// Text an entity shows in the map.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldText {
    /// The lines, top first.
    pub lines: Vec<String>,
    /// The height of a line in units. Below zero the text is upside down, as in the game.
    pub size: f64,
    pub color: [u8; 3],
    /// Where the first line's baseline starts.
    pub origin: DVec3,
    /// The direction the text runs in.
    pub right: DVec3,
    /// The direction from a line to the one above it.
    pub up: DVec3,
}

/// One character of a [`WorldText`]: the corners of its cell in the map, counter-clockwise
/// from the bottom left as its reader sees them, and where each is in the typeface's
/// picture.
pub struct Glyph {
    pub corners: [DVec3; 4],
    pub uv: [[f32; 2]; 4],
}

impl WorldText {
    /// The text `entity` shows: its `message`, when it is an editor_text or its class has
    /// the `worldtext()` helper. `None` when there is nothing to draw, so the entity keeps
    /// its box and can still be found and clicked.
    pub fn read(entity: &Entity, class: Option<&EntityClass>, origin: DVec3) -> Option<Self> {
        let shows_text = entity.classname().eq_ignore_ascii_case(EDITOR_TEXT_CLASS)
            || class.is_some_and(|class| class.world_text);
        if !shows_text {
            return None;
        }
        TYPEFACE_LAYOUT.as_ref()?;
        let message = entity.get("message")?;
        let lines: Vec<String> = message
            .replace("\\n", "\n")
            .lines()
            .map(|line| line.replace('\t', &" ".repeat(TAB_WIDTH)))
            .collect();
        if lines.iter().all(|line| line.trim().is_empty()) {
            return None;
        }
        let size = match entity.get("textsize").map(|value| value.trim().parse::<f64>()) {
            Some(Ok(size)) => size,
            _ => DEFAULT_SIZE,
        };
        if !size.is_finite() || size == 0.0 {
            return None;
        }
        let mut channels = entity
            .get("color")
            .unwrap_or_default()
            .split_whitespace()
            .map(|channel| channel.parse::<f64>().ok());
        let color = std::array::from_fn(|_| channels.next().flatten().map_or(255, |channel| channel.clamp(0.0, 255.0) as u8));
        let [pitch, yaw, roll] = entity
            .get("angles")
            .and_then(parse_vec3)
            .map_or([0.0; 3], |angles| angles.to_array().map(f64::to_radians));
        let rotation = DMat3::from_rotation_z(yaw) * DMat3::from_rotation_y(pitch) * DMat3::from_rotation_x(roll);
        Some(Self {
            lines,
            size,
            color,
            origin,
            right: -rotation.y_axis,
            up: rotation.z_axis,
        })
    }

    /// The direction the text faces: toward its reader, against the entity's angles.
    pub fn normal(&self) -> DVec3 {
        self.right.cross(self.up)
    }

    /// The point `across` units along the text and `above` units over the first line's
    /// baseline.
    fn point(&self, across: f64, above: f64) -> DVec3 {
        self.origin + self.right * across + self.up * above
    }

    /// The corners of the rectangle around the text, outline included, counter-clockwise
    /// from the bottom left as the reader sees them.
    pub fn corners(&self) -> [DVec3; 4] {
        let Some(layout) = TYPEFACE_LAYOUT.as_ref() else {
            return [self.origin; 4];
        };
        let columns = self.lines.iter().map(|line| line.chars().count()).max().unwrap_or(0);
        let margin = f64::from(OUTLINE_PIXELS / EM_PIXELS);
        let lowest_line = self.lines.len().saturating_sub(1) as f64;
        let a = DVec2::new(-margin, layout.descent - margin - lowest_line) * self.size;
        let b = DVec2::new(columns as f64 * layout.advance + margin, layout.ascent + margin) * self.size;
        let (min, max) = (a.min(b), a.max(b));
        [
            self.point(min.x, min.y),
            self.point(max.x, min.y),
            self.point(max.x, max.y),
            self.point(min.x, max.y),
        ]
    }

    /// Every character that leaves a mark, in reading order.
    pub fn glyphs(&self) -> Vec<Glyph> {
        let Some(layout) = TYPEFACE_LAYOUT.as_ref() else {
            return Vec::new();
        };
        let mut glyphs = Vec::new();
        for (row, line) in self.lines.iter().enumerate() {
            for (column, character) in line.chars().enumerate() {
                if character.is_whitespace() {
                    continue;
                }
                let Some(uv) = layout.cell_uv(character) else {
                    continue;
                };
                let left = (column as f64 * layout.advance - layout.cell_left) * self.size;
                let right = left + layout.cell_width * self.size;
                let top = (layout.cell_top - row as f64) * self.size;
                let bottom = top - layout.cell_height * self.size;
                glyphs.push(Glyph {
                    corners: [
                        self.point(left, bottom),
                        self.point(right, bottom),
                        self.point(right, top),
                        self.point(left, top),
                    ],
                    uv: [[uv[0], uv[3]], [uv[2], uv[3]], [uv[2], uv[1]], [uv[0], uv[1]]],
                });
            }
        }
        glyphs
    }
}

/// Where every character is in the typeface's picture, and the measures of the typeface,
/// in ems.
struct TypefaceLayout {
    font: FontRef<'static>,
    /// The cell of each character, counted along the rows.
    cells: HashMap<char, u32>,
    /// The size of a cell, in pixels.
    cell: [u32; 2],
    /// The size of the picture, in pixels.
    size: [u32; 2],
    /// The baseline's distance from the top of a cell, in pixels.
    baseline: u32,
    /// How far the next character starts after this one.
    advance: f64,
    /// How far letters reach above and below the baseline; `descent` is below zero.
    ascent: f64,
    descent: f64,
    /// How far a cell's left edge is before its character starts, and its top edge above
    /// the baseline.
    cell_left: f64,
    cell_top: f64,
    cell_width: f64,
    cell_height: f64,
}

/// The layout of the typeface's picture, or `None` when egui has no such typeface.
static TYPEFACE_LAYOUT: LazyLock<Option<TypefaceLayout>> = LazyLock::new(TypefaceLayout::new);

impl TypefaceLayout {
    fn new() -> Option<Self> {
        let definitions = FontDefinitions::default();
        let bytes: &'static [u8] = match &definitions.font_data.get(TYPEFACE)?.font {
            Cow::Borrowed(bytes) => bytes,
            Cow::Owned(bytes) => Box::leak(bytes.clone().into_boxed_slice()),
        };
        let font = FontRef::try_from_slice(bytes).ok()?;
        let em = font.units_per_em()?;
        let advance = font.h_advance_unscaled(font.glyph_id('M')) / em;
        let (ascent, descent) = (font.ascent_unscaled() / em, font.descent_unscaled() / em);
        let characters = (0x20..=0x7e_u32)
            .chain(0xa0..=0x17f)
            .chain(0x400..=0x45f)
            .filter_map(char::from_u32)
            .chain(EXTRA_CHARACTERS.chars())
            .filter(|character| font.glyph_id(*character).0 != 0);
        let cells: HashMap<char, u32> = characters.zip(0..).collect();
        let baseline = CELL_BORDER + (ascent * EM_PIXELS).ceil() as u32;
        let cell = [
            (advance * EM_PIXELS).ceil() as u32 + 2 * CELL_BORDER,
            baseline + (-descent * EM_PIXELS).ceil() as u32 + CELL_BORDER,
        ];
        let rows = (cells.len() as u32).div_ceil(COLUMNS).max(1);
        let em_pixels = f64::from(EM_PIXELS);
        Some(Self {
            font,
            cells,
            cell,
            size: [COLUMNS * cell[0], rows * cell[1]],
            baseline,
            advance: f64::from(advance),
            ascent: f64::from(ascent),
            descent: f64::from(descent),
            cell_left: f64::from(CELL_BORDER) / em_pixels,
            cell_top: f64::from(baseline) / em_pixels,
            cell_width: f64::from(cell[0]) / em_pixels,
            cell_height: f64::from(cell[1]) / em_pixels,
        })
    }

    /// The top left pixel of a cell.
    fn cell_corner(&self, cell: u32) -> [u32; 2] {
        [(cell % COLUMNS) * self.cell[0], (cell / COLUMNS) * self.cell[1]]
    }

    /// The left, top, right and bottom of a character's cell in the picture, from 0 to 1;
    /// the cell of [`UNKNOWN`] for a character the typeface lacks.
    fn cell_uv(&self, character: char) -> Option<[f32; 4]> {
        let cell = *self.cells.get(&character).or_else(|| self.cells.get(&UNKNOWN))?;
        let [x, y] = self.cell_corner(cell);
        let [width, height] = self.size.map(|side| side as f32);
        Some([
            x as f32 / width,
            y as f32 / height,
            (x + self.cell[0]) as f32 / width,
            (y + self.cell[1]) as f32 / height,
        ])
    }

    /// How much of each pixel of a cell the character covers, from 0 to 1, row by row.
    /// Whatever reaches past the cell's border is cut off.
    fn coverage(&self, character: char) -> Vec<f32> {
        let [width, height] = self.cell;
        let mut covered = vec![0.0_f32; (width * height) as usize];
        let em = self.font.units_per_em().unwrap_or(1.0);
        let scale = PxScale::from(EM_PIXELS * self.font.height_unscaled() / em);
        let glyph = self
            .font
            .glyph_id(character)
            .with_scale_and_position(scale, point(CELL_BORDER as f32, self.baseline as f32));
        let Some(outline) = self.font.outline_glyph(glyph) else {
            return covered;
        };
        let bounds = outline.px_bounds();
        let inside = |value: i64, side: u32| (1..i64::from(side) - 1).contains(&value);
        outline.draw(|x, y, amount| {
            let (x, y) = (bounds.min.x as i64 + i64::from(x), bounds.min.y as i64 + i64::from(y));
            if inside(x, width) && inside(y, height) {
                let pixel = &mut covered[(y as u32 * width + x as u32) as usize];
                *pixel = pixel.max(amount.clamp(0.0, 1.0));
            }
        });
        covered
    }

    /// The coverage of a character's outline: its own coverage spread [`OUTLINE_PIXELS`]
    /// in every direction.
    fn outline(&self, covered: &[f32]) -> Vec<f32> {
        let [width, height] = self.cell.map(i64::from);
        let reach = OUTLINE_PIXELS.ceil() as i64 + 1;
        let mut spread = vec![0.0_f32; covered.len()];
        for (index, amount) in covered.iter().enumerate().filter(|(_, amount)| **amount > 0.0) {
            let (x, y) = (index as i64 % width, index as i64 / width);
            for near_y in (y - reach).max(0)..=(y + reach).min(height - 1) {
                for near_x in (x - reach).max(0)..=(x + reach).min(width - 1) {
                    let distance = (((near_x - x).pow(2) + (near_y - y).pow(2)) as f32).sqrt();
                    let falloff = (OUTLINE_PIXELS + 0.5 - distance).clamp(0.0, 1.0);
                    let pixel = &mut spread[(near_y * width + near_x) as usize];
                    *pixel = pixel.max(amount * falloff);
                }
            }
        }
        spread
    }
}

/// The picture of the typeface: every character white on its black outline, clear
/// elsewhere, one cell for each. `None` when egui has no such typeface.
pub fn typeface_picture() -> Option<Mips> {
    let layout = TYPEFACE_LAYOUT.as_ref()?;
    let [width, height] = layout.size;
    let mut data = vec![0_u8; (width * height * 4) as usize];
    for (character, cell) in &layout.cells {
        let covered = layout.coverage(*character);
        let outline = layout.outline(&covered);
        let [left, top] = layout.cell_corner(*cell);
        for (index, (letter, around)) in covered.iter().zip(&outline).enumerate() {
            let alpha = letter + (1.0 - letter) * around;
            if alpha <= 0.0 {
                continue;
            }
            let (x, y) = (left + index as u32 % layout.cell[0], top + index as u32 / layout.cell[0]);
            let white = ((letter / alpha).powf(1.0 / 2.2) * 255.0).round() as u8;
            let at = ((y * width + x) * 4) as usize;
            data[at..at + 4].copy_from_slice(&[white, white, white, (alpha * 255.0).round() as u8]);
        }
    }
    Some(Mips::from_rgba((width, height), Level { width, height, data }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mallet_vmf::Vmf;

    fn text_of(keys: &str) -> Option<WorldText> {
        let vmf = Vmf::parse(&format!(
            "entity\n{{\n\"id\" \"1\"\n\"classname\" \"editor_text\"\n\"origin\" \"64 32 16\"\n{keys}\n}}\n"
        ))
        .unwrap();
        let entity = vmf.entities().next().unwrap();
        WorldText::read(entity, None, entity.origin().unwrap())
    }

    #[test]
    fn reads_the_message_size_and_colour() {
        let text = text_of("\"message\" \"Hello\\nWorld!\"\n\"textsize\" \"24\"\n\"color\" \"255 128 0\"").unwrap();
        assert_eq!(text.lines, ["Hello", "World!"]);
        assert_eq!(text.size, 24.0);
        assert_eq!(text.color, [255, 128, 0]);
        let plain = text_of("\"message\" \"Hi\"").unwrap();
        assert_eq!((plain.size, plain.color), (10.0, [255; 3]));
    }

    #[test]
    fn an_entity_with_nothing_to_show_keeps_its_box() {
        assert!(text_of("").is_none());
        assert!(text_of("\"message\" \"  \"").is_none());
        assert!(text_of("\"message\" \"Hi\"\n\"textsize\" \"0\"").is_none());
    }

    #[test]
    fn text_starts_at_the_origin_and_runs_to_the_right_of_its_angles() {
        let text = text_of("\"message\" \"Hi\"").unwrap();
        assert!(text.right.abs_diff_eq(DVec3::NEG_Y, 1e-9));
        assert!(text.up.abs_diff_eq(DVec3::Z, 1e-9));
        assert!(text.normal().abs_diff_eq(DVec3::NEG_X, 1e-9));
        let turned = text_of("\"message\" \"Hi\"\n\"angles\" \"0 90 0\"").unwrap();
        assert!(turned.right.abs_diff_eq(DVec3::X, 1e-9));
        assert!(turned.normal().abs_diff_eq(DVec3::NEG_Y, 1e-9));
        let [bottom_left, bottom_right, top_right, _] = turned.corners();
        assert!(bottom_left.x < 64.0 && bottom_left.x > 63.0);
        assert!(bottom_left.z < 16.0 && top_right.z > 16.0 + 5.0);
        assert!((bottom_right.x - bottom_left.x - 2.0 * 10.0 * 0.602).abs() < 1.5);
    }

    #[test]
    fn every_drawn_character_has_its_own_cell() {
        let text = text_of("\"message\" \"A b\\n\u{416}\u{e9}\u{1f600}\"").unwrap();
        let glyphs = text.glyphs();
        assert_eq!(glyphs.len(), 5);
        let layout = TYPEFACE_LAYOUT.as_ref().unwrap();
        assert_eq!(glyphs[4].uv[3], {
            let uv = layout.cell_uv(UNKNOWN).unwrap();
            [uv[0], uv[1]]
        });
        assert_ne!(glyphs[2].uv, glyphs[3].uv);
        assert!(glyphs[2].corners[0].z < glyphs[0].corners[0].z);
    }

    #[test]
    fn the_picture_has_white_letters_on_black_outlines() {
        let picture = typeface_picture().unwrap();
        let level = &picture.levels[0];
        let layout = TYPEFACE_LAYOUT.as_ref().unwrap();
        assert_eq!([level.width, level.height], layout.size);
        let pixels: Vec<&[u8]> = level.data.chunks_exact(4).collect();
        assert!(pixels.iter().any(|pixel| pixel[0] == 255 && pixel[3] == 255));
        assert!(pixels.iter().any(|pixel| pixel[0] == 0 && pixel[3] == 255));
        assert!(pixels.iter().any(|pixel| pixel[3] == 0));
        let [x, y] = layout.cell_corner(layout.cells[&'W']);
        let edge = |dx: u32, dy: u32| level.data[(((y + dy) * level.width + x + dx) * 4 + 3) as usize];
        assert!((0..layout.cell[0]).all(|dx| edge(dx, 0) == 0 && edge(dx, layout.cell[1] - 1) == 0));
        assert!((0..layout.cell[1]).all(|dy| edge(0, dy) == 0 && edge(layout.cell[0] - 1, dy) == 0));
    }
}
