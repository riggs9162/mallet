//! Mallet's own map format, `.mallet`: the same map a VMF holds, in a compact binary
//! form that opens and saves several times faster, plus the editor's own state.
//!
//! A VMF spends most of its loading time turning text into numbers: every face writes
//! its plane and texture axes as decimal text. A `.mallet` file stores those as numbers
//! (small whole numbers as one or two bytes, everything else as the exact 64-bit
//! value), so nothing is parsed or formatted, and stores every other string once. Keys
//! and blocks Mallet does not model are kept verbatim, exactly as [`Vmf`] keeps them, so
//! a map converts to a `.mallet` file and back to the same VMF.
//!
//! # Layout
//!
//! ```text
//! signature  8 bytes   89 'M' 'A' 'L' 'L' 0D 0A 1A   (a non-ASCII first byte and CR LF
//!                                                    catch damaging text-mode copies)
//! version    u16 LE    1
//! flags      u16 LE    0
//! sections, until the end of the file:
//!   tag          4 bytes ASCII, such as "MAP " or "META"
//!   compression  u8      0 = stored, 1 = LZ4 block
//!   size         u64 LE  bytes that follow
//!   raw size     u64 LE  bytes after decompression
//!   crc32        u32 LE  of the stored bytes
//!   data         `size` bytes
//! ```
//!
//! The `MAP ` section is a string table (every key, value, block name and material
//! once) followed by the map tree, with counts and string indices as LEB128 varints:
//!
//! ```text
//! map      count, then per item: 0 pair | 1 world entity | 2 entity | 3 block
//! entity   count, then per item: 0 pair | 1 solid | 2 block
//! solid    count, then per item: 0 pair | 1 side | 2 block
//! side     id, 9 plane numbers, material, 5 + 5 texture axis numbers, a flags byte
//!          for rotation / lightmap scale / smoothing groups and those present, then
//!          the extra items with their positions among the typed fields
//! block    name, count, then per item: 0 pair | 1 block
//! pair     key, value
//! number   varint n: an even n is the whole number zigzag(n / 2); n = 1 is followed
//!          by the 8 bytes of an f64
//! ```
//!
//! The `META` section is UTF-8 JSON that the editor reads and writes; this crate does
//! not look inside it. Readers skip sections they do not know, so later versions can
//! add some without breaking older Mallets.

use std::error::Error;
use std::fmt;

use glam::DVec3;
use mallet_kv::{Block, Item, Pair};
use mallet_vmf::{Entity, EntityItem, Side, Solid, SolidItem, TexAxis, TopLevel, Vmf};

/// The first bytes of every `.mallet` file.
pub const SIGNATURE: [u8; 8] = [0x89, b'M', b'A', b'L', b'L', 0x0D, 0x0A, 0x1A];
/// The format version this crate writes and the newest it reads.
pub const VERSION: u16 = 1;
/// The file extension, without the dot.
pub const EXTENSION: &str = "mallet";

const MAP_TAG: [u8; 4] = *b"MAP ";
const META_TAG: [u8; 4] = *b"META";
const STORED: u8 = 0;
const LZ4: u8 = 1;
/// Blocks nest deeper than this only in a damaged file.
const MAX_DEPTH: usize = 64;

const HAS_ROTATION: u8 = 1;
const HAS_LIGHTMAP_SCALE: u8 = 2;
const HAS_SMOOTHING_GROUPS: u8 = 4;

/// A map read from a `.mallet` file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MalletFile {
    /// The map itself.
    pub vmf: Vmf,
    /// The editor's own state as JSON text, if the file has any.
    pub meta: Option<String>,
}

/// Why a `.mallet` file could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    /// The file does not start with the `.mallet` signature.
    NotMallet,
    /// The file was written by a newer Mallet.
    NewerVersion(u16),
    /// The file ends early or a value is out of range.
    Damaged(String),
    /// A section's checksum does not match its bytes.
    Checksum([u8; 4]),
    /// The file has no map section.
    NoMap,
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotMallet => write!(f, "not a Mallet map"),
            Self::NewerVersion(version) => write!(
                f,
                "the map was saved by a newer Mallet (format {version}; this one reads up to {VERSION})"
            ),
            Self::Damaged(what) => write!(f, "the map file is damaged: {what}"),
            Self::Checksum(tag) => write!(
                f,
                "the map file is damaged: its {} section fails its checksum",
                String::from_utf8_lossy(tag).trim()
            ),
            Self::NoMap => write!(f, "the file holds no map"),
        }
    }
}

