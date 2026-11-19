//! Contact sheets for AI agents: thumbnails laid out in a grid with a caption under each,
//! put together on the CPU. Nothing here needs the window or the graphics card, so the
//! tools that show materials and models work while the window is covered and never hold
//! up a frame.

use ab_glyph::{point, Font, PxScale, ScaleFont};

use crate::model_render::Picture;

/// The size of an em of the caption letters, in pixels.
const CAPTION_EM: f32 = 12.0;
/// The room one caption line takes, in pixels.
const LINE_HEIGHT: u32 = 15;
/// The space around the sheet, between tiles and between a picture and its caption.
const GAP: u32 = 8;
/// The side of a square of the pattern behind see-through pictures, in pixels.
const CHECK: u32 = 8;
/// The widest a sheet is laid out, in pixels, so it reaches a vision model unscaled.
const MAX_WIDTH: u32 = 1280;

const BACKGROUND: [u8; 3] = [28, 29, 32];
const PLATE: [u8; 3] = [46, 48, 54];
const CHECK_DARK: [u8; 3] = [98, 100, 106];
const CHECK_LIGHT: [u8; 3] = [130, 132, 140];
const BACKDROP: [u8; 3] = [74, 76, 83];
const CAPTION: [u8; 3] = [232, 232, 232];
const FAINT: [u8; 3] = [158, 160, 166];
const PROBLEM: [u8; 3] = [240, 128, 104];

/// One picture of a contact sheet.
#[derive(Debug, Clone)]
pub struct Tile {
    /// The picture, or why there is none, which is written in its place.
    pub picture: Result<Picture, String>,
    /// What the picture's alpha lets show through.
    pub behind: Behind,
    /// The lines under the picture. The first is the bright one.
    pub caption: Vec<String>,
}

/// What lies behind a picture where its alpha is low.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behind {
    /// Nothing: the alpha is left out, as an opaque material's alpha masks something
    /// else than what can be seen through.
    Nothing,
    /// A chequered pattern, which says that a texture is see-through there.
    Pattern,
    /// One plain colour, for a picture of a thing rather than of a surface.
    Backdrop,
}

/// A finished sheet: opaque RGBA pixels, row by row from the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sheet {
    pub width: u32,
    pub height: u32,
    rgba: Vec<u8>,
}

/// How many tiles of `side` pixels fit beside each other on a sheet, at most `count`.
pub fn columns_for(side: u32, count: usize) -> u32 {
    let fitting = (MAX_WIDTH - GAP) / (side + GAP);
    fitting.clamp(1, 10).min(count.max(1) as u32)
}

/// How many letters of a caption fit under a tile of `side` pixels.
pub fn caption_letters(side: u32) -> usize {
    (side as f32 / letter_advance()).floor().max(1.0) as usize
}

/// Shortens `text` to `letters` letters by leaving out its middle, with two dots in its
/// place: a name's start says what it is and its end tells it from its neighbours.
pub fn shorten(text: &str, letters: usize) -> String {
    let count = text.chars().count();
    if count <= letters {
        return text.to_owned();
    }
    let kept = letters.saturating_sub(2);
    let head: String = text.chars().take(kept.div_ceil(2)).collect();
    let tail: String = text.chars().skip(count - kept / 2).collect();
    format!("{head}..{tail}")
}

impl Sheet {
    /// Lays tiles out in rows of `columns`, each picture fitted into a square of `side`
    /// pixels, with `lines` caption lines under it.
    pub fn of_tiles(tiles: &[Tile], side: u32, columns: u32, lines: u32) -> Self {
        let side = side.max(16);
        let columns = columns.max(1);
        let rows = (tiles.len() as u32).div_ceil(columns).max(1);
        let cell_height = side + GAP / 2 + lines * LINE_HEIGHT;
        let width = GAP + columns * (side + GAP);
        let height = GAP + rows * (cell_height + GAP);
        let mut sheet = Self {
            width,
            height,
            rgba: BACKGROUND
                .iter()
                .copied()
                .chain([255])
                .cycle()
                .take((width * height * 4) as usize)
                .collect(),
        };
        let letters = caption_letters(side);
        for (index, tile) in tiles.iter().enumerate() {
            let column = index as u32 % columns;
            let row = index as u32 / columns;
            let left = GAP + column * (side + GAP);
            let top = GAP + row * (cell_height + GAP);
            match &tile.picture {
                Ok(picture) => sheet.picture(picture, tile.behind, left, top, side),
                Err(reason) => sheet.placeholder(reason, left, top, side),
            }
            for (line, text) in tile.caption.iter().take(lines as usize).enumerate() {
                let color = if line == 0 { CAPTION } else { FAINT };
                let y = top + side + GAP / 2 + line as u32 * LINE_HEIGHT;
                sheet.text(&shorten(text, letters), left as f32, y as f32, color);
            }
        }
        sheet
    }

