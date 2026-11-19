//! Autosave: copies of maps with unsaved changes, kept in a folder of their own as Hammer
//! keeps them, and the marker each running Mallet leaves so that the next one can tell it
//! did not shut down correctly and offer those copies back.
//!
//! A copy is named `<map>_autosave_<number>` with the map's own extension, the highest
//! number being the newest. Nothing here touches a file that is not named that way, so
//! the folder may hold other files too.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config;
use crate::document::{DocumentError, MapFormat, Snapshot};
use crate::editor::{self, Editor};

/// The part of a copy's name between the map's name and the copy's number.
const MARK: &str = "_autosave_";
/// The extension of the file a running Mallet keeps locked.
const LOCK_EXTENSION: &str = "lock";
/// The extension of the file that lists a Mallet's copies.
const LIST_EXTENSION: &str = "json";
/// How often the autosave thread looks at the open maps.
const CHECK_EVERY: Duration = Duration::from_secs(2);
/// How old a copy that was never finished has to be before it is cleared away.
const STALE_TEMPORARY: Duration = Duration::from_secs(600);

/// What the autosave folder is kept within.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Copies kept of each map.
    pub copies: usize,
    /// Bytes all copies together may take.
    pub bytes: u64,
}

/// One map to copy. It is taken while the editor is locked and written after the lock is
/// released, so a large map does not hold the window up.
pub struct Job {
    /// The document the copy is of, as [`crate::document::Document::serial`] numbers it.
    pub document: u64,
    /// The map's file name, for messages.
    pub title: String,
    /// The map's file name without its extension, which the copy's name starts with.
    pub stem: String,
    pub format: MapFormat,
    /// How many changes the map had seen when the snapshot was taken.
    pub changes: u64,
    pub snapshot: Snapshot,
}

/// The copies one autosave makes, and where they go.
pub struct Pass {
    pub folder: PathBuf,
    pub limits: Limits,
    pub jobs: Vec<Job>,
}

/// What became of a [`Job`].
pub struct Outcome {
    pub document: u64,
    pub title: String,
    pub changes: u64,
    /// The copy that was written, or why it could not be.
    pub copy: Result<PathBuf, String>,
}

/// A copy found in the autosave folder.
struct FolderCopy {
    path: PathBuf,
    /// The name of the map it is of, in lower case.
    stem: String,
    number: u64,
    modified: SystemTime,
    bytes: u64,
}

/// The name of a copy: the map's name, the mark, the number and the format's extension.
fn copy_name(stem: &str, number: u64, format: MapFormat) -> String {
    format!("{stem}{MARK}{number:04}.{}", format.extension())
}

/// The map name and the number in a copy's file name, or `None` for any other file.
fn parse_copy_name(name: &str) -> Option<(&str, u64)> {
    let (base, extension) = name.rsplit_once('.')?;
    let is_map = [MapFormat::Vmf, MapFormat::Mallet]
        .iter()
        .any(|format| extension.eq_ignore_ascii_case(format.extension()));
    let (stem, number) = base.rsplit_once(MARK)?;
    let numbered = !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit());
    if !is_map || stem.is_empty() || !numbered {
        return None;
    }
    Some((stem, number.parse().ok()?))
}

/// A path as text that is the same however the path's case and slashes were written.
fn path_key(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\").to_lowercase()
}

/// Every finished copy in the autosave folder. A copy that is still empty is one a Mallet
/// is writing at this moment, or never finished, and is left out.
fn copies_in(folder: &Path) -> Vec<FolderCopy> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let (stem, number) = parse_copy_name(name.to_str()?)?;
            let metadata = entry
                .metadata()
                .ok()
                .filter(|metadata| metadata.is_file() && metadata.len() > 0)?;
            Some(FolderCopy {
                path: entry.path(),
                stem: stem.to_lowercase(),
                number,
                modified: metadata.modified().unwrap_or(UNIX_EPOCH),
                bytes: metadata.len(),
            })
        })
        .collect()
}