impl Error for FormatError {}

/// Whether bytes start like a `.mallet` file.
pub fn is_mallet(bytes: &[u8]) -> bool {
    bytes.starts_with(&SIGNATURE)
}

/// Writes a map, and the editor's state if given, as `.mallet` bytes.
pub fn write(vmf: &Vmf, meta: Option<&str>) -> Vec<u8> {
    let map = encode_map(vmf);
    let mut out = Vec::with_capacity(map.len() / 2 + 64);
    out.extend_from_slice(&SIGNATURE);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    write_section(&mut out, MAP_TAG, &map, true);
    if let Some(meta) = meta {
        write_section(&mut out, META_TAG, meta.as_bytes(), false);
    }
    out
}

/// Reads `.mallet` bytes.
pub fn read(bytes: &[u8]) -> Result<MalletFile, FormatError> {
    if !is_mallet(bytes) {
        return Err(FormatError::NotMallet);
    }
    let mut reader = Reader::new(bytes);
    reader.at = SIGNATURE.len();
    let version = u16::from_le_bytes(reader.array()?);
    if version > VERSION {
        return Err(FormatError::NewerVersion(version));
    }
    let _flags = u16::from_le_bytes(reader.array()?);
    let mut vmf = None;
    let mut meta = None;
    while reader.at < bytes.len() {
        let tag: [u8; 4] = reader.array()?;
        let compression = reader.byte()?;
        let size = reader.size()?;
        let raw_size = reader.size()?;
        let crc = u32::from_le_bytes(reader.array()?);
        let stored = reader.take(size)?;
        if crc32fast::hash(stored) != crc {
            return Err(FormatError::Checksum(tag));
        }
        if tag != MAP_TAG && tag != META_TAG {
            // A section from a later version.
            continue;
        }
        let data = match compression {
            STORED => stored.to_vec(),
            LZ4 => lz4_flex::block::decompress(stored, raw_size).map_err(|error| damaged(&error.to_string()))?,
            other => return Err(damaged(&format!("unknown compression {other}"))),
        };
        if tag == MAP_TAG {
            vmf = Some(decode_map(&data)?);
        } else {
            meta = Some(String::from_utf8(data).map_err(|_| damaged("the editor state is not UTF-8"))?);
        }
    }
    let vmf = vmf.ok_or(FormatError::NoMap)?;
    Ok(MalletFile { vmf, meta })
}

fn damaged(what: &str) -> FormatError {
    FormatError::Damaged(what.to_owned())
}

fn write_section(out: &mut Vec<u8>, tag: [u8; 4], data: &[u8], compress: bool) {
    let compressed = compress.then(|| lz4_flex::block::compress(data));
    let (compression, stored) = match &compressed {
        Some(compressed) => (LZ4, compressed.as_slice()),
        None => (STORED, data),
    };
    out.extend_from_slice(&tag);
    out.push(compression);
    out.extend_from_slice(&(stored.len() as u64).to_le_bytes());
    out.extend_from_slice(&(data.len() as u64).to_le_bytes());
    out.extend_from_slice(&crc32fast::hash(stored).to_le_bytes());
    out.extend_from_slice(stored);
}

// Writing.

fn encode_map(vmf: &Vmf) -> Vec<u8> {
    let mut writer = Writer::default();
    writer.count(vmf.items.len());
    for item in &vmf.items {
        match item {
            TopLevel::Pair(pair) => {
                writer.out.push(0);
                writer.pair(pair);
            }
            TopLevel::World(world) => {
                writer.out.push(1);
                writer.entity(world);
            }
            TopLevel::Entity(entity) => {
                writer.out.push(2);
                writer.entity(entity);
            }
            TopLevel::Other(block) => {
                writer.out.push(3);
                writer.block(block);
            }
        }
    }
    let Writer { strings, out, .. } = writer;
    let text_bytes: usize = strings.iter().map(|text| text.len() + 2).sum();
    let mut map = Vec::with_capacity(out.len() + text_bytes + 8);
    write_varint(&mut map, strings.len());
    for text in &strings {
        write_varint(&mut map, text.len());
        map.extend_from_slice(text.as_bytes());
    }
    map.extend_from_slice(&out);
    map
}