    /// The sheet as a PNG file.
    pub fn png(&self) -> Result<Vec<u8>, String> {
        let size = tiny_skia::IntSize::from_wh(self.width, self.height).ok_or("the picture has no area")?;
        let pixmap = tiny_skia::Pixmap::from_vec(self.rgba.clone(), size).ok_or("the picture has the wrong size")?;
        pixmap.encode_png().map_err(|error| error.to_string())
    }

    /// The colour of one pixel, for tests.
    #[cfg(test)]
    fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let at = ((y * self.width + x) * 4) as usize;
        [self.rgba[at], self.rgba[at + 1], self.rgba[at + 2]]
    }

    fn put(&mut self, x: u32, y: u32, color: [u8; 3]) {
        if x < self.width && y < self.height {
            let at = ((y * self.width + x) * 4) as usize;
            self.rgba[at..at + 3].copy_from_slice(&color);
        }
    }

    /// Blends a colour over a pixel by `amount`, from 0 to 1.
    fn blend(&mut self, x: u32, y: u32, color: [u8; 3], amount: f32) {
        if x >= self.width || y >= self.height || amount <= 0.0 {
            return;
        }
        let at = ((y * self.width + x) * 4) as usize;
        let amount = amount.min(1.0);
        for (channel, value) in color.into_iter().enumerate() {
            let under = f32::from(self.rgba[at + channel]);
            self.rgba[at + channel] = (under + (f32::from(value) - under) * amount).round() as u8;
        }
    }

    fn fill(&mut self, left: u32, top: u32, width: u32, height: u32, color: [u8; 3]) {
        for y in top..top + height {
            for x in left..left + width {
                self.put(x, y, color);
            }
        }
    }

    /// Draws a picture as large as fits a square of `side` pixels, in its middle. A
    /// picture drawn smaller than it is has its pixels averaged; one drawn larger keeps
    /// them sharp, so a texture of a few pixels shows as what it is.
    fn picture(&mut self, picture: &Picture, behind: Behind, left: u32, top: u32, side: u32) {
        let (from_width, from_height) = (picture.width.max(1), picture.height.max(1));
        if picture.rgba.len() < (from_width * from_height * 4) as usize {
            return self.placeholder("no picture", left, top, side);
        }
        let see_through = behind != Behind::Nothing;
        if behind == Behind::Backdrop {
            self.fill(left, top, side, side, BACKDROP);
        }
        let scale = side as f32 / from_width.max(from_height) as f32;
        let width = ((from_width as f32 * scale).round() as u32).clamp(1, side);
        let height = ((from_height as f32 * scale).round() as u32).clamp(1, side);
        let (left, top) = (left + (side - width) / 2, top + (side - height) / 2);
        let texel = |x: u32, y: u32| {
            let at = ((y.min(from_height - 1) * from_width + x.min(from_width - 1)) * 4) as usize;
            [0, 1, 2, 3].map(|channel| f32::from(picture.rgba[at + channel]))
        };
        for y in 0..height {
            let first_row = y * from_height / height;
            let last_row = ((y + 1) * from_height).div_ceil(height).max(first_row + 1);
            for x in 0..width {
                let first_column = x * from_width / width;
                let last_column = ((x + 1) * from_width).div_ceil(width).max(first_column + 1);
                let mut sum = [0.0_f32; 4];
                for row in first_row..last_row {
                    for column in first_column..last_column {
                        let [red, green, blue, alpha] = texel(column, row);
                        let cover = if see_through { alpha / 255.0 } else { 1.0 };
                        sum[0] += red * cover;
                        sum[1] += green * cover;
                        sum[2] += blue * cover;
                        sum[3] += cover;
                    }
                }
                let count = ((last_row - first_row) * (last_column - first_column)) as f32;
                let cover = sum[3] / count;
                let color = if sum[3] > 0.0 {
                    [0, 1, 2].map(|channel| (sum[channel] / sum[3]).round().clamp(0.0, 255.0) as u8)
                } else {
                    [0; 3]
                };
                let (at_x, at_y) = (left + x, top + y);
                if behind == Behind::Pattern {
                    let light = (x / CHECK + y / CHECK) % 2 == 0;
                    self.put(at_x, at_y, if light { CHECK_LIGHT } else { CHECK_DARK });
                }
                self.blend(at_x, at_y, color, cover);
            }
        }
    }

    /// Draws a plate that says why a tile has no picture.
    fn placeholder(&mut self, reason: &str, left: u32, top: u32, side: u32) {
        self.fill(left, top, side, side, PLATE);
        let letters = caption_letters(side.saturating_sub(GAP));
        let words: Vec<&str> = reason.split_whitespace().collect();
        let mut lines: Vec<String> = Vec::new();
        for word in words {
            match lines.last_mut() {
                Some(line) if line.chars().count() + 1 + word.chars().count() <= letters => {
                    line.push(' ');
                    line.push_str(word);
                }
                _ => lines.push(shorten(word, letters)),
            }
        }
        let most = (side / LINE_HEIGHT).max(1) as usize;
        lines.truncate(most);
        let block = lines.len() as u32 * LINE_HEIGHT;
        let start = top + side.saturating_sub(block) / 2;
        for (index, line) in lines.iter().enumerate() {
            let y = start + index as u32 * LINE_HEIGHT;
            self.text(line, (left + GAP / 2) as f32, y as f32, PROBLEM);
        }
    }

    /// Writes one line of text with its top left corner at `(left, top)`.
    fn text(&mut self, text: &str, left: f32, top: f32, color: [u8; 3]) {
        let Some(font) = crate::world_text::typeface() else {
            return;
        };
        let scale = letter_scale(font);
        let scaled = font.as_scaled(scale);
        let baseline = top + scaled.ascent();
        let mut pen = left;
        for character in text.chars() {
            let known = font.glyph_id(character).0 != 0;
            let id = font.glyph_id(if known { character } else { '?' });
            let glyph = id.with_scale_and_position(scale, point(pen, baseline));
            pen += scaled.h_advance(id);
            let Some(outline) = font.outline_glyph(glyph) else {
                continue;
            };
            let bounds = outline.px_bounds();
            let mut marks: Vec<(i64, i64, f32)> = Vec::new();
            outline.draw(|x, y, amount| {
                marks.push((
                    bounds.min.x as i64 + i64::from(x),
                    bounds.min.y as i64 + i64::from(y),
                    amount,
                ));
            });
            for (x, y, amount) in marks {
                if x >= 0 && y >= 0 {
                    self.blend(x as u32, y as u32, color, amount);
                }
            }
        }
    }
}

