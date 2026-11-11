//! Valve Texture Format (VTF) decoding, versions 7.0 to 7.5.
//!
//! Only the first frame, face and depth slice is decoded, which is all an editor viewport
//! needs. A VTF holds its picture at many sizes, smallest first, and only the sizes that
//! are asked for are read from the file, so the large ones a viewport never shows cost
//! neither disk time nor memory.

/// A decoded texture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// Width of the full-size texture, used for texture coordinates.
    pub width: u32,
    /// Height of the full-size texture, used for texture coordinates.
    pub height: u32,
    /// Width of the decoded mip level.
    pub pixel_width: u32,
    /// Height of the decoded mip level.
    pub pixel_height: u32,
    /// RGBA pixels of the decoded mip level, row by row from the top.
    pub rgba: Vec<u8>,
}

impl Image {
    /// Halves the picture until its sides are at most `max_side` pixels, for textures
    /// stored without smaller sizes of their own.
    fn fit(mut self, max_side: u32) -> Self {
        while self.pixel_width.max(self.pixel_height) > max_side.max(1) {
            let half = halve(&Level {
                width: self.pixel_width,
                height: self.pixel_height,
                data: std::mem::take(&mut self.rgba),
            });
            self.pixel_width = half.width;
            self.pixel_height = half.height;
            self.rgba = half.data;
        }
        self
    }
}

/// How the bytes of a [`Level`] store its pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// Four bytes a pixel: red, green, blue and alpha, row by row from the top.
    Rgba,
    /// DXT1 blocks, also called BC1: eight bytes for every four by four pixels.
    Bc1,
    /// DXT3 blocks, also called BC2: sixteen bytes for every four by four pixels.
    Bc2,
    /// DXT5 blocks, also called BC3: sixteen bytes for every four by four pixels.
    Bc3,
}

/// One size of a texture's picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Level {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// A texture as a graphics card takes it: its picture at every size from the largest one
/// shown down to the smallest, each half the one before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mips {
    /// Width of the full-size texture, used for texture coordinates.
    pub width: u32,
    /// Height of the full-size texture, used for texture coordinates.
    pub height: u32,
    pub encoding: Encoding,
    /// The picture's sizes, largest first.
    pub levels: Vec<Level>,
    /// How many animation frames the file holds; `levels` is one of them.
    pub frames: u32,
}

impl Mips {
    /// An RGBA picture with every smaller size made from it, each pixel the average of
    /// the four it covers. `size` is the full-size texture's width and height.
    pub fn from_rgba(size: (u32, u32), level: Level) -> Self {
        let mut levels = vec![level];
        while let Some(half) = levels
            .last()
            .filter(|last| last.width > 1 || last.height > 1)
            .map(halve)
        {
            levels.push(half);
        }
        Self {
            width: size.0,
            height: size.1,
            encoding: Encoding::Rgba,
            levels,
            frames: 1,
        }
    }

    /// A decoded texture with every smaller size made from it.
    pub fn from_image(image: Image) -> Self {
        Self::from_rgba(
            (image.width, image.height),
            Level {
                width: image.pixel_width,
                height: image.pixel_height,
                data: image.rgba,
            },
        )
    }

    /// How many bytes every size of the picture takes together.
    pub fn bytes(&self) -> usize {
        self.levels.iter().map(|level| level.data.len()).sum()
    }
}

const FLAG_ENVMAP: u32 = 0x4000;
const RESOURCE_HIGH_RES_IMAGE: [u8; 3] = [0x30, 0, 0];
/// Bytes read from the start of a VTF to find out where its picture lies. The longest
/// header, with every resource a 7.5 file may list, is shorter.
const HEADER_BYTES: u64 = 1024;
/// How many of the smallest sizes of a block-compressed picture a file may leave out and
/// still be handed to the graphics card as stored: those under one block.
const OPTIONAL_LEVELS: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Rgba8888,
    Abgr8888,
    Rgb888,
    Bgr888,
    Rgb565,
    I8,
    Ia88,
    A8,
    Argb8888,
    Bgra8888,
    Dxt1,
    Dxt3,
    Dxt5,
    Bgrx8888,
    Bgr565,
    Bgra4444,
    Bgrx5551,
    Bgra5551,
    Uv88,
}

