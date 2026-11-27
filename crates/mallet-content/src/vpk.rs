//! Valve Pak (VPK) archives, versions 1 and 2.
//!
//! An archive is a `*_dir.vpk` file holding the directory tree, plus numbered `*_000.vpk`
//! files holding most of the data. Only the tree is read when opening; file data is read
//! on demand.
//!
//! A game with its mounted games lists hundreds of thousands of files, so the tree is kept
//! compactly: folders sorted by path, each with its files sorted by name, and all the
//! names in one string. A lookup is two binary searches and no path is stored twice.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::ContentError;

const SIGNATURE: u32 = 0x55AA_1234;
/// Archive index meaning the data follows the tree inside the `_dir.vpk` file itself.
const IN_DIRECTORY_FILE: u16 = 0x7FFF;
const ENTRY_TERMINATOR: u16 = 0xFFFF;

/// One file of an archive.
#[derive(Debug, Clone, Copy)]
struct Entry {
    /// Where the file's name, with its extension, starts in [`VpkArchive::names`].
    name: u32,
    name_length: u16,
    archive: u16,
    offset: u32,
    length: u32,
    /// Where the file's preloaded bytes start, counted from the start of the tree.
    preload: u32,
    preload_length: u16,
}

/// One folder of an archive.
#[derive(Debug)]
struct Folder {
    /// Lower case with forward slashes; empty for the archive's root.
    path: Box<str>,
    /// The folder's files in [`VpkArchive::files`].
    files: Range<u32>,
}

/// An opened VPK archive.
#[derive(Debug)]
pub struct VpkArchive {
    dir_path: PathBuf,
    /// Where the directory tree starts in the `_dir.vpk` file.
    tree_start: u64,
    /// Where data stored in the `_dir.vpk` file starts.
    data_start: u64,
    /// Every folder, sorted by path.
    folders: Vec<Folder>,
    /// Every file, folder by folder, sorted by name within its folder.
    files: Vec<Entry>,
    /// The names of all files, lower case and back to back.
    names: String,
}

impl VpkArchive {
    /// The archive's `_dir.vpk` file.
    pub fn dir_path(&self) -> &Path {
        &self.dir_path
    }

    /// Opens an archive from its `_dir.vpk` file and reads its directory tree.
    pub fn open(dir_path: &Path) -> Result<Self, ContentError> {
        let io_error = |error| ContentError::io(dir_path, error);
        let mut reader = BufReader::new(File::open(dir_path).map_err(io_error)?);
        let signature = read_u32(&mut reader).map_err(io_error)?;
        if signature != SIGNATURE {
            return Err(ContentError::format(dir_path, "not a VPK directory file"));
        }
        let version = read_u32(&mut reader).map_err(io_error)?;
        let tree_size = read_u32(&mut reader).map_err(io_error)?;
        let header_size: u64 = match version {
            1 => 12,
            2 => {
                let mut rest = [0u8; 16];
                reader.read_exact(&mut rest).map_err(io_error)?;
                28
            }
            other => {
                return Err(ContentError::format(
                    dir_path,
                    format!("unsupported VPK version {other}"),
                ))
            }
        };

        let mut tree = vec![0u8; tree_size as usize];
        reader.read_exact(&mut tree).map_err(io_error)?;
        let (folders, files, names) =
            parse_tree(&tree).ok_or_else(|| ContentError::format(dir_path, "corrupt VPK tree"))?;
        Ok(Self {
            dir_path: dir_path.to_owned(),
            tree_start: header_size,
            data_start: header_size + u64::from(tree_size),
            folders,
            files,
            names,
        })
    }