/// Writes one more copy of a map into the autosave folder, numbered after the map's newest
/// copy there, then clears away the copies past the limits. `write` puts the map into the
/// file it is given. `listed` holds the copies that must stay, as [`listed_copies`] gives
/// them. Returns the new copy's path.
pub fn write_copy(
    folder: &Path,
    stem: &str,
    format: MapFormat,
    limits: Limits,
    listed: &HashSet<String>,
    write: impl FnOnce(&Path) -> Result<(), DocumentError>,
) -> Result<PathBuf, String> {
    fs::create_dir_all(folder).map_err(|error| format!("could not create {}: {error}", folder.display()))?;
    let path =
        reserve(folder, stem, format).map_err(|error| format!("could not write to {}: {error}", folder.display()))?;
    if let Err(error) = write(&path) {
        let _ = fs::remove_file(&path);
        return Err(format!("could not write {}: {error}", path.display()));
    }
    prune(folder, limits, &path, listed);
    Ok(path)
}

/// Takes the number after a map's newest copy by creating that copy's file, still empty.
/// A file can only be created once, so two Mallets that autosave maps of the same name at
/// the same moment get different numbers instead of writing one copy over the other.
fn reserve(folder: &Path, stem: &str, format: MapFormat) -> io::Result<PathBuf> {
    let wanted = stem.to_lowercase();
    let newest = copies_in(folder)
        .iter()
        .filter(|copy| copy.stem == wanted)
        .map(|copy| copy.number)
        .max()
        .unwrap_or(0);
    for number in newest.saturating_add(1)..=u64::MAX {
        let path = folder.join(copy_name(stem, number, format));
        match File::options().write(true).create_new(true).open(&path) {
            Ok(_) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::ErrorKind::AlreadyExists.into())
}

/// Removes the copies past the limits: of each map all but its newest few, then the
/// oldest of any map until the rest fit in the space allowed. The copy just written and
/// the copies in `listed`, which a Mallet may still have to offer back, always stay.
fn prune(folder: &Path, limits: Limits, written: &Path, listed: &HashSet<String>) {
    let mut copies = copies_in(folder);
    copies.sort_by(|a, b| a.stem.cmp(&b.stem).then(b.number.cmp(&a.number)));
    let stays = |copy: &FolderCopy| copy.path == written || listed.contains(&path_key(&copy.path));
    let mut remaining = Vec::new();
    for map in copies.chunk_by(|a, b| a.stem == b.stem) {
        for (rank, copy) in map.iter().enumerate() {
            if rank >= limits.copies && !stays(copy) {
                let _ = fs::remove_file(&copy.path);
            } else {
                remaining.push(copy);
            }
        }
    }
    let mut total: u64 = remaining.iter().map(|copy| copy.bytes).sum();
    remaining.sort_by_key(|copy| copy.modified);
    for copy in remaining {
        if total <= limits.bytes {
            break;
        }
        if !stays(copy) && fs::remove_file(&copy.path).is_ok() {
            total -= copy.bytes;
        }
    }
    clear_unfinished_copies(folder);
}

/// Removes copies that were never finished, which a Mallet stopped in the middle of
/// writing one leaves behind: a copy's temporary file, or a copy that is still empty.
fn clear_unfinished_copies(folder: &Path) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let unfinished = match name.strip_suffix(".tmp") {
            Some(copy) => parse_copy_name(copy).is_some(),
            None => parse_copy_name(name).is_some() && metadata.len() == 0,
        };
        let old = metadata
            .modified()
            .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age >= STALE_TEMPORARY));
        if unfinished && old && metadata.is_file() {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// A copy a marker lists: the autosaved copy of a map that had unsaved changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListedMap {
    /// The map's file name, or `untitled`.
    pub title: String,
    /// Where the map itself is saved; `None` for a map that never was.
    pub original: Option<PathBuf>,
    /// The copy in the autosave folder.
    pub copy: PathBuf,
}

/// What a marker's list file holds.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Listing {
    maps: Vec<ListedMap>,
}

/// The marker of one running Mallet: a file it keeps locked for as long as it runs, and
/// beside it the list of the copies that hold its maps' unsaved changes. The system lets
/// go of the lock when the program ends, however it ends, so a marker whose lock can be
/// taken belongs to a Mallet that did not shut down correctly. One that does removes its
/// marker with [`Session::end`].
pub struct Session {
    lock: File,
    lock_path: PathBuf,
    list_path: PathBuf,
}

impl Session {
    /// Leaves this Mallet's marker in its settings folder. `None` when that cannot be
    /// done, as when the folder cannot be written to.
    pub fn start() -> Option<Self> {
        Self::start_in(&sessions_folder()?)
    }