#[derive(Default)]
struct Writer<'a> {
    /// Every distinct string, numbered in the order first seen.
    strings: Vec<&'a str>,
    index: StringIndex<'a>,
    out: Vec<u8>,
}

impl<'a> Writer<'a> {
    fn count(&mut self, count: usize) {
        write_varint(&mut self.out, count);
    }

    fn string(&mut self, text: &'a str) {
        let next = self.strings.len();
        let at = *self.index.entry(text).or_insert(next);
        if at == next {
            self.strings.push(text);
        }
        write_varint(&mut self.out, at);
    }

    fn number(&mut self, value: f64) {
        // Whole numbers, which most coordinates and axes are, take one to three bytes.
        // Negative zero must stay negative, so it goes the long way.
        const LIMIT: f64 = (1u64 << 52) as f64;
        if value.fract() == 0.0 && value.abs() < LIMIT && !(value == 0.0 && value.is_sign_negative()) {
            let whole = value as i64;
            let zigzag = ((whole << 1) ^ (whole >> 63)) as u64;
            write_varint_u64(&mut self.out, zigzag << 1);
        } else {
            self.out.push(1);
            self.out.extend_from_slice(&value.to_le_bytes());
        }
    }

    fn vector(&mut self, vector: DVec3) {
        self.number(vector.x);
        self.number(vector.y);
        self.number(vector.z);
    }

    fn pair(&mut self, pair: &'a Pair) {
        self.string(&pair.key);
        self.string(&pair.value);
    }

    fn block(&mut self, block: &'a Block) {
        self.string(&block.name);
        self.items(&block.items);
    }

    fn items(&mut self, items: &'a [Item]) {
        self.count(items.len());
        for item in items {
            self.item(item);
        }
    }

    fn item(&mut self, item: &'a Item) {
        match item {
            Item::Pair(pair) => {
                self.out.push(0);
                self.pair(pair);
            }
            Item::Block(block) => {
                self.out.push(1);
                self.block(block);
            }
        }
    }

    fn entity(&mut self, entity: &'a Entity) {
        self.count(entity.items.len());
        for item in &entity.items {
            match item {
                EntityItem::Key(pair) => {
                    self.out.push(0);
                    self.pair(pair);
                }
                EntityItem::Solid(solid) => {
                    self.out.push(1);
                    self.solid(solid);
                }
                EntityItem::Other(block) => {
                    self.out.push(2);
                    self.block(block);
                }
            }
        }
    }

    fn solid(&mut self, solid: &'a Solid) {
        self.count(solid.items.len());
        for item in &solid.items {
            match item {
                SolidItem::Key(pair) => {
                    self.out.push(0);
                    self.pair(pair);
                }
                SolidItem::Side(side) => {
                    self.out.push(1);
                    self.side(side);
                }
                SolidItem::Other(block) => {
                    self.out.push(2);
                    self.block(block);
                }
            }
        }
    }

    fn side(&mut self, side: &'a Side) {
        write_varint(&mut self.out, side.id as usize);
        for point in side.plane {
            self.vector(point);
        }
        self.string(&side.material);
        for axis in [&side.uaxis, &side.vaxis] {
            self.vector(axis.axis);
            self.number(axis.offset);
            self.number(axis.scale);
        }
        let mut flags = 0;
        if side.rotation.is_some() {
            flags |= HAS_ROTATION;
        }
        if side.lightmap_scale.is_some() {
            flags |= HAS_LIGHTMAP_SCALE;
        }
        if side.smoothing_groups.is_some() {
            flags |= HAS_SMOOTHING_GROUPS;
        }
        self.out.push(flags);
        if let Some(rotation) = side.rotation {
            self.number(rotation);
        }
        if let Some(scale) = side.lightmap_scale {
            write_varint(&mut self.out, scale as usize);
        }
        if let Some(groups) = side.smoothing_groups {
            write_varint(&mut self.out, groups as usize);
        }
        self.count(side.extra.len());
        for (index, item) in side.extra.iter().enumerate() {
            self.item(item);
            // 0 for an item without a recorded position, else the position plus one.
            let slot = side.extra_slots.get(index).map_or(0, |slot| slot.saturating_add(1));
            write_varint(&mut self.out, slot);
        }
    }
}