    /// Number of files in the archive.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether the archive holds no files.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Every file path in the archive that starts with `prefix` and ends with `suffix`,
    /// lower case with forward slashes.
    pub fn paths_matching<'a>(&'a self, prefix: &'a str, suffix: &'a str) -> impl Iterator<Item = String> + 'a {
        self.folders
            .iter()
            .filter(move |folder| folder.path.starts_with(prefix) || prefix.starts_with(&*folder.path))
            .flat_map(move |folder| {
                self.files_of(folder).iter().filter_map(move |entry| {
                    let name = self.name(entry);
                    if !name.ends_with(suffix) && suffix.len() <= name.len() {
                        return None;
                    }
                    let path = if folder.path.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{}/{name}", folder.path)
                    };
                    (path.starts_with(prefix) && path.ends_with(suffix)).then_some(path)
                })
            })
    }

    /// Whether the archive holds a file. Paths use forward slashes and ignore case.
    pub fn contains(&self, path: &str) -> bool {
        self.find(path).is_some()
    }

    /// Reads a file, or returns `None` when the archive does not hold it.
    pub fn read(&self, path: &str) -> Result<Option<Vec<u8>>, ContentError> {
        self.read_range(path, 0, u64::MAX)
    }

    /// Reads `length` bytes of a file from `start`, or fewer when the file ends first, or
    /// returns `None` when the archive does not hold it. A file is its preloaded bytes in
    /// the directory file followed by the rest in a data file.
    pub fn read_range(&self, path: &str, start: u64, length: u64) -> Result<Option<Vec<u8>>, ContentError> {
        let Some(entry) = self.find(path) else {
            return Ok(None);
        };
        let preload = u64::from(entry.preload_length);
        let end = start.saturating_add(length).min(preload + u64::from(entry.length));
        let mut data = Vec::with_capacity(end.saturating_sub(start) as usize);
        let preload_end = end.min(preload);
        if start < preload_end {
            read_range(
                &self.dir_path,
                self.tree_start + u64::from(entry.preload) + start,
                preload_end - start,
                &mut data,
            )?;
        }
        let body_start = start.max(preload);
        if body_start < end {
            let (path, base) = if entry.archive == IN_DIRECTORY_FILE {
                (self.dir_path.clone(), self.data_start + u64::from(entry.offset))
            } else {
                (self.archive_path(entry.archive), u64::from(entry.offset))
            };
            read_range(&path, base + body_start - preload, end - body_start, &mut data)?;
        }
        Ok(Some(data))
    }

    /// Looks a file up by its path: first its folder, then its name among the folder's.
    fn find(&self, path: &str) -> Option<&Entry> {
        let path = normalized(path);
        let (folder, name) = path.rsplit_once('/').unwrap_or(("", &path));
        let at = self
            .folders
            .binary_search_by(|candidate| (*candidate.path).cmp(folder))
            .ok()?;
        let files = self.files_of(&self.folders[at]);
        let at = files.binary_search_by(|entry| self.name(entry).cmp(name)).ok()?;
        files.get(at)
    }

    fn files_of(&self, folder: &Folder) -> &[Entry] {
        &self.files[folder.files.start as usize..folder.files.end as usize]
    }

    /// A file's name with its extension.
    fn name(&self, entry: &Entry) -> &str {
        let start = entry.name as usize;
        &self.names[start..start + usize::from(entry.name_length)]
    }

    fn archive_path(&self, index: u16) -> PathBuf {
        let name = self
            .dir_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = name.strip_suffix("_dir.vpk").unwrap_or(&name);
        self.dir_path.with_file_name(format!("{stem}_{index:03}.vpk"))
    }
}

/// Lower-cases a path and uses forward slashes, the form VPK trees store.
pub fn normalize(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches('/').to_ascii_lowercase()
}

/// A path as [`normalize`] gives it, without copying one that is in that form already.
fn normalized(path: &str) -> Cow<'_, str> {
    let plain = !path.starts_with('/') && !path.bytes().any(|byte| byte == b'\\' || byte.is_ascii_uppercase());
    if plain {
        Cow::Borrowed(path)
    } else {
        Cow::Owned(normalize(path))
    }
}

fn read_range(path: &Path, start: u64, length: u64, out: &mut Vec<u8>) -> Result<(), ContentError> {
    let io_error = |error| ContentError::io(path, error);
    let mut file = File::open(path).map_err(io_error)?;
    file.seek(SeekFrom::Start(start)).map_err(io_error)?;
    file.take(length).read_to_end(out).map_err(io_error)?;
    Ok(())
}