    fn start_in(folder: &Path) -> Option<Self> {
        fs::create_dir_all(folder).ok()?;
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_millis());
        let name = format!("{}-{started}", std::process::id());
        let lock_path = folder.join(format!("{name}.{LOCK_EXTENSION}"));
        let lock = File::create(&lock_path).ok()?;
        lock.try_lock().ok()?;
        Some(Self {
            lock,
            list_path: folder.join(format!("{name}.{LIST_EXTENSION}")),
            lock_path,
        })
    }

    /// Replaces the list of copies. An empty list removes the list file.
    pub fn list(&self, maps: &[ListedMap]) -> io::Result<()> {
        if maps.is_empty() {
            return match fs::remove_file(&self.list_path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            };
        }
        let text = serde_json::to_string_pretty(&serde_json::json!({ "maps": maps }))?;
        let temporary = self.list_path.with_extension("tmp");
        fs::write(&temporary, text)?;
        fs::rename(&temporary, &self.list_path)
    }

    /// Removes the marker: this Mallet is shutting down correctly.
    pub fn end(self) {
        drop(self.lock);
        remove_marker(&self.lock_path, &self.list_path);
    }
}

/// A copy an earlier Mallet still listed when it stopped without shutting down correctly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftMap {
    pub map: ListedMap,
    /// When the copy was written.
    pub copied: SystemTime,
    /// Whether the map's own file was saved after the copy was written, which makes the
    /// copy the older of the two.
    pub saved_since: bool,
}

/// What an earlier Mallet that did not shut down correctly left behind. While this is
/// held no other Mallet offers the same copies, and [`Unfinished::forget`] removes the
/// marker once the user has decided about them.
pub struct Unfinished {
    pub maps: Vec<LeftMap>,
    lock: Option<File>,
    lock_path: PathBuf,
    list_path: PathBuf,
}

impl Unfinished {
    /// Removes the marker, so its copies are not offered again. The copies themselves
    /// stay in the autosave folder.
    pub fn forget(self) {
        drop(self.lock);
        remove_marker(&self.lock_path, &self.list_path);
    }
}

/// The markers of every Mallet that did not shut down correctly and whose listed copies
/// still exist. Markers with nothing left to offer are removed.
pub fn unfinished() -> Vec<Unfinished> {
    sessions_folder()
        .map(|folder| unfinished_in(&folder))
        .unwrap_or_default()
}

fn unfinished_in(folder: &Path) -> Vec<Unfinished> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut markers: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case(LOCK_EXTENSION) || extension.eq_ignore_ascii_case(LIST_EXTENSION)
            })
        })
        .map(|path| path.with_extension(""))
        .collect();
    markers.sort();
    markers.dedup();
    markers
        .into_iter()
        .filter_map(|marker| {
            let lock_path = marker.with_extension(LOCK_EXTENSION);
            let list_path = marker.with_extension(LIST_EXTENSION);
            let lock = claim(&lock_path)?;
            let maps = read_listing(&list_path).into_iter().filter_map(left_map).collect();
            let left = Unfinished {
                maps,
                lock,
                lock_path,
                list_path,
            };
            if left.maps.is_empty() {
                left.forget();
                return None;
            }
            Some(left)
        })
        .collect()
}

/// Takes the lock of a marker whose Mallet has gone. `None` while that Mallet still runs,
/// and `Some(None)` when the marker has no lock file left to take.
fn claim(lock_path: &Path) -> Option<Option<File>> {
    match File::options().read(true).write(true).open(lock_path) {
        Ok(file) => file.try_lock().is_ok().then_some(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Some(None),
        Err(_) => None,
    }
}

/// A listed copy that still exists, with when it was written.
fn left_map(map: ListedMap) -> Option<LeftMap> {
    let copied = fs::metadata(&map.copy).ok()?.modified().ok()?;
    let saved_since = map
        .original
        .as_deref()
        .and_then(|original| fs::metadata(original).ok()?.modified().ok())
        .is_some_and(|saved| saved > copied);
    Some(LeftMap {
        map,
        copied,
        saved_since,
    })
}

/// The copies a list file names; none when it is missing or cannot be read.
fn read_listing(list_path: &Path) -> Vec<ListedMap> {
    fs::read_to_string(list_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Listing>(&text).ok())
        .unwrap_or_default()
        .maps
}

/// Removes the files of a marker, with the list's own temporary file.
fn remove_marker(lock_path: &Path, list_path: &Path) {
    let _ = fs::remove_file(list_path);
    let _ = fs::remove_file(list_path.with_extension("tmp"));
    let _ = fs::remove_file(lock_path);
}

/// Every copy that the marker of any Mallet lists, running or not, as text that compares
/// equal however a path was written. These may still have to be offered back, so they are
/// never cleared away.
pub fn listed_copies() -> HashSet<String> {
    sessions_folder()
        .map(|folder| listed_copies_in(&folder))
        .unwrap_or_default()
}

fn listed_copies_in(folder: &Path) -> HashSet<String> {
    let Ok(entries) = fs::read_dir(folder) else {
        return HashSet::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case(LIST_EXTENSION))
        })
        .flat_map(|path| read_listing(&path))
        .map(|map| path_key(&map.copy))
        .collect()
}

