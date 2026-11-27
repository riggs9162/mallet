//! A game's virtual file system, built from the search paths in its `gameinfo.txt` and,
//! for Garry's Mod, the other games listed in `cfg/mount.cfg`.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant, SystemTime};

use mallet_kv::{Block, Item};

use crate::vpk::{normalize, VpkArchive};
use crate::ContentError;

/// One mounted location, and which [`Source`] it belongs to.
#[derive(Debug)]
struct Mount {
    place: Place,
    source: usize,
}

#[derive(Debug)]
enum Place {
    Folder(Folder),
    Vpk(VpkArchive),
}

/// How long a directory that was not found in a mounted folder is taken to be missing
/// still. Short, so content added while Mallet runs shows up.
const MISSING_FOR: Duration = Duration::from_secs(5);

/// A mounted folder. It remembers which of its directories exist, so looking a file up in
/// the many folders that cannot hold it costs no disk access: Garry's Mod mounts a folder
/// for every addon, and most have no `materials` or `models` at all.
#[derive(Debug)]
struct Folder {
    root: PathBuf,
    /// The directories looked for so far, by path below `root`.
    directories: RwLock<HashMap<String, Known>>,
}

/// What a [`Folder`] found out about one of its directories.
#[derive(Debug, Clone, Copy)]
enum Known {
    Present,
    /// Not there when it was looked for at this time.
    Missing(Instant),
}

impl Folder {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            directories: RwLock::default(),
        }
    }

    /// Where a game file would lie in this folder, or `None` when the directory it would
    /// be in is not there. `path` is lower case with forward slashes.
    fn file(&self, path: &str) -> Option<PathBuf> {
        let directory = path.rsplit_once('/').map_or("", |(directory, _)| directory);
        let top = directory.split('/').next().unwrap_or_default();
        let found = directory.is_empty() || (self.has(top) && (top == directory || self.has(directory)));
        found.then(|| self.root.join(path))
    }

    /// Whether the folder has a directory, asking the disk only when that is not known
    /// yet or when it was last found missing more than [`MISSING_FOR`] ago.
    fn has(&self, directory: &str) -> bool {
        let known = self
            .directories
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(directory)
            .copied();
        match known {
            Some(Known::Present) => true,
            Some(Known::Missing(since)) if since.elapsed() < MISSING_FOR => false,
            _ => {
                let exists = self.root.join(directory).is_dir();
                let known = if exists {
                    Known::Present
                } else {
                    Known::Missing(Instant::now())
                };
                self.directories
                    .write()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(directory.to_owned(), known);
                exists
            }
        }
    }
}

/// What kind of content a source is, for grouping in the texture browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SourceKind {
    /// The game's own folders and VPKs.
    Game,
    /// Another game mounted through `cfg/mount.cfg`.
    MountedGame,
    /// A folder in `addons/`.
    Addon,
    /// Content downloaded from servers.
    Download,
}

/// A named group of mounts, such as `hl2_textures`, `cstrike` or one addon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub name: String,
    pub kind: SourceKind,
    /// The folder or archive it comes from.
    pub path: PathBuf,
}

/// Game files from every search path, looked up in priority order.
#[derive(Debug)]
pub struct GameFs {
    game_dir: PathBuf,
    mounts: Vec<Mount>,
    sources: Vec<Source>,
    /// Every loose file read so far, so [`GameFs::changed_files`] can tell when one is
    /// saved again while Mallet runs.
    watched: Mutex<HashMap<PathBuf, Watched>>,
}

/// When a loose file was last seen written, and how large it was then; `None` when it
/// was not there.
type Stamp = Option<(SystemTime, u64)>;

/// What [`GameFs`] knows about a loose file it has read.
#[derive(Debug)]
struct Watched {
    /// The game path it was read as, such as `materials/brick/brickwall001a.vtf`.
    path: String,
    stamp: Stamp,
    /// Set when the file changed at the last look. It is reported once a look finds it
    /// unchanged again, so a file still being written is not read half done.
    settling: bool,
}