/// The scale that makes an em of `font` [`CAPTION_EM`] pixels.
fn letter_scale(font: &impl Font) -> PxScale {
    let em = font.units_per_em().unwrap_or(1.0);
    PxScale::from(CAPTION_EM * font.height_unscaled() / em)
}

/// How far one caption letter moves the pen, in pixels. The typeface is monospace, so
/// every letter moves it as far.
fn letter_advance() -> f32 {
    crate::world_text::typeface().map_or(CAPTION_EM * 0.6, |font| {
        font.as_scaled(letter_scale(font)).h_advance(font.glyph_id('M'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(width: u32, height: u32, color: [u8; 4]) -> Picture {
        Picture {
            width,
            height,
            rgba: color
                .iter()
                .copied()
                .cycle()
                .take((width * height * 4) as usize)
                .collect(),
        }
    }

    fn tile(picture: Result<Picture, String>, behind: Behind) -> Tile {
        Tile {
            picture,
            behind,
            caption: vec!["1 name".to_owned(), "folder/".to_owned()],
        }
    }

    #[test]
    fn tiles_are_laid_out_in_rows_with_room_for_captions() {
        let tiles: Vec<Tile> = (0..5)
            .map(|_| tile(Ok(flat(64, 64, [200, 0, 0, 255])), Behind::Nothing))
            .collect();
        let sheet = Sheet::of_tiles(&tiles, 64, 3, 2);
        assert_eq!(sheet.width, GAP + 3 * (64 + GAP));
        assert_eq!(sheet.height, GAP + 2 * (64 + GAP / 2 + 2 * LINE_HEIGHT + GAP));
        assert_eq!(sheet.pixel(GAP + 10, GAP + 10), [200, 0, 0]);
        assert_eq!(sheet.pixel(GAP + 2 * (64 + GAP) + 10, GAP + 10), [200, 0, 0]);
        let second_row = GAP + 64 + GAP / 2 + 2 * LINE_HEIGHT + GAP;
        assert_eq!(sheet.pixel(GAP + 64 + GAP + 10, second_row + 10), [200, 0, 0]);
        assert_eq!(sheet.pixel(GAP + 2 * (64 + GAP) + 10, second_row + 10), BACKGROUND);
        assert!(sheet.png().unwrap().starts_with(b"\x89PNG"));
    }

    #[test]
    fn see_through_pictures_show_the_pattern_and_others_ignore_alpha() {
        let clear = flat(32, 32, [255, 255, 255, 0]);
        let shown = Sheet::of_tiles(&[tile(Ok(clear.clone()), Behind::Pattern)], 32, 1, 0);
        assert_eq!(shown.pixel(GAP + 1, GAP + 1), CHECK_LIGHT);
        assert_eq!(shown.pixel(GAP + CHECK + 1, GAP + 1), CHECK_DARK);
        let masked = Sheet::of_tiles(&[tile(Ok(clear.clone()), Behind::Nothing)], 32, 1, 0);
        assert_eq!(masked.pixel(GAP + 1, GAP + 1), [255, 255, 255]);
        let mut small = clear;
        small.height = 16;
        small.rgba.truncate(32 * 16 * 4);
        let plain = Sheet::of_tiles(&[tile(Ok(small), Behind::Backdrop)], 32, 1, 0);
        assert_eq!(plain.pixel(GAP + 1, GAP + 16), BACKDROP);
        assert_eq!(
            plain.pixel(GAP + 1, GAP + 1),
            BACKDROP,
            "the backdrop fills the whole square"
        );
    }

    #[test]
    fn pictures_keep_their_shape_and_small_ones_stay_sharp() {
        let mut wide = flat(4, 2, [0, 0, 255, 255]);
        wide.rgba[0..4].copy_from_slice(&[255, 0, 0, 255]);
        let sheet = Sheet::of_tiles(&[tile(Ok(wide), Behind::Nothing)], 64, 1, 0);
        assert_eq!(
            sheet.pixel(GAP + 4, GAP + 4),
            BACKGROUND,
            "the bands above and below stay empty"
        );
        assert_eq!(
            sheet.pixel(GAP + 4, GAP + 20),
            [255, 0, 0],
            "one texel fills a block of pixels"
        );
        assert_eq!(sheet.pixel(GAP + 20, GAP + 20), [0, 0, 255]);
    }

    #[test]
    fn large_pictures_are_averaged_down() {
        let mut picture = flat(128, 128, [0, 0, 0, 255]);
        for pixel in picture.rgba.chunks_exact_mut(8) {
            pixel[0..3].copy_from_slice(&[255, 255, 255]);
        }
        let sheet = Sheet::of_tiles(&[tile(Ok(picture), Behind::Nothing)], 64, 1, 0);
        let [red, _, _] = sheet.pixel(GAP + 30, GAP + 30);
        assert!(
            (120..=135).contains(&red),
            "white and black columns average to grey: {red}"
        );
    }

    #[test]
    fn captions_and_reasons_are_written() {
        let sheet = Sheet::of_tiles(&[tile(Err("no such material".to_owned()), Behind::Nothing)], 96, 1, 2);
        assert_eq!(sheet.pixel(GAP + 1, GAP + 1), PLATE);
        let lit = |top: u32, height: u32, wanted: [u8; 3]| {
            (top..top + height).any(|y| (GAP..GAP + 96).any(|x| sheet.pixel(x, y) == wanted))
        };
        assert!(lit(GAP, 96, PROBLEM), "the reason is written on the plate");
        assert!(
            lit(GAP + 96 + GAP / 2, LINE_HEIGHT, CAPTION),
            "the first caption line is bright"
        );
    }

    #[test]
    fn long_captions_are_shortened() {
        assert_eq!(shorten("brickwall001a", 20), "brickwall001a");
        assert_eq!(shorten("brickwall001a", 8), "bri..01a");
        assert_eq!(shorten("concretefloor033k_c17", 9), "conc..c17");
        assert!(caption_letters(128) >= 14);
        assert_eq!(columns_for(128, 3), 3);
        assert!(columns_for(128, 64) >= 8);
        assert_eq!(columns_for(512, 64), 2);
    }
}
