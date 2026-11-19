//! A map file on disk, VMF or Mallet's own `.mallet`, together with its parsed map and
//! undo history.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use mallet_format::FormatError;
use mallet_vmf::edit::History;
use mallet_vmf::{Vmf, VmfError};
use serde::{Deserialize, Serialize};

/// The two kinds of map file Mallet reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapFormat {
    /// Hammer's text format, which the compilers and other editors read.
    Vmf,
    /// Mallet's binary format: the same map, several times faster to open and save and
    /// about a tenth of the size.
    Mallet,
}

impl MapFormat {
    /// The format a path's extension asks for: `.mallet`, or VMF for anything else.
    pub fn for_path(path: &Path) -> Self {
        let mallet = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case(mallet_format::EXTENSION));
        if mallet {
            Self::Mallet
        } else {
            Self::Vmf
        }
    }

    /// The file extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            Self::Vmf => "vmf",
            Self::Mallet => mallet_format::EXTENSION,
        }
    }

    /// The name file dialogs show.
    pub fn name(self) -> &'static str {
        match self {
            Self::Vmf => "Valve Map File",
            Self::Mallet => "Mallet Map",
        }
    }
}

/// How the file's bytes were decoded, so saving writes them back the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum TextEncoding {
    #[default]
    Utf8,
    /// Any file that is not valid UTF-8. Each byte maps to one character, so every byte
    /// survives a load and save unchanged.
    Latin1,
}

/// What a `.mallet` file keeps beside the map: the editor's own notes, as JSON.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct MalletMeta {
    /// The Mallet that wrote the file, for diagnosing problems.
    writer: String,
    /// How the map's text was encoded when it came from a VMF, so exporting it to a VMF
    /// again writes the same bytes.
    text_encoding: TextEncoding,
}

/// Why a document could not be loaded or saved.
#[derive(Debug)]
pub enum DocumentError {
    /// Reading or writing the file failed.
    Io(io::Error),
    /// The file is not a valid VMF.
    Vmf(VmfError),
    /// The file is a `.mallet` map that cannot be read.
    Mallet(FormatError),
    /// A new map has no file to save to yet.
    Untitled,
}

impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Vmf(error) => error.fmt(f),
            Self::Mallet(error) => error.fmt(f),
            Self::Untitled => write!(f, "the map has never been saved; choose where to save it"),
        }
    }
}

impl Error for DocumentError {}

impl From<io::Error> for DocumentError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<VmfError> for DocumentError {
    fn from(error: VmfError) -> Self {
        Self::Vmf(error)
    }
}

impl From<FormatError> for DocumentError {
    fn from(error: FormatError) -> Self {
        Self::Mallet(error)
    }
}

/// The autosaved copy of a map's unsaved changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    /// The copy, in the autosave folder.
    pub file: PathBuf,
    /// How many changes the map had seen when the copy was made, as
    /// [`History::changes`] counts them.
    pub changes: u64,
}

/// A map as it was at one moment, held as the bytes of a `.mallet` file, so it can be
/// written to disk later without the document it was taken from. Taking one is quicker
/// than copying the map in memory.
pub struct Snapshot {
    bytes: Vec<u8>,
}

impl Snapshot {
    /// Writes the map to `path`, in the format its extension names, and waits until it
    /// has reached the disk. The copy goes through a temporary file, like
    /// [`Document::export`].
    pub fn write(&self, path: &Path) -> Result<(), DocumentError> {
        match MapFormat::for_path(path) {
            MapFormat::Mallet => write_file(path, &self.bytes, true),
            MapFormat::Vmf => {
                let file = mallet_format::read(&self.bytes)?;
                let encoding = text_encoding(file.meta.as_deref());
                write_file(path, &encode(&file.vmf, encoding, MapFormat::Vmf), true)
            }
        }
    }
}

/// The number the next document gets as its [`Document::serial`].
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

/// An open map.
pub struct Document {
    /// Where the map is saved; `None` for a new map that has never been saved.
    pub path: Option<PathBuf>,
    /// The parsed map.
    pub vmf: Vmf,
    /// Undo and redo stacks for this map.
    pub history: History,
    /// Whether the map has changed since it was opened or last saved.
    pub dirty: bool,
    /// The latest autosaved copy of the map's unsaved changes, if one has been made.
    pub backup: Option<Backup>,
    encoding: TextEncoding,
    serial: u64,
}