/// The stamp of a file on disk now.
fn stamp_of(file: &Path) -> Stamp {
    let metadata = fs::metadata(file).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

impl GameFs {
    /// Mounts the `game` search paths of the `gameinfo.txt` in `game_dir`, such as
    /// `.../GarrysMod/garrysmod` or `.../Half-Life 2/hl2`, then every game in its
    /// `cfg/mount.cfg`, as Garry's Mod does.
    ///
    /// Search paths that do not exist are skipped, as the engine does. Paths ending in
    /// `.vpk` mount the matching `_dir.vpk`; paths ending in `/*` mount every subfolder.
    pub fn from_gameinfo(game_dir: &Path) -> Result<Self, ContentError> {
        Self::with_mounts(game_dir, &|_| true)
    }

    /// Like [`GameFs::from_gameinfo`], mounting only the `mount.cfg` games `mount` lets in.
    pub fn with_mounts(game_dir: &Path, mount: &dyn Fn(&str) -> bool) -> Result<Self, ContentError> {
        let gameinfo_path = game_dir.join("gameinfo.txt");
        if !gameinfo_path.is_file() {
            return Err(ContentError::NoGameInfo(game_dir.to_owned()));
        }
        let text = fs::read(&gameinfo_path).map_err(|error| ContentError::io(&gameinfo_path, error))?;
        let text = String::from_utf8_lossy(&text);
        let items = mallet_kv::parse(&text).map_err(|error| ContentError::Syntax {
            path: gameinfo_path.clone(),
            error,
        })?;
        let base_dir = game_dir.parent().unwrap_or(game_dir).to_owned();
        let mut filesystem = Self {
            game_dir: game_dir.to_owned(),
            mounts: Vec::new(),
            sources: Vec::new(),
            watched: Mutex::default(),
        };
        for value in search_paths(&items) {
            filesystem.mount_search_path(&value, game_dir, &base_dir);
        }
        for (name, folder) in mounted_games(game_dir) {
            if mount(&name) {
                filesystem.mount_game(&name, &folder);
            }
        }
        Ok(filesystem)
    }

    /// The folder holding `gameinfo.txt`.
    pub fn game_dir(&self) -> &Path {
        &self.game_dir
    }

    /// Short descriptions of every mount, in priority order.
    pub fn mounts(&self) -> Vec<String> {
        self.mounts
            .iter()
            .map(|mount| match &mount.place {
                Place::Folder(folder) => format!("folder {}", folder.root.display()),
                Place::Vpk(archive) => format!("vpk ({} files)", archive.len()),
            })
            .collect()
    }

    /// The sources mounts come from, indexed by the numbers [`GameFs::list_sourced`] gives.
    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    /// Whether any mount has a game file, without reading it.
    pub fn exists(&self, path: &str) -> bool {
        let path = normalize(path);
        self.mounts.iter().any(|mount| match &mount.place {
            Place::Folder(folder) => folder.file(&path).is_some_and(|file| file.is_file()),
            Place::Vpk(archive) => archive.contains(&path),
        })
    }

    /// Every game file under `folder` (such as `materials/`) with the given extension, from
    /// every mount, lower case with forward slashes, sorted and without repeats.
    pub fn list(&self, folder: &str, extension: &str) -> Vec<String> {
        self.list_sourced(folder, extension)
            .into_iter()
            .map(|(path, _)| path)
            .collect()
    }

    /// Like [`GameFs::list`], with every source that has each file. The first is the one
    /// it is read from, as for [`GameFs::read`]; the rest hold copies, such as Half-Life
    /// 2's own materials when Garry's Mod ships them too.
    pub fn list_sourced(&self, folder: &str, extension: &str) -> Vec<(String, Vec<usize>)> {
        let folder = normalize(folder);
        let folder = folder.trim_end_matches('/');
        let extension = extension.to_ascii_lowercase();
        let mut found: std::collections::BTreeMap<String, Vec<usize>> = std::collections::BTreeMap::new();
        let mut note = |path: String, source: usize| {
            let sources = found.entry(path).or_default();
            if !sources.contains(&source) {
                sources.push(source);
            }
        };
        for mount in &self.mounts {
            match &mount.place {
                Place::Vpk(archive) => {
                    for path in archive.paths_matching(folder, &extension) {
                        note(path, mount.source);
                    }
                }
                Place::Folder(mounted) => {
                    let root = &mounted.root;
                    let mut pending = vec![root.join(folder)];
                    while let Some(directory) = pending.pop() {
                        let Ok(children) = fs::read_dir(&directory) else {
                            continue;
                        };
                        for child in children.filter_map(Result::ok) {
                            let path = child.path();
                            // The entry's own type needs no extra query, unlike is_dir().
                            if child.file_type().is_ok_and(|kind| kind.is_dir()) {
                                pending.push(path);
                            } else if let Ok(relative) = path.strip_prefix(root) {
                                let relative = normalize(&relative.to_string_lossy());
                                if relative.ends_with(&extension) {
                                    note(relative, mount.source);
                                }
                            }
                        }
                    }
                }
            }
        }
        found.into_iter().collect()
    }

    /// Reads a game file such as `materials/tools/toolsnodraw.vmt` from the first mount
    /// that has it. Paths ignore case and may use either slash.
    pub fn read(&self, path: &str) -> Option<Vec<u8>> {
        let path = normalize(path);
        self.mounts.iter().find_map(|mount| match &mount.place {
            Place::Folder(folder) => folder.file(&path).and_then(|file| {
                let data = fs::read(&file).ok()?;
                self.watch(file, &path);
                Some(data)
            }),
            Place::Vpk(archive) => archive.read(&path).ok().flatten(),
        })
    }

    /// Opens a game file from the first mount that has it, to read only the parts of it
    /// that are needed. Paths ignore case and may use either slash.
    pub fn open(&self, path: &str) -> Option<GameFile<'_>> {
        let path = normalize(path);
        self.mounts.iter().find_map(|mount| match &mount.place {
            Place::Folder(folder) => folder.file(&path).and_then(|file| {
                let opened = fs::File::open(&file).ok()?;
                self.watch(file, &path);
                Some(GameFile::Loose(opened))
            }),
            Place::Vpk(archive) => archive.contains(&path).then(|| GameFile::Packed(archive, path.clone())),
        })
    }

    /// The source a game file is read from: the one of the first mount that has it.
    pub fn source_of(&self, path: &str) -> Option<&Source> {
        let path = normalize(path);
        self.mounts
            .iter()
            .find(|mount| match &mount.place {
                Place::Folder(folder) => folder.file(&path).is_some_and(|file| file.is_file()),
                Place::Vpk(archive) => archive.contains(&path),
            })
            .and_then(|mount| self.sources.get(mount.source))
    }

    /// Where a game file lies on disk, when it is a loose file rather than in a VPK.
    pub fn locate(&self, path: &str) -> Option<PathBuf> {
        let path = normalize(path);
        for mount in &self.mounts {
            match &mount.place {
                Place::Folder(folder) => {
                    if let Some(file) = folder.file(&path).filter(|file| file.is_file()) {
                        return Some(file);
                    }
                }
                Place::Vpk(archive) => {
                    if archive.contains(&path) {
                        return None;
                    }
                }
            }
        }
        None
    }

    /// Starts watching a loose file that was just read as game path `path`.
    fn watch(&self, file: PathBuf, path: &str) {
        let mut watched = self.watched.lock().unwrap_or_else(PoisonError::into_inner);
        if watched.contains_key(&file) {
            return;
        }
        let stamp = stamp_of(&file);
        watched.insert(
            file,
            Watched {
                path: path.to_owned(),
                stamp,
                settling: false,
            },
        );
    }

    /// The game paths of loose files that were saved, replaced or deleted since this was
    /// last asked, among those read so far. A file is reported once it has stopped
    /// changing between two calls, so call this every second or so. It asks the disk
    /// about every file read, so call it away from the window's thread.
    pub fn changed_files(&self) -> Vec<String> {
        let files: Vec<PathBuf> = self
            .watched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .keys()
            .cloned()
            .collect();
        let stamps: Vec<(PathBuf, Stamp)> = files
            .into_iter()
            .map(|file| {
                let stamp = stamp_of(&file);
                (file, stamp)
            })
            .collect();
        let mut watched = self.watched.lock().unwrap_or_else(PoisonError::into_inner);
        let mut changed = Vec::new();
        for (file, stamp) in stamps {
            let Some(entry) = watched.get_mut(&file) else {
                continue;
            };
            if entry.stamp != stamp {
                entry.stamp = stamp;
                entry.settling = true;
            } else if entry.settling {
                entry.settling = false;
                changed.push(entry.path.clone());
            }
        }
        changed.sort();
        changed.dedup();
        changed
    }

    fn mount_search_path(&mut self, value: &str, game_dir: &Path, base_dir: &Path) {
        let expanded = value
            .replace("|gameinfo_path|", &format!("{}/", game_dir.display()))
            .replace("|all_source_engine_paths|", &format!("{}/", base_dir.display()));
        let path = Path::new(&expanded);
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            base_dir.join(path)
        };
        let text = path.to_string_lossy().replace('\\', "/");

        if let Some(parent) = text.strip_suffix("/*") {
            let Ok(children) = fs::read_dir(parent) else {
                return;
            };
            let mut folders: Vec<PathBuf> = children
                .filter_map(Result::ok)
                .map(|child| child.path())
                .filter(|child| child.is_dir())
                .collect();
            folders.sort();
            let addons = Path::new(parent)
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("addons"));
            for folder in folders {
                let name = folder
                    .file_name()
                    .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                let kind = if addons { SourceKind::Addon } else { SourceKind::Game };
                let source = self.source(&name, kind, &folder);
                self.add(Place::Folder(Folder::new(folder)), source);
            }
        } else if let Some(stem) = text.strip_suffix(".vpk") {
            let dir_file = PathBuf::from(format!("{stem}_dir.vpk"));
            if let Ok(archive) = VpkArchive::open(&dir_file) {
                let name = Path::new(stem)
                    .file_name()
                    .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                let source = self.source(&name, SourceKind::Game, &dir_file);
                self.add(Place::Vpk(archive), source);
            }
        } else if path.is_dir() {
            let folder: PathBuf = path.components().collect();
            let name = folder
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            let kind = if name.eq_ignore_ascii_case("download") {
                SourceKind::Download
            } else {
                SourceKind::Game
            };
            let source = self.source(&name, kind, &folder);
            self.add(Place::Folder(Folder::new(folder)), source);
        }
    }

    /// Mounts another game's folder and every VPK in it, as Garry's Mod does for the games
    /// in `mount.cfg`.
    fn mount_game(&mut self, name: &str, folder: &Path) {
        if !folder.is_dir() {
            return;
        }
        let source = self.source(name, SourceKind::MountedGame, folder);
        self.add(Place::Folder(Folder::new(folder.components().collect())), source);
        let Ok(children) = fs::read_dir(folder) else { return };
        let mut archives: Vec<PathBuf> = children
            .filter_map(Result::ok)
            .map(|child| child.path())
            .filter(|path| path.to_string_lossy().to_ascii_lowercase().ends_with("_dir.vpk"))
            .collect();
        archives.sort();
        for archive in archives {
            if let Ok(archive) = VpkArchive::open(&archive) {
                self.add(Place::Vpk(archive), source);
            }
        }
    }

    /// The index of a source, added when new.
    fn source(&mut self, name: &str, kind: SourceKind, path: &Path) -> usize {
        if let Some(index) = self
            .sources
            .iter()
            .position(|source| source.name == name && source.kind == kind)
        {
            return index;
        }
        self.sources.push(Source {
            name: name.to_owned(),
            kind,
            path: path.to_owned(),
        });
        self.sources.len() - 1
    }

    fn add(&mut self, place: Place, source: usize) {
        if let Place::Folder(new) = &place {
            let duplicate = self.mounts.iter().any(
                |existing| matches!(&existing.place, Place::Folder(folder) if same_folder(&folder.root, &new.root)),
            );
            if duplicate {
                return;
            }
        }
        self.mounts.push(Mount { place, source });
    }
}