/// Where the markers are kept: `sessions` in Mallet's settings folder.
fn sessions_folder() -> Option<PathBuf> {
    config::data_dir().map(|dir| dir.join("sessions"))
}

/// Writes the copies of one autosave.
pub fn run(pass: Pass) -> Vec<Outcome> {
    let listed = listed_copies();
    pass.jobs
        .into_iter()
        .map(|job| {
            let copy = write_copy(&pass.folder, &job.stem, job.format, pass.limits, &listed, |path| {
                job.snapshot.write(path)
            });
            Outcome {
                document: job.document,
                title: job.title,
                changes: job.changes,
                copy,
            }
        })
        .collect()
}

/// Starts the thread that autosaves. Every couple of seconds it asks the editor which
/// maps are due a copy, writes those with the editor unlocked and tells it what became of
/// them; `wake` is then called so the window shows the news. An error in one round does
/// not end the thread, as that would end autosaving without a word.
pub fn start(editor: Arc<Mutex<Editor>>, wake: impl Fn() + Send + 'static) {
    let spawned = thread::Builder::new().name("autosave".to_owned()).spawn(move || loop {
        thread::sleep(CHECK_EVERY);
        let _ = panic::catch_unwind(AssertUnwindSafe(|| {
            let Some(pass) = editor::lock(&editor).autosave_pass() else {
                return;
            };
            let outcomes = run(pass);
            editor::lock(&editor).autosave_done(outcomes);
            wake();
        }));
    });
    if let Err(error) = spawned {
        eprintln!("Autosave could not start: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOMY: Limits = Limits {
        copies: 3,
        bytes: u64::MAX,
    };

    fn temporary_folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("mallet-autosave-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        folder
    }

    fn write_bytes(count: usize) -> impl FnOnce(&Path) -> Result<(), DocumentError> {
        move |path: &Path| Ok(fs::write(path, vec![b'x'; count])?)
    }

    fn names(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn copy_names_are_told_from_other_files() {
        assert_eq!(copy_name("rp_city", 7, MapFormat::Vmf), "rp_city_autosave_0007.vmf");
        assert_eq!(parse_copy_name("rp_city_autosave_0007.vmf"), Some(("rp_city", 7)));
        assert_eq!(
            parse_copy_name("a_autosave_0001_autosave_12345.MALLET"),
            Some(("a_autosave_0001", 12345))
        );
        for other in [
            "rp_city.vmf",
            "rp_city_autosave_.vmf",
            "rp_city_autosave_7b.vmf",
            "_autosave_0001.vmf",
            "rp_city_autosave_0007.vmf.tmp",
            "rp_city_autosave_0007.txt",
        ] {
            assert_eq!(parse_copy_name(other), None, "{other}");
        }
    }

    #[test]
    fn copies_are_numbered_and_the_oldest_cleared_away() {
        let folder = temporary_folder("numbering");
        fs::write(folder.join("notes.txt"), "kept").unwrap();
        fs::write(folder.join("town_autosave_0001.vmf"), "another map").unwrap();
        let none = HashSet::new();
        for _ in 0..5 {
            write_copy(&folder, "City", MapFormat::Vmf, ROOMY, &none, write_bytes(10)).unwrap();
        }
        let newest = write_copy(&folder, "city", MapFormat::Mallet, ROOMY, &none, write_bytes(10)).unwrap();
        assert_eq!(newest, folder.join("city_autosave_0006.mallet"));
        assert_eq!(
            names(&folder),
            [
                "City_autosave_0004.vmf",
                "City_autosave_0005.vmf",
                "city_autosave_0006.mallet",
                "notes.txt",
                "town_autosave_0001.vmf",
            ]
        );
        let _ = fs::remove_dir_all(&folder);
    }

    #[test]
    fn two_mallets_never_write_the_same_copy() {
        let folder = temporary_folder("reserve");
        let first = reserve(&folder, "city", MapFormat::Vmf).unwrap();
        let second = reserve(&folder, "city", MapFormat::Vmf).unwrap();
        assert_ne!(first, second, "a copy being written elsewhere keeps its number");
        let refuse = |_: &Path| Err(DocumentError::Untitled);
        let failed = write_copy(&folder, "city", MapFormat::Vmf, ROOMY, &HashSet::new(), refuse);
        assert!(failed.is_err());
        assert_eq!(
            names(&folder),
            ["city_autosave_0001.vmf", "city_autosave_0002.vmf"],
            "a copy that could not be written leaves nothing behind"
        );
        let _ = fs::remove_dir_all(&folder);
    }

    #[test]
    fn listed_copies_outlive_the_limits() {
        let folder = temporary_folder("listed");
        let first = write_copy(&folder, "city", MapFormat::Vmf, ROOMY, &HashSet::new(), write_bytes(10)).unwrap();
        let listed = HashSet::from([path_key(&first)]);
        let tight = Limits { copies: 1, bytes: 15 };
        for _ in 0..3 {
            write_copy(&folder, "city", MapFormat::Vmf, tight, &listed, write_bytes(10)).unwrap();
        }
        assert_eq!(names(&folder), ["city_autosave_0001.vmf", "city_autosave_0004.vmf"]);
        let _ = fs::remove_dir_all(&folder);
    }

    #[test]
    fn the_oldest_copies_go_when_space_runs_out() {
        let folder = temporary_folder("space");
        let none = HashSet::new();
        let limits = Limits { copies: 5, bytes: 250 };
        for map in ["a", "b", "c"] {
            write_copy(&folder, map, MapFormat::Vmf, limits, &none, write_bytes(100)).unwrap();
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(names(&folder), ["b_autosave_0001.vmf", "c_autosave_0001.vmf"]);
        let large = write_copy(&folder, "d", MapFormat::Vmf, limits, &none, write_bytes(400)).unwrap();
        assert_eq!(
            names(&folder),
            ["d_autosave_0001.vmf"],
            "the copy just written always stays"
        );
        assert!(large.is_file());
        let _ = fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_marker_is_found_once_its_mallet_has_gone() {
        let folder = temporary_folder("marker");
        let copy = folder.join("city_autosave_0001.vmf");
        let gone = folder.join("gone_autosave_0001.vmf");
        fs::write(&copy, "map").unwrap();
        let session = Session::start_in(&folder).unwrap();
        let maps = [
            ListedMap {
                title: "city.vmf".to_owned(),
                original: Some(folder.join("city.vmf")),
                copy: copy.clone(),
            },
            ListedMap {
                title: "untitled".to_owned(),
                original: None,
                copy: gone,
            },
        ];
        session.list(&maps).unwrap();
        assert!(
            unfinished_in(&folder).is_empty(),
            "a running Mallet's marker is left alone"
        );
        assert_eq!(listed_copies_in(&folder).len(), 2);

        drop(session.lock);
        let found = unfinished_in(&folder);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].maps.len(), 1, "a copy that no longer exists is not offered");
        assert_eq!(found[0].maps[0].map, maps[0]);
        assert!(!found[0].maps[0].saved_since);
        assert!(
            unfinished_in(&folder).is_empty(),
            "a marker being offered is not offered twice"
        );
        for left in found {
            left.forget();
        }
        assert_eq!(names(&folder), ["city_autosave_0001.vmf"]);
        let _ = fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_mallet_that_shuts_down_leaves_no_marker() {
        let folder = temporary_folder("clean");
        let session = Session::start_in(&folder).unwrap();
        let listed = ListedMap {
            title: "city.vmf".to_owned(),
            original: None,
            copy: folder.join("city_autosave_0001.vmf"),
        };
        session.list(std::slice::from_ref(&listed)).unwrap();
        session.list(&[]).unwrap();
        assert_eq!(names(&folder).len(), 1, "an empty list leaves only the lock");
        session.list(&[listed]).unwrap();
        session.end();
        assert!(names(&folder).is_empty());
        let _ = fs::remove_dir_all(&folder);
    }
}