impl Document {
    /// Reads and parses a map file. The format is told by the file's first bytes, not
    /// its extension, so a renamed file still opens.
    pub fn load(path: &Path) -> Result<Self, DocumentError> {
        let bytes = fs::read(path)?;
        let (vmf, encoding) = if mallet_format::is_mallet(&bytes) {
            let file = mallet_format::read(&bytes)?;
            let encoding = text_encoding(file.meta.as_deref());
            (file.vmf, encoding)
        } else {
            let (text, encoding) = match String::from_utf8(bytes) {
                Ok(text) => (text, TextEncoding::Utf8),
                Err(error) => (
                    error.into_bytes().iter().map(|&byte| char::from(byte)).collect(),
                    TextEncoding::Latin1,
                ),
            };
            (Vmf::parse(&text)?, encoding)
        };
        Ok(Self {
            path: Some(path.to_owned()),
            vmf,
            history: History::default(),
            dirty: false,
            backup: None,
            encoding,
            serial: NEXT_SERIAL.fetch_add(1, Ordering::Relaxed),
        })
    }

    /// A new, empty, unsaved map.
    pub fn untitled() -> Self {
        Self {
            path: None,
            vmf: Vmf::new_map(),
            history: History::default(),
            dirty: false,
            backup: None,
            encoding: TextEncoding::Utf8,
            serial: NEXT_SERIAL.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// Writes the map to [`Document::path`], in the format its extension names.
    pub fn save(&self) -> Result<(), DocumentError> {
        let path = self.path.as_ref().ok_or(DocumentError::Untitled)?;
        self.export(path)
    }

    /// Writes a copy of the map to `path`, in the format its extension names, without
    /// making it the document's file. The copy goes through a temporary file, so a failed
    /// write never leaves a half-written map behind.
    pub fn export(&self, path: &Path) -> Result<(), DocumentError> {
        let bytes = encode(&self.vmf, self.encoding, MapFormat::for_path(path));
        write_file(path, &bytes, false)
    }

    /// Writes a copy like [`Document::export`] and waits until it has reached the disk,
    /// for copies made to keep a map from being lost.
    pub fn export_durable(&self, path: &Path) -> Result<(), DocumentError> {
        let bytes = encode(&self.vmf, self.encoding, MapFormat::for_path(path));
        write_file(path, &bytes, true)
    }

    /// The map as it is now, to write to a file later.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            bytes: encode(&self.vmf, self.encoding, MapFormat::Mallet),
        }
    }

    /// A number no other map opened or started by this Mallet has, which tells the
    /// document apart from every other one while tabs open, close and change places.
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// Whether the map has unsaved changes that no autosaved copy holds yet.
    pub fn needs_backup(&self) -> bool {
        self.dirty
            && self
                .backup
                .as_ref()
                .is_none_or(|backup| backup.changes != self.history.changes())
    }

    /// The format the map is saved in.
    pub fn format(&self) -> MapFormat {
        self.path.as_deref().map_or(MapFormat::Vmf, MapFormat::for_path)
    }

    /// The file name shown in the title bar, or `untitled` for a new map.
    pub fn file_name(&self) -> String {
        match &self.path {
            Some(path) => path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
            None => "untitled".to_owned(),
        }
    }

    /// The full path for messages, or `untitled` for a new map.
    pub fn display_path(&self) -> String {
        self.path
            .as_ref()
            .map_or_else(|| "untitled".to_owned(), |path| path.display().to_string())
    }
}

/// The text encoding a `.mallet` file's notes name, or UTF-8 when it has none.
fn text_encoding(meta: Option<&str>) -> TextEncoding {
    meta.and_then(|meta| serde_json::from_str::<MalletMeta>(meta).ok())
        .unwrap_or_default()
        .text_encoding
}

/// A map as the bytes of a file in `format`.
fn encode(vmf: &Vmf, encoding: TextEncoding, format: MapFormat) -> Vec<u8> {
    match format {
        MapFormat::Mallet => {
            let meta = MalletMeta {
                writer: format!("Mallet {}", env!("CARGO_PKG_VERSION")),
                text_encoding: encoding,
            };
            let meta = serde_json::to_string(&meta).expect("the editor state is plain data");
            mallet_format::write(vmf, Some(&meta))
        }
        MapFormat::Vmf => {
            let text = vmf.to_text();
            match encoding {
                TextEncoding::Utf8 => text.into_bytes(),
                TextEncoding::Latin1 => text
                    .chars()
                    .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
                    .collect(),
            }
        }
    }
}