/// A game file opened with [`GameFs::open`], read a part at a time. A texture file holds
/// its picture at many sizes and a viewport shows only the small ones, so reading parts
/// keeps most of a large file off the disk and out of memory.
#[derive(Debug)]
pub enum GameFile<'a> {
    /// A loose file in a folder.
    Loose(fs::File),
    /// A file in a VPK archive, by its path there.
    Packed(&'a VpkArchive, String),
}

impl GameFile<'_> {
    /// Reads `length` bytes from `start`, or fewer when the file ends first.
    pub fn read(&self, start: u64, length: u64) -> Option<Vec<u8>> {
        match self {
            Self::Loose(file) => {
                let mut file: &fs::File = file;
                let size = file.metadata().ok()?.len();
                let mut data = Vec::with_capacity(size.saturating_sub(start).min(length) as usize);
                file.seek(SeekFrom::Start(start)).ok()?;
                file.take(length).read_to_end(&mut data).ok()?;
                Some(data)
            }
            Self::Packed(archive, path) => archive.read_range(path, start, length).ok().flatten(),
        }
    }
}

/// The games a Garry's Mod install mounts from `cfg/mount.cfg`, as (name, folder) pairs
/// in file order. Empty for other games.
pub fn mounted_games(game_dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(bytes) = fs::read(game_dir.join("cfg").join("mount.cfg")) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let Ok(items) = mallet_kv::parse(&text) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| match item {
            Item::Block(block) if block.name.eq_ignore_ascii_case("mountcfg") => Some(block),
            _ => None,
        })
        .flat_map(Block::pairs)
        .map(|pair| (pair.key.clone(), PathBuf::from(pair.value.trim())))
        .collect()
}