impl Format {
    fn from_id(id: i32) -> Option<Self> {
        Some(match id {
            0 => Self::Rgba8888,
            1 => Self::Abgr8888,
            2 | 9 => Self::Rgb888,
            3 | 10 => Self::Bgr888,
            4 => Self::Rgb565,
            5 => Self::I8,
            6 => Self::Ia88,
            8 => Self::A8,
            11 => Self::Argb8888,
            12 => Self::Bgra8888,
            13 | 20 => Self::Dxt1,
            14 => Self::Dxt3,
            15 => Self::Dxt5,
            16 => Self::Bgrx8888,
            17 => Self::Bgr565,
            18 => Self::Bgrx5551,
            19 => Self::Bgra4444,
            21 => Self::Bgra5551,
            22 => Self::Uv88,
            _ => return None,
        })
    }

    /// Bytes needed for one image of this size.
    fn size(self, width: u32, height: u32) -> usize {
        let (width, height) = (width as usize, height as usize);
        let blocks = width.div_ceil(4).max(1) * height.div_ceil(4).max(1);
        match self {
            Self::Dxt1 => blocks * 8,
            Self::Dxt3 | Self::Dxt5 => blocks * 16,
            Self::I8 | Self::A8 => width * height,
            Self::Ia88
            | Self::Rgb565
            | Self::Bgr565
            | Self::Bgra4444
            | Self::Bgrx5551
            | Self::Bgra5551
            | Self::Uv88 => width * height * 2,
            Self::Rgb888 | Self::Bgr888 => width * height * 3,
            Self::Rgba8888 | Self::Abgr8888 | Self::Argb8888 | Self::Bgra8888 | Self::Bgrx8888 => width * height * 4,
        }
    }

    /// The blocks a graphics card can sample as the file stores them, or [`Encoding::Rgba`]
    /// for formats that have to be decoded first.
    fn encoding(self) -> Encoding {
        match self {
            Self::Dxt1 => Encoding::Bc1,
            Self::Dxt3 => Encoding::Bc2,
            Self::Dxt5 => Encoding::Bc3,
            _ => Encoding::Rgba,
        }
    }
}

/// What a VTF's header says about its picture and where it lies in the file.
#[derive(Clone, Copy, Debug)]
struct Header {
    width: u32,
    height: u32,
    format: Format,
    mip_count: u32,
    /// Images stored for each mip level: frames times faces times depth slices.
    copies: u64,
    /// Animation frames, each `copies / frames` images of every mip level.
    frames: u64,
    /// Where the mip levels start, smallest first.
    start: u64,
}

impl Header {
    /// Reads the header through `read`, which gives part of the file.
    fn read(read: &impl Fn(u64, u64) -> Option<Vec<u8>>) -> Result<Self, String> {
        Self::parse(&read(0, HEADER_BYTES).ok_or("VTF file cannot be read")?)
    }

