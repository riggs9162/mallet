//! Texture browser thumbnails kept on disk between runs, so a game's materials show again
//! without being read from slow drives. Each thumbnail remembers the size and write time
//! of the files it was made from and is made again when any of them has changed. The
//! cache lives in `%LOCALAPPDATA%`, as it is not worth roaming, and is trimmed to
//! [`MAX_BYTES`] once a run, oldest thumbnails first.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::thread;
use std::time::SystemTime;

use mallet_content::vtf::Image;
use mallet_content::MaterialLoader;

const MAGIC: &[u8; 8] = b"MTHUMB01";
/// How large the cache may grow before the oldest thumbnails are deleted.
const MAX_BYTES: u64 = 1 << 30;
/// What a trimmed cache is cut down to, so it is not trimmed again straight away.
const TRIMMED_BYTES: u64 = 768 << 20;
const RAW: u8 = 0;
const BC3: u8 = 1;

/// The thumbnails of one game at one size.
pub(super) struct ThumbnailCache {
    folder: PathBuf,
}

impl ThumbnailCache {
    /// The cache of `game`'s thumbnails at most `side` pixels across, or `None` when there
    /// is no folder to keep it in. Starts trimming the whole cache the first time.
    pub(super) fn new(game: &Path, side: u32) -> Option<Self> {
        let root = root()?;
        static TRIM: Once = Once::new();
        TRIM.call_once(|| {
            let root = root.clone();
            thread::spawn(move || trim(&root));
        });
        let game = game.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
        let folder = root.join(format!("{:016x}-{side}", fnv(game.as_bytes())));
        fs::create_dir_all(&folder).ok()?;
        Some(Self { folder })
    }

    /// The thumbnail of `material` kept from before, when every file it was made from is
    /// unchanged.
    pub(super) fn load(&self, loader: &MaterialLoader, material: &str) -> Option<Image> {
        let data = fs::read(self.file(material)).ok()?;
        let mut reader = Reader { data: &data, at: 0 };
        if reader.take(MAGIC.len())? != MAGIC {
            return None;
        }
        for _ in 0..reader.u32()? {
            let entry = std::str::from_utf8(reader.bytes()?).ok()?;
            let (path, version) = entry.split_once('\t')?;
            if loader.file_version(path).as_deref() != Some(version) {
                return None;
            }
        }
        let [width, height, pixel_width, pixel_height] =
            [reader.u32()?, reader.u32()?, reader.u32()?, reader.u32()?];
        let pixels = pixel_width as usize * pixel_height as usize * 4;
        let encoding = *reader.take(1)?.first()?;
        let stored = reader.bytes()?;
        let rgba = match encoding {
            RAW if stored.len() == pixels => stored.to_vec(),
            BC3 if stored.len() == texpresso::Format::Bc3.compressed_size(pixel_width as usize, pixel_height as usize) => {
                let mut rgba = vec![0; pixels];
                texpresso::Format::Bc3.decompress(stored, pixel_width as usize, pixel_height as usize, &mut rgba);
                rgba
            }
            _ => return None,
        };
        Some(Image {
            width,
            height,
            pixel_width,
            pixel_height,
            rgba,
        })
    }

    /// Keeps `image` as the thumbnail of `material`, with the versions of the files it is
    /// made from. Nothing is kept when one of them cannot be found.
    pub(super) fn store(&self, loader: &MaterialLoader, material: &str, image: &Image) {
        let files = loader.thumbnail_files(material);
        if files.is_empty() {
            return;
        }
        let Some(versions) = files
            .iter()
            .map(|path| Some(format!("{path}\t{}", loader.file_version(path)?)))
            .collect::<Option<Vec<String>>>()
        else {
            return;
        };
        let (width, height) = (image.pixel_width as usize, image.pixel_height as usize);
        if image.rgba.len() != width * height * 4 {
            return;
        }
        let mut data = MAGIC.to_vec();
        data.extend((versions.len() as u32).to_le_bytes());
        for version in &versions {
            push_bytes(&mut data, version.as_bytes());
        }
        for value in [image.width, image.height, image.pixel_width, image.pixel_height] {
            data.extend(value.to_le_bytes());
        }
        if width >= 4 && height >= 4 {
            let mut blocks = vec![0; texpresso::Format::Bc3.compressed_size(width, height)];
            let params = texpresso::Params {
                algorithm: texpresso::Algorithm::RangeFit,
                ..texpresso::Params::default()
            };
            texpresso::Format::Bc3.compress(&image.rgba, width, height, params, &mut blocks);
            data.push(BC3);
            push_bytes(&mut data, &blocks);
        } else {
            data.push(RAW);
            push_bytes(&mut data, &image.rgba);
        }
        let file = self.file(material);
        let partial = file.with_extension(format!("{}.part", thread_tag()));
        if fs::write(&partial, &data).is_ok() && fs::rename(&partial, &file).is_err() {
            let _ = fs::remove_file(&partial);
        }
    }

    fn file(&self, material: &str) -> PathBuf {
        self.folder.join(format!("{:016x}.thumb", fnv(material.as_bytes())))
    }
}

/// The folder every game's thumbnails are kept in.
fn root() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(
        PathBuf::from(local)
            .join(crate::config::app_folder())
            .join("thumbnails"),
    )
}

/// Deletes the oldest thumbnails while the cache is larger than [`MAX_BYTES`].
fn trim(root: &Path) {
    let mut files: Vec<(SystemTime, u64, PathBuf)> = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|folder| fs::read_dir(folder.path()).ok())
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            let written = metadata.modified().ok()?;
            Some((written, metadata.len(), entry.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|(_, length, _)| length).sum();
    if total <= MAX_BYTES {
        return;
    }
    files.sort();
    for (_, length, path) in files {
        if total <= TRIMMED_BYTES {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(length);
        }
    }
}

/// A name for the current thread, so two threads writing the same thumbnail do not
/// share a partial file.
fn thread_tag() -> String {
    format!("{:?}", thread::current().id())
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

/// The 64-bit FNV-1a hash, which stays the same between runs and Rust versions.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Appends `bytes` after their length.
fn push_bytes(data: &mut Vec<u8>, bytes: &[u8]) {
    data.extend((bytes.len() as u32).to_le_bytes());
    data.extend_from_slice(bytes);
}

/// Reads a cache file front to back.
struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let bytes = self.data.get(self.at..self.at.checked_add(length)?)?;
        self.at += length;
        Some(bytes)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    /// Bytes stored after their length, as [`push_bytes`] writes them.
    fn bytes(&mut self) -> Option<&'a [u8]> {
        let length = self.u32()? as usize;
        self.take(length)
    }
}