fn same_folder(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The values of `game` search paths (`game`, `game+mod` and so on), in file order.
fn search_paths(items: &[Item]) -> Vec<String> {
    let Some(gameinfo) = items.iter().find_map(|item| match item {
        Item::Block(block) if block.name.eq_ignore_ascii_case("GameInfo") => Some(block),
        _ => None,
    }) else {
        return Vec::new();
    };
    let search_paths: Option<&Block> = gameinfo
        .blocks_named("FileSystem")
        .next()
        .and_then(|filesystem| filesystem.blocks_named("SearchPaths").next());
    search_paths
        .into_iter()
        .flat_map(Block::pairs)
        .filter(|pair| pair.key.split('+').any(|kind| kind.eq_ignore_ascii_case("game")))
        .map(|pair| pair.value.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mounts_the_games_in_mount_cfg() {
        let Some(dir) = std::env::var_os("MALLET_GAME_DIR") else {
            return;
        };
        let dir = PathBuf::from(dir);
        let started = std::time::Instant::now();
        let alone = GameFs::with_mounts(&dir, &|_| false).unwrap();
        let alone_time = started.elapsed();
        let started = std::time::Instant::now();
        let all = GameFs::from_gameinfo(&dir).unwrap();
        let all_time = started.elapsed();
        let started = std::time::Instant::now();
        let materials = all.list_sourced("materials/", ".vmt");
        eprintln!(
            "game alone: {} mounts in {alone_time:?}; with mount.cfg: {} mounts, {} sources in {all_time:?}; {} materials listed in {:?}",
            alone.mounts().len(),
            all.mounts().len(),
            all.sources().len(),
            materials.len(),
            started.elapsed()
        );
        let games: Vec<&Source> = all
            .sources()
            .iter()
            .filter(|source| source.kind == SourceKind::MountedGame)
            .collect();
        eprintln!(
            "mounted games: {:?}",
            games.iter().map(|source| &source.name).collect::<Vec<_>>()
        );
        assert!(all.mounts().len() >= alone.mounts().len());
        for name in ["hl2", "hl2_misc", "cstrike"] {
            let Some(index) = all.sources().iter().position(|source| source.name == name) else {
                continue;
            };
            let total = materials.iter().filter(|(_, sources)| sources.contains(&index)).count();
            let loaded_from = materials
                .iter()
                .filter(|(_, sources)| sources.first() == Some(&index))
                .count();
            eprintln!("{name}: has {total} materials, {loaded_from} load from it");
        }
    }
}