    fn parse(data: &[u8]) -> Result<Self, String> {
        let read_u16 = |at: usize| data.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let read_u32 = |at: usize| {
            data.get(at..at + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        if data.get(0..4) != Some(b"VTF\0") {
            return Err("not a VTF file".to_owned());
        }
        let truncated = || "VTF header is truncated".to_owned();
        let minor = read_u32(8).ok_or_else(truncated)?;
        let header_size = u64::from(read_u32(12).ok_or_else(truncated)?);
        let width = u32::from(read_u16(16).ok_or_else(truncated)?);
        let height = u32::from(read_u16(18).ok_or_else(truncated)?);
        let flags = read_u32(20).ok_or_else(truncated)?;
        let frames = u64::from(read_u16(24).ok_or_else(truncated)?.max(1));
        let format_id = read_u32(52).ok_or_else(truncated)? as i32;
        let mip_count = u32::from(*data.get(56).ok_or_else(truncated)?).max(1);
        let low_format_id = read_u32(57).ok_or_else(truncated)? as i32;
        let low_width = u32::from(*data.get(61).ok_or_else(truncated)?);
        let low_height = u32::from(*data.get(62).ok_or_else(truncated)?);
        let depth = if minor >= 2 {
            u64::from(read_u16(63).ok_or_else(truncated)?.max(1))
        } else {
            1
        };
        let format = Format::from_id(format_id).ok_or_else(|| format!("unsupported VTF image format {format_id}"))?;
        let faces = if flags & FLAG_ENVMAP != 0 { 6 } else { 1 };

        let start = if minor >= 3 {
            let resource_count = read_u32(68).ok_or_else(truncated)? as usize;
            let offset = (0..resource_count)
                .find_map(|index| {
                    let at = 80usize.checked_add(index.checked_mul(8)?)?;
                    (data.get(at..at + 3)? == RESOURCE_HIGH_RES_IMAGE)
                        .then(|| read_u32(at + 4))
                        .flatten()
                })
                .ok_or("VTF has no image resource")?;
            u64::from(offset)
        } else {
            let low_res_size = Format::from_id(low_format_id).map_or(0, |low| {
                if low_width == 0 || low_height == 0 {
                    0
                } else {
                    low.size(low_width, low_height)
                }
            });
            header_size + low_res_size as u64
        };
        Ok(Self {
            width,
            height,
            format,
            mip_count,
            copies: frames * faces * depth,
            frames,
            start,
        })
    }

    fn mip_size(&self, level: u32) -> (u32, u32) {
        let side = |full: u32| full.checked_shr(level).unwrap_or(0).max(1);
        (side(self.width), side(self.height))
    }

    /// The largest mip level whose sides are at most `max_side` pixels, or the smallest
    /// level the file has if none fits.
    fn level_for(&self, max_side: u32) -> u32 {
        (0..self.mip_count)
            .find(|&level| {
                let (width, height) = self.mip_size(level);
                width.max(height) <= max_side
            })
            .unwrap_or(self.mip_count - 1)
    }

    /// Bytes one image of a mip level takes.
    fn level_bytes(&self, level: u32) -> u64 {
        let (width, height) = self.mip_size(level);
        self.format.size(width, height) as u64
    }

    /// Where the first image of animation frame `frame` of a mip level lies in the file,
    /// and how long it is. The smaller levels come before it, each with all its frames,
    /// faces and slices, and then the level's earlier frames.
    fn level_range(&self, level: u32, frame: u64) -> (u64, u64) {
        let smaller = (level + 1..self.mip_count)
            .map(|smaller| self.level_bytes(smaller).saturating_mul(self.copies))
            .fold(0, u64::saturating_add);
        let bytes = self.level_bytes(level);
        let earlier = bytes
            .saturating_mul(self.copies / self.frames.max(1))
            .saturating_mul(frame);
        (self.start.saturating_add(smaller).saturating_add(earlier), bytes)
    }

    /// Decodes animation frame `frame` of one mip level to RGBA, scaled down to fit
    /// `max_side`.
    fn decode(
        &self,
        read: &impl Fn(u64, u64) -> Option<Vec<u8>>,
        level: u32,
        max_side: u32,
        frame: u64,
    ) -> Result<Image, String> {
        let (start, length) = self.level_range(level, frame);
        let source = read_exactly(read, start, length)?;
        let (pixel_width, pixel_height) = self.mip_size(level);
        let image = Image {
            width: self.width,
            height: self.height,
            pixel_width,
            pixel_height,
            rgba: to_rgba(self.format, &source, pixel_width, pixel_height),
        };
        Ok(image.fit(max_side))
    }
}

/// Reads exactly `length` bytes of a VTF from `start`.
fn read_exactly(read: &impl Fn(u64, u64) -> Option<Vec<u8>>, start: u64, length: u64) -> Result<Vec<u8>, String> {
    read(start, length)
        .filter(|data| data.len() as u64 == length)
        .ok_or_else(|| "VTF image data is truncated".to_owned())
}

/// Decodes a whole VTF file held in memory, as [`load`] does one read a part at a time.
pub fn decode(data: &[u8], max_side: u32) -> Result<Image, String> {
    load(
        |start, length| {
            let start = usize::try_from(start).ok()?.min(data.len());
            let end = start.saturating_add(usize::try_from(length).unwrap_or(usize::MAX));
            Some(data[start..end.min(data.len())].to_vec())
        },
        max_side,
    )
}

/// The width and height of a VTF's full-size picture, from its header alone. `read`
/// gives part of the file, as for [`load`].
pub fn size(read: impl Fn(u64, u64) -> Option<Vec<u8>>) -> Result<[u32; 2], String> {
    let header = Header::read(&read)?;
    Ok([header.width, header.height])
}

/// Decodes the largest size of a VTF's picture whose sides are at most `max_side` pixels.
/// `read` gives `length` bytes of the file from `start`, or fewer at its end, so only the
/// header and that one size are read. A texture stored without a size that small is
/// scaled down to fit.
pub fn load(read: impl Fn(u64, u64) -> Option<Vec<u8>>, max_side: u32) -> Result<Image, String> {
    let header = Header::read(&read)?;
    header.decode(&read, header.level_for(max_side), max_side, 0)
}

/// Reads a VTF's picture for a graphics card, at every size from the largest whose sides
/// are at most `max_side` pixels down. With `blocks`, a DXT picture is handed over as the
/// file stores it, which takes an eighth to a quarter of the memory of its pixels and no
/// decoding. Otherwise, and for the other formats, the largest size is decoded as [`load`]
/// does and the smaller ones are made from it.
pub fn load_mips(read: impl Fn(u64, u64) -> Option<Vec<u8>>, max_side: u32, blocks: bool) -> Result<Mips, String> {
    load_frame_mips(read, max_side, blocks, 0)
}

/// Reads animation frame `frame` of a VTF's picture as [`load_mips`] reads the first, for
/// animated sprites. A frame past the last is an error.
pub fn load_frame_mips(
    read: impl Fn(u64, u64) -> Option<Vec<u8>>,
    max_side: u32,
    blocks: bool,
    frame: u32,
) -> Result<Mips, String> {
    let header = Header::read(&read)?;
    let frame = u64::from(frame);
    if frame >= header.frames {
        return Err(format!("VTF has no frame {frame}"));
    }
    let level = header.level_for(max_side);
    let (width, height) = header.mip_size(level);
    let encoding = header.format.encoding();
    let full_chain = 32 - width.max(height).leading_zeros();
    let count = full_chain.min(header.mip_count - level);
    let as_stored = blocks
        && encoding != Encoding::Rgba
        && width.max(height) <= max_side
        && width % 4 == 0
        && height % 4 == 0
        && count + OPTIONAL_LEVELS >= full_chain;
    if !as_stored {
        return header.decode(&read, level, max_side, frame).map(|image| Mips {
            frames: header.frames as u32,
            ..Mips::from_image(image)
        });
    }
    let (start, length) = header.level_range(level, frame);
    let data = read_exactly(&read, header.start, start.saturating_add(length) - header.start)?;
    let levels = (level..level + count)
        .map(|level| {
            let (start, length) = header.level_range(level, frame);
            let at = usize::try_from(start - header.start).ok()?;
            let (width, height) = header.mip_size(level);
            Some(Level {
                width,
                height,
                data: data.get(at..at.checked_add(length as usize)?)?.to_vec(),
            })
        })
        .collect::<Option<Vec<Level>>>()
        .ok_or("VTF image data is truncated")?;
    Ok(Mips {
        width: header.width,
        height: header.height,
        encoding,
        levels,
        frames: header.frames as u32,
    })
}

/// Halves an RGBA picture, each pixel the average of the four it covers. A side of one
/// pixel stays one pixel.
fn halve(level: &Level) -> Level {
    let (from_width, from_height) = (level.width as usize, level.height as usize);
    let (width, height) = ((from_width / 2).max(1), (from_height / 2).max(1));
    let mut data = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let top = (y * 2).min(from_height - 1) * from_width * 4;
        let bottom = (y * 2 + 1).min(from_height - 1) * from_width * 4;
        for x in 0..width {
            let left = (x * 2).min(from_width - 1) * 4;
            let right = (x * 2 + 1).min(from_width - 1) * 4;
            for channel in 0..4 {
                let sum = u32::from(level.data[top + left + channel])
                    + u32::from(level.data[top + right + channel])
                    + u32::from(level.data[bottom + left + channel])
                    + u32::from(level.data[bottom + right + channel]);
                data.push((sum / 4) as u8);
            }
        }
    }
    Level {
        width: width as u32,
        height: height as u32,
        data,
    }
}

fn to_rgba(format: Format, source: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let block = match format {
        Format::Dxt1 => Some(texpresso::Format::Bc1),
        Format::Dxt3 => Some(texpresso::Format::Bc2),
        Format::Dxt5 => Some(texpresso::Format::Bc3),
        _ => None,
    };
    if let Some(block) = block {
        let mut rgba = vec![0u8; w * h * 4];
        block.decompress(source, w, h, &mut rgba);
        return rgba;
    }
    let expand5 = |value: u16| ((value << 3) | (value >> 2)) as u8;
    let expand6 = |value: u16| ((value << 2) | (value >> 4)) as u8;
    let expand4 = |value: u16| ((value << 4) | value) as u8;
    let pixel_size = format.size(1, 1);
    source
        .chunks_exact(pixel_size)
        .take(w * h)
        .flat_map(|p| {
            let short = || u16::from_le_bytes([p[0], p[1]]);
            match format {
                Format::Rgba8888 => [p[0], p[1], p[2], p[3]],
                Format::Abgr8888 => [p[3], p[2], p[1], p[0]],
                Format::Argb8888 => [p[1], p[2], p[3], p[0]],
                Format::Bgra8888 => [p[2], p[1], p[0], p[3]],
                Format::Bgrx8888 => [p[2], p[1], p[0], 255],
                Format::Rgb888 => [p[0], p[1], p[2], 255],
                Format::Bgr888 => [p[2], p[1], p[0], 255],
                Format::I8 => [p[0], p[0], p[0], 255],
                Format::Ia88 => [p[0], p[0], p[0], p[1]],
                Format::A8 => [255, 255, 255, p[0]],
                Format::Uv88 => [p[0], p[1], 255, 255],
                Format::Rgb565 => {
                    let v = short();
                    [expand5(v & 31), expand6((v >> 5) & 63), expand5(v >> 11), 255]
                }
                Format::Bgr565 => {
                    let v = short();
                    [expand5(v >> 11), expand6((v >> 5) & 63), expand5(v & 31), 255]
                }
                Format::Bgra4444 => {
                    let v = short();
                    [
                        expand4((v >> 8) & 15),
                        expand4((v >> 4) & 15),
                        expand4(v & 15),
                        expand4(v >> 12),
                    ]
                }
                Format::Bgrx5551 | Format::Bgra5551 => {
                    let v = short();
                    let alpha = if format == Format::Bgrx5551 || v >> 15 == 1 {
                        255
                    } else {
                        0
                    };
                    [expand5((v >> 10) & 31), expand5((v >> 5) & 31), expand5(v & 31), alpha]
                }
                Format::Dxt1 | Format::Dxt3 | Format::Dxt5 => unreachable!("block formats return above"),
            }
        })
        .collect()
}