fn write_varint(out: &mut Vec<u8>, value: usize) {
    write_varint_u64(out, value as u64);
}

fn write_varint_u64(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// A string-to-index map with a fast, non-cryptographic hash: interning every key and
/// value of a large map spends most of its time hashing.
type StringIndex<'a> = std::collections::HashMap<&'a str, usize, std::hash::BuildHasherDefault<FastHasher>>;

/// The multiply-rotate hash used by rustc ("FxHash"), on eight bytes at a time.
#[derive(Default)]
struct FastHasher(u64);

const FAST_HASH_SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FastHasher {
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(FAST_HASH_SEED);
    }
}

impl std::hash::Hasher for FastHasher {
    fn write(&mut self, mut bytes: &[u8]) {
        while bytes.len() >= 8 {
            self.add(u64::from_le_bytes(bytes[..8].try_into().expect("eight bytes")));
            bytes = &bytes[8..];
        }
        if !bytes.is_empty() {
            let mut word = [0; 8];
            word[..bytes.len()].copy_from_slice(bytes);
            self.add(u64::from_le_bytes(word));
        }
    }

    fn write_u8(&mut self, byte: u8) {
        self.add(u64::from(byte));
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

// Reading.

fn decode_map(data: &[u8]) -> Result<Vmf, FormatError> {
    let mut reader = Reader::new(data);
    let count = reader.count()?;
    let mut strings = Vec::with_capacity(count);
    for _ in 0..count {
        let length = reader.varint()?;
        let bytes = reader.take(length)?;
        strings.push(std::str::from_utf8(bytes).map_err(|_| damaged("a string is not UTF-8"))?);
    }
    reader.strings = strings;
    let count = reader.count()?;
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        items.push(match reader.byte()? {
            0 => TopLevel::Pair(reader.pair()?),
            1 => TopLevel::World(reader.entity()?),
            2 => TopLevel::Entity(reader.entity()?),
            3 => TopLevel::Other(reader.block(0)?),
            other => return Err(damaged(&format!("unknown map item {other}"))),
        });
    }
    if reader.at != data.len() {
        return Err(damaged("bytes after the map"));
    }
    Ok(Vmf::with_items(items))
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    strings: Vec<&'a str>,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            at: 0,
            strings: Vec::new(),
        }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], FormatError> {
        let end = self
            .at
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| damaged("it ends early"))?;
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn byte(&mut self) -> Result<u8, FormatError> {
        Ok(self.take(1)?[0])
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], FormatError> {
        Ok(self.take(N)?.try_into().expect("take returns N bytes"))
    }

    fn size(&mut self) -> Result<usize, FormatError> {
        usize::try_from(u64::from_le_bytes(self.array()?)).map_err(|_| damaged("a section is too large"))
    }

    fn varint_u64(&mut self) -> Result<u64, FormatError> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.byte()?;
            value |= u64::from(byte & 0x7F) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(damaged("a number is too long"))
    }

    fn varint(&mut self) -> Result<usize, FormatError> {
        usize::try_from(self.varint_u64()?).map_err(|_| damaged("a number is too large"))
    }

    fn u32(&mut self) -> Result<u32, FormatError> {
        u32::try_from(self.varint_u64()?).map_err(|_| damaged("a number is too large"))
    }

    /// A count of items, each at least one byte, so a damaged count cannot reserve more
    /// memory than the file could hold.
    fn count(&mut self) -> Result<usize, FormatError> {
        let count = self.varint()?;
        if count > self.bytes.len() - self.at {
            return Err(damaged("a count is larger than the file"));
        }
        Ok(count)
    }

    fn number(&mut self) -> Result<f64, FormatError> {
        let tag = self.varint_u64()?;
        if tag == 1 {
            return Ok(f64::from_le_bytes(self.array()?));
        }
        if tag & 1 != 0 {
            return Err(damaged("a number has an unknown form"));
        }
        let zigzag = tag >> 1;
        let whole = ((zigzag >> 1) as i64) ^ -((zigzag & 1) as i64);
        Ok(whole as f64)
    }

    fn vector(&mut self) -> Result<DVec3, FormatError> {
        Ok(DVec3::new(self.number()?, self.number()?, self.number()?))
    }

    fn str(&mut self) -> Result<&'a str, FormatError> {
        let index = self.varint()?;
        self.strings
            .get(index)
            .copied()
            .ok_or_else(|| damaged("a string index"))
    }

    fn string(&mut self) -> Result<String, FormatError> {
        self.str().map(str::to_owned)
    }

    fn pair(&mut self) -> Result<Pair, FormatError> {
        Ok(Pair {
            key: self.string()?,
            value: self.string()?,
        })
    }

    fn block(&mut self, depth: usize) -> Result<Block, FormatError> {
        if depth > MAX_DEPTH {
            return Err(damaged("blocks nest too deep"));
        }
        let name = self.string()?;
        let count = self.count()?;
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            items.push(self.item(depth)?);
        }
        Ok(Block { name, items })
    }

    fn item(&mut self, depth: usize) -> Result<Item, FormatError> {
        match self.byte()? {
            0 => Ok(Item::Pair(self.pair()?)),
            1 => Ok(Item::Block(self.block(depth + 1)?)),
            other => Err(damaged(&format!("unknown item {other}"))),
        }
    }

    fn entity(&mut self) -> Result<Entity, FormatError> {
        let count = self.count()?;
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            items.push(match self.byte()? {
                0 => EntityItem::Key(self.pair()?),
                1 => EntityItem::Solid(self.solid()?),
                2 => EntityItem::Other(self.block(1)?),
                other => return Err(damaged(&format!("unknown entity item {other}"))),
            });
        }
        Ok(Entity { items })
    }

    fn solid(&mut self) -> Result<Solid, FormatError> {
        let count = self.count()?;
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            items.push(match self.byte()? {
                0 => SolidItem::Key(self.pair()?),
                1 => SolidItem::Side(Box::new(self.side()?)),
                2 => SolidItem::Other(self.block(2)?),
                other => return Err(damaged(&format!("unknown solid item {other}"))),
            });
        }
        Ok(Solid { items })
    }

    fn tex_axis(&mut self) -> Result<TexAxis, FormatError> {
        Ok(TexAxis {
            axis: self.vector()?,
            offset: self.number()?,
            scale: self.number()?,
        })
    }

    fn side(&mut self) -> Result<Side, FormatError> {
        let id = self.u32()?;
        let plane = [self.vector()?, self.vector()?, self.vector()?];
        let material = self.string()?;
        let uaxis = self.tex_axis()?;
        let vaxis = self.tex_axis()?;
        let flags = self.byte()?;
        let rotation = if flags & HAS_ROTATION != 0 {
            Some(self.number()?)
        } else {
            None
        };
        let lightmap_scale = if flags & HAS_LIGHTMAP_SCALE != 0 {
            Some(self.u32()?)
        } else {
            None
        };
        let smoothing_groups = if flags & HAS_SMOOTHING_GROUPS != 0 {
            Some(self.u32()?)
        } else {
            None
        };
        let count = self.count()?;
        let mut extra = Vec::with_capacity(count);
        let mut slots = Vec::with_capacity(count);
        for _ in 0..count {
            extra.push(self.item(3)?);
            slots.push(self.varint()?);
        }
        // Positions are recorded for a leading run of the extra items (VMF loading gives
        // every one a position), so the run ends at the first item without one.
        let recorded = slots.iter().take_while(|slot| **slot != 0).count();
        let extra_slots = slots[..recorded].iter().map(|slot| slot - 1).collect();
        Ok(Side {
            id,
            plane,
            material,
            uaxis,
            vaxis,
            rotation,
            lightmap_scale,
            smoothing_groups,
            extra,
            extra_slots,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"versioninfo
{
	"editorversion" "400"
	"mapversion" "1"
}
world
{
	"id" "1"
	"classname" "worldspawn"
	solid
	{
		"id" "2"
		side
		{
			"id" "1"
			"plane" "(-0 0.1 1e300) (64.5 -3 0) (64 64 -2147483649)"
			"material" "TOOLS/TOOLSNODRAW"
			"uaxis" "[1 0 0 0.333] 0.25"
			"vaxis" "[0 -1 0 -17] 0.25"
			"rotation" "-45.5"
			"lightmapscale" "16"
			"smoothing_groups" "0"
			dispinfo
			{
				"power" "3"
				normals
				{
					"row0" "0 0 1 0 0 1"
				}
			}
			"vertices_plus" "x"
		}
		editor
		{
			"color" "0 180 227"
		}
	}
}
entity
{
	"id" "3"
	"classname" "info_player_start"
	"origin" "0 0 1"
	"targetname" "ünïcödé"
	"key" ""
}
"#;

    #[test]
    fn round_trips_exactly() {
        let vmf = Vmf::parse(SAMPLE).unwrap();
        let meta = r#"{"camera":[1,2,3]}"#;
        let bytes = write(&vmf, Some(meta));
        assert!(is_mallet(&bytes));
        let back = read(&bytes).unwrap();
        assert_eq!(back.vmf, vmf);
        assert_eq!(back.meta.as_deref(), Some(meta));
        assert_eq!(back.vmf.to_text(), vmf.to_text());
    }

    #[test]
    fn numbers_keep_their_exact_value() {
        let values = [
            0.0,
            -0.0,
            1.0,
            -1.0,
            63.0,
            64.0,
            -64.0,
            0.25,
            1e300,
            f64::MIN_POSITIVE,
            4503599627370495.0,
            4503599627370496.0,
            -9.5e15,
            f64::INFINITY,
            f64::NAN,
        ];
        let mut writer = Writer::default();
        for value in values {
            writer.number(value);
        }
        let mut reader = Reader::new(&writer.out);
        for value in values {
            let back = reader.number().unwrap();
            assert_eq!(back.to_bits(), value.to_bits(), "{value}");
        }
        assert_eq!(reader.at, writer.out.len());
    }

    #[test]
    fn rejects_damage_and_other_files() {
        let bytes = write(&Vmf::parse(SAMPLE).unwrap(), None);
        assert_eq!(read(SAMPLE.as_bytes()), Err(FormatError::NotMallet));
        let mut flipped = bytes.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 0xFF;
        assert_eq!(read(&flipped), Err(FormatError::Checksum(MAP_TAG)));
        assert!(matches!(read(&bytes[..bytes.len() - 5]), Err(FormatError::Damaged(_))));
        let mut newer = bytes.clone();
        newer[8] = 9;
        assert_eq!(read(&newer), Err(FormatError::NewerVersion(9)));
        assert_eq!(read(&bytes[..12]), Err(FormatError::NoMap));
    }

    #[test]
    fn skips_unknown_sections() {
        let vmf = Vmf::parse(SAMPLE).unwrap();
        let mut bytes = write(&vmf, None);
        write_section(&mut bytes, *b"NEW!", b"from the future", false);
        assert_eq!(read(&bytes).unwrap().vmf, vmf);
    }

    /// `MALLET_BIG_VMF=<path> cargo test --release -p mallet-format big -- --nocapture`
    /// compares the two formats on a real map.
    #[test]
    fn big_map_benchmark() {
        let Some(path) = std::env::var_os("MALLET_BIG_VMF") else {
            return;
        };
        let time = |what: &str, work: &mut dyn FnMut()| {
            let started = std::time::Instant::now();
            work();
            eprintln!("{what}: {:?}", started.elapsed());
        };
        let text = std::fs::read_to_string(path).unwrap();
        let mut vmf = Vmf::default();
        time("VMF open (parse)", &mut || vmf = Vmf::parse(&text).unwrap());
        let mut saved = String::new();
        time("VMF save (format)", &mut || saved = vmf.to_text());
        let mut bytes = Vec::new();
        time(".mallet save (encode)", &mut || bytes = write(&vmf, None));
        let mut back = MalletFile::default();
        time(".mallet open (decode)", &mut || back = read(&bytes).unwrap());
        eprintln!(
            "VMF {:.1} MB, .mallet {:.1} MB",
            saved.len() as f64 / 1e6,
            bytes.len() as f64 / 1e6
        );
        assert_eq!(back.vmf, vmf);
        assert_eq!(back.vmf.to_text(), saved);
    }
}