fn read_u32(reader: &mut impl Read) -> std::io::Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

/// Walks the extension, folder, file-name nesting of a VPK tree, and gives its folders
/// sorted by path, their files sorted by name, and the string holding the names.
fn parse_tree(tree: &[u8]) -> Option<(Vec<Folder>, Vec<Entry>, String)> {
    let mut cursor = Cursor { data: tree, pos: 0 };
    let mut names = String::new();
    let mut paths: Vec<String> = Vec::new();
    let mut numbers: HashMap<String, u32> = HashMap::new();
    let mut found: Vec<(u32, Entry)> = Vec::new();
    loop {
        let extension = cursor.string()?;
        if extension.is_empty() {
            break;
        }
        loop {
            let folder = cursor.string()?;
            if folder.is_empty() {
                break;
            }
            let path = if folder == " " {
                String::new()
            } else {
                normalize(folder)
            };
            let number = match numbers.get(&path) {
                Some(number) => *number,
                None => {
                    let number = u32::try_from(paths.len()).ok()?;
                    paths.push(path.clone());
                    numbers.insert(path, number);
                    number
                }
            };
            loop {
                let name = cursor.string()?;
                if name.is_empty() {
                    break;
                }
                let _crc = cursor.u32()?;
                let preload_length = cursor.u16()?;
                let archive = cursor.u16()?;
                let offset = cursor.u32()?;
                let length = cursor.u32()?;
                if cursor.u16()? != ENTRY_TERMINATOR {
                    return None;
                }
                let preload = u32::try_from(cursor.pos).ok()?;
                cursor.skip(usize::from(preload_length))?;
                let start = names.len();
                names.push_str(name);
                if extension != " " {
                    names.push('.');
                    names.push_str(extension);
                }
                names[start..].make_ascii_lowercase();
                found.push((
                    number,
                    Entry {
                        name: u32::try_from(start).ok()?,
                        name_length: u16::try_from(names.len() - start).ok()?,
                        archive,
                        offset,
                        length,
                        preload,
                        preload_length,
                    },
                ));
            }
        }
    }

    let name_of = |entry: &Entry| {
        let start = entry.name as usize;
        &names[start..start + usize::from(entry.name_length)]
    };
    found.sort_unstable_by(|(a_folder, a), (b_folder, b)| {
        paths[*a_folder as usize]
            .cmp(&paths[*b_folder as usize])
            .then_with(|| name_of(a).cmp(name_of(b)))
    });
    let mut folders: Vec<Folder> = Vec::new();
    let mut last = None;
    for (at, (number, _)) in found.iter().enumerate() {
        let at = at as u32;
        if last != Some(*number) {
            last = Some(*number);
            folders.push(Folder {
                path: paths[*number as usize].as_str().into(),
                files: at..at,
            });
        }
        if let Some(folder) = folders.last_mut() {
            folder.files.end = at + 1;
        }
    }
    let files = found.into_iter().map(|(_, entry)| entry).collect();
    names.shrink_to_fit();
    Some((folders, files, names))
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn string(&mut self) -> Option<&'a str> {
        let length = self.data.get(self.pos..)?.iter().position(|&byte| byte == 0)?;
        let text = std::str::from_utf8(&self.data[self.pos..self.pos + length]).ok()?;
        self.pos += length + 1;
        Some(text)
    }

    fn bytes<const N: usize>(&mut self) -> Option<[u8; N]> {
        let bytes = self.data.get(self.pos..self.pos + N)?.try_into().ok()?;
        self.pos += N;
        Some(bytes)
    }

    fn u16(&mut self) -> Option<u16> {
        self.bytes().map(u16::from_le_bytes)
    }

    fn u32(&mut self) -> Option<u32> {
        self.bytes().map(u32::from_le_bytes)
    }

    fn skip(&mut self, count: usize) -> Option<()> {
        (self.pos + count <= self.data.len()).then(|| self.pos += count)
    }
}