/// Writes a file through a temporary one beside it, so a failed write never leaves a
/// half-written map behind. With `durable` it also waits until the bytes are on the disk,
/// so the file is whole even when the computer loses power right after.
fn write_file(path: &Path, bytes: &[u8], durable: bool) -> Result<(), DocumentError> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    let mut file = fs::File::create(&temporary)?;
    file.write_all(bytes)?;
    if durable {
        file.sync_all()?;
    }
    drop(file);
    fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_writes_the_map_in_either_format() {
        let folder = std::env::temp_dir().join(format!("mallet-snapshot-{}", std::process::id()));
        fs::create_dir_all(&folder).unwrap();
        let text = Vmf::new_map().to_text().replace("sky_day01_01", "sky_caf\u{e9}");
        let bytes: Vec<u8> = text.chars().map(|c| u8::try_from(u32::from(c)).unwrap()).collect();
        let original = folder.join("map.vmf");
        fs::write(&original, &bytes).unwrap();
        let document = Document::load(&original).unwrap();
        let snapshot = document.snapshot();

        let as_vmf = folder.join("copy.vmf");
        snapshot.write(&as_vmf).unwrap();
        assert_eq!(fs::read(&as_vmf).unwrap(), bytes, "a VMF copy has the map's own bytes");
        let as_mallet = folder.join("copy.mallet");
        snapshot.write(&as_mallet).unwrap();
        assert_eq!(Document::load(&as_mallet).unwrap().vmf, document.vmf);
        assert!(!folder.join("copy.vmf.tmp").exists());
        let _ = fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_map_needs_a_backup_until_a_copy_holds_its_changes() {
        let mut document = Document::untitled();
        assert_ne!(document.serial(), Document::untitled().serial());
        assert!(!document.needs_backup(), "a map without unsaved changes needs none");
        document.dirty = true;
        assert!(document.needs_backup());
        document.backup = Some(Backup {
            file: PathBuf::from("copy.vmf"),
            changes: document.history.changes(),
        });
        assert!(!document.needs_backup());
        let world = document.vmf.world().and_then(mallet_vmf::Entity::id).unwrap();
        let edit = mallet_vmf::edit::Edit::SetKey {
            entity: world,
            key: "skyname".to_owned(),
            value: "sky_day02_01".to_owned(),
        };
        document.history.apply(&mut document.vmf, &edit).unwrap();
        assert!(document.needs_backup(), "a change after the copy needs a new one");
    }

    #[test]
    fn converts_between_formats_without_losing_bytes() {
        let folder = std::env::temp_dir().join(format!("mallet-document-{}", std::process::id()));
        fs::create_dir_all(&folder).unwrap();
        // Not valid UTF-8, so the map is read as Latin-1 and must come back byte for byte.
        let text = Vmf::new_map().to_text().replace("sky_day01_01", "sky_caf\u{e9}");
        let bytes: Vec<u8> = text.chars().map(|c| u8::try_from(u32::from(c)).unwrap()).collect();
        assert!(String::from_utf8(bytes.clone()).is_err());
        let original = folder.join("map.vmf");
        fs::write(&original, &bytes).unwrap();

        let mut document = Document::load(&original).unwrap();
        let binary = folder.join("map.mallet");
        document.path = Some(binary.clone());
        document.save().unwrap();
        assert_eq!(document.format(), MapFormat::Mallet);
        assert!(mallet_format::is_mallet(&fs::read(&binary).unwrap()));

        // A `.mallet` file opens by its contents, even under another name.
        let renamed = folder.join("renamed.vmf");
        fs::copy(&binary, &renamed).unwrap();
        let reopened = Document::load(&renamed).unwrap();
        assert_eq!(reopened.vmf, document.vmf);
        let exported = folder.join("export.vmf");
        reopened.export(&exported).unwrap();
        assert_eq!(fs::read(&exported).unwrap(), bytes);
        let _ = fs::remove_dir_all(&folder);
    }
}
