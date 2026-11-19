//! Autosave from the editor's side: which open maps are due a copy, what became of the
//! copies, and opening a copy again as the map it was made from.

use std::time::{Duration, Instant};

use super::Editor;
use crate::autosave::{Job, Limits, ListedMap, Outcome, Pass, Session};
use crate::document::{Backup, Document, MapFormat};

/// How long an autosave that is due waits for a drag or mouselook to end before it goes
/// ahead anyway.
const HOLD_LIMIT: Duration = Duration::from_secs(30);

/// What the editor keeps about autosaving.
#[derive(Default)]
pub struct AutosaveState {
    /// Whether a mouse button is held in the window, as during a drag. An autosave that
    /// is due waits for it to be let go, so the drag stays smooth.
    pub pointer_held: bool,
    /// This Mallet's marker, which lists the copies that hold unsaved changes.
    session: Option<Session>,
    /// The copies the marker lists now.
    listed: Vec<ListedMap>,
    /// When the open maps were last copied, or when the wait for the first copy began.
    last_pass: Option<Instant>,
}

/// The name a map's copies start with: its file name without the extension.
fn copy_stem(document: &Document) -> String {
    document
        .path
        .as_deref()
        .and_then(|path| path.file_stem())
        .map_or_else(|| "untitled".to_owned(), |stem| stem.to_string_lossy().into_owned())
}

/// The format a map's copies are written in: its own, or `untitled` for a map that has
/// never been saved.
fn copy_format(document: &Document, untitled: MapFormat) -> MapFormat {
    match document.path {
        Some(_) => document.format(),
        None => untitled,
    }
}

impl Editor {
    /// Every open map: the one being edited and those in the other tabs.
    fn documents(&self) -> impl Iterator<Item = &Document> {
        let parked = self.tabs.iter().flatten().filter_map(|map| map.document.as_ref());
        self.document.iter().chain(parked)
    }

    fn documents_mut(&mut self) -> impl Iterator<Item = &mut Document> {
        let parked = self.tabs.iter_mut().flatten().filter_map(|map| map.document.as_mut());
        self.document.iter_mut().chain(parked)
    }

    /// The format copies of maps that have never been saved are written in: the one Save
    /// As offers first for them.
    fn untitled_format(&self) -> MapFormat {
        if self.config.files.new_maps_as_mallet {
            MapFormat::Mallet
        } else {
            MapFormat::Vmf
        }
    }

    /// The limits Mallet Options sets for the autosave folder.
    fn autosave_limits(&self) -> Limits {
        Limits {
            copies: self.config.autosave.copy_count(),
            bytes: self.config.autosave.byte_limit(),
        }
    }

    /// Leaves the marker that lets a later Mallet tell whether this one shut down
    /// correctly.
    pub fn begin_session(&mut self) {
        self.autosave.session = Session::start();
    }

    /// Removes the marker, as Mallet is shutting down correctly.
    pub fn end_session(&mut self) {
        if let Some(session) = self.autosave.session.take() {
            session.end();
        }
    }

    /// The copies an autosave is due to make now, if any: one of every map with unsaved
    /// changes that its latest copy does not hold, once the time between autosaves has
    /// passed. A drag or mouselook puts it off for a while. The copies are snapshots, to
    /// be written with [`crate::autosave::run`] once the editor is unlocked and reported back
    /// with [`Editor::autosave_done`].
    pub fn autosave_pass(&mut self) -> Option<Pass> {
        self.list_backups();
        let options = self.config.autosave.clone();
        if !options.enabled {
            self.autosave.last_pass = None;
            return None;
        }
        let now = Instant::now();
        let waited = now.duration_since(*self.autosave.last_pass.get_or_insert(now));
        let held = self.autosave.pointer_held || self.camera.mouse_look();
        if waited < options.interval() || (held && waited < options.interval() + HOLD_LIMIT) {
            return None;
        }
        let folder = options.folder()?;
        self.autosave.last_pass = Some(now);
        if self.document.as_ref().is_some_and(Document::needs_backup) {
            let _ = self.store_view();
        }
        let untitled = self.untitled_format();
        let jobs: Vec<Job> = self
            .documents()
            .filter(|document| document.needs_backup())
            .map(|document| Job {
                document: document.serial(),
                title: document.file_name(),
                stem: copy_stem(document),
                format: copy_format(document, untitled),
                changes: document.history.changes(),
                snapshot: document.snapshot(),
            })
            .collect();
        (!jobs.is_empty()).then(|| Pass {
            folder,
            limits: self.autosave_limits(),
            jobs,
        })
    }

    /// Records what became of an autosave's copies: each map remembers its copy, the
    /// marker lists it, and the status bar says what was copied or what went wrong.
    pub fn autosave_done(&mut self, outcomes: Vec<Outcome>) {
        let mut copied = Vec::new();
        let mut failure = None;
        for outcome in outcomes {
            match outcome.copy {
                Ok(file) => {
                    let document = self
                        .documents_mut()
                        .find(|document| document.serial() == outcome.document && document.dirty);
                    if let Some(document) = document {
                        document.backup = Some(Backup {
                            file,
                            changes: outcome.changes,
                        });
                    }
                    copied.push(outcome.title);
                }
                Err(error) => failure = Some(format!("Could not autosave {}: {error}", outcome.title)),
            }
        }
        self.list_backups();
        self.message = match (failure, copied.as_slice()) {
            (Some(failure), _) => failure,
            (None, []) => return,
            (None, [title]) => format!("Autosaved {title}"),
            (None, titles) => format!("Autosaved {} maps", titles.len()),
        };
    }

    /// Brings the marker's list up to the copies that hold unsaved changes now. Saving or
    /// closing a map takes its copy off the list.
    fn list_backups(&mut self) {
        let maps: Vec<ListedMap> = self
            .documents()
            .filter(|document| document.dirty)
            .filter_map(|document| {
                Some(ListedMap {
                    title: document.file_name(),
                    original: document.path.clone(),
                    copy: document.backup.as_ref()?.file.clone(),
                })
            })
            .collect();
        if maps == self.autosave.listed {
            return;
        }
        let Some(session) = &self.autosave.session else {
            return;
        };
        if session.list(&maps).is_ok() {
            self.autosave.listed = maps;
        }
    }

    /// Opens an autosaved copy as the map it was made from: under the map's own path, so
    /// saving writes there, and with unsaved changes, as the map's file has not been
    /// touched. A tab that shows the same map without changes of its own makes way for
    /// it; otherwise it opens in a tab of its own.
    pub fn restore_copy(&mut self, map: &ListedMap) -> Result<(), String> {
        let mut document = match Document::load(&map.copy) {
            Ok(document) => document,
            Err(error) => {
                self.message = format!("Could not restore {} from {}: {error}", map.title, map.copy.display());
                return Err(self.message.clone());
            }
        };
        document.path = map.original.clone();
        document.dirty = true;
        document.backup = Some(Backup {
            file: map.copy.clone(),
            changes: document.history.changes(),
        });
        let showing = map.original.as_deref().and_then(|original| self.tab_of_path(original));
        match showing {
            Some(index) if !self.tabs()[index].dirty => self.bring_to_front(index),
            _ if self.tab_is_blank() => {}
            _ => self.new_tab(),
        }
        self.show_document(document);
        self.message = format!("Restored {} from its autosaved copy", map.title);
        self.list_backups();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use glam::DVec3;

    use super::*;
    use crate::autosave;

    fn editor_saving_to(name: &str) -> (Editor, PathBuf) {
        let folder = std::env::temp_dir().join(format!("mallet-editor-autosave-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        let mut editor = Editor::default();
        editor.config.autosave.folder = Some(folder.join("autosave"));
        editor.config.autosave.minutes = 1;
        editor.new_map();
        (editor, folder)
    }

    fn add_block(editor: &mut Editor) {
        editor.create_block(DVec3::splat(-64.0), DVec3::splat(64.0)).unwrap();
    }

    fn make_due(editor: &mut Editor) {
        editor.autosave.last_pass = Instant::now().checked_sub(Duration::from_secs(61));
    }

    fn copies(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder.join("autosave"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn maps_with_unsaved_changes_are_copied_when_the_time_has_passed() {
        let (mut editor, folder) = editor_saving_to("pass");
        assert!(editor.autosave_pass().is_none(), "a map without changes needs no copy");
        add_block(&mut editor);
        assert!(
            editor.autosave_pass().is_none(),
            "the time between autosaves has not passed"
        );
        make_due(&mut editor);
        editor.autosave.pointer_held = true;
        assert!(editor.autosave_pass().is_none(), "a drag puts the autosave off");
        editor.autosave.pointer_held = false;
        let pass = editor.autosave_pass().expect("the map has unsaved changes");
        assert_eq!(pass.jobs.len(), 1);
        editor.autosave_done(autosave::run(pass));
        assert_eq!(editor.message, "Autosaved untitled");
        assert_eq!(copies(&folder), ["untitled_autosave_0001.mallet"]);
        make_due(&mut editor);
        assert!(editor.autosave_pass().is_none(), "the copy already holds every change");

        add_block(&mut editor);
        editor.new_tab();
        add_block(&mut editor);
        make_due(&mut editor);
        let pass = editor.autosave_pass().expect("two maps have changes without a copy");
        assert_eq!(pass.jobs.len(), 2);
        editor.autosave_done(autosave::run(pass));
        assert_eq!(editor.message, "Autosaved 2 maps");
        assert_eq!(copies(&folder).len(), 3);
        let _ = fs::remove_dir_all(&folder);
    }

    #[test]
    fn autosaving_can_be_turned_off() {
        let (mut editor, folder) = editor_saving_to("off");
        editor.config.autosave.enabled = false;
        add_block(&mut editor);
        make_due(&mut editor);
        assert!(editor.autosave_pass().is_none());
        assert!(copies(&folder).is_empty());
        let _ = fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_copy_comes_back_as_the_map_it_was_made_from() {
        let (mut editor, folder) = editor_saving_to("restore");
        let map = folder.join("city.vmf");
        let document = editor.document.as_mut().unwrap();
        document.path = Some(map.clone());
        document.save().unwrap();
        add_block(&mut editor);
        let solids = editor.scene.solid_count;
        make_due(&mut editor);
        let pass = editor.autosave_pass().expect("the map has unsaved changes");
        editor.autosave_done(autosave::run(pass));
        let copy = folder.join("autosave").join("city_autosave_0001.vmf");
        assert_eq!(editor.tabs()[0].backup.as_ref(), Some(&copy));

        let mut editor = Editor::default();
        editor.new_map();
        let listed = ListedMap {
            title: "city.vmf".to_owned(),
            original: Some(map.clone()),
            copy,
        };
        editor.restore_copy(&listed).unwrap();
        assert_eq!(
            editor.tabs().len(),
            1,
            "an untouched new map makes way for the restored one"
        );
        assert_eq!(editor.scene.solid_count, solids);
        let document = editor.document.as_ref().unwrap();
        assert_eq!(document.path.as_deref(), Some(map.as_path()));
        assert!(document.dirty, "the map's own file does not have the changes yet");
        assert!(!document.needs_backup());
        let saved = Document::load(&map).unwrap();
        assert_eq!(
            saved.vmf.world().unwrap().solids().count(),
            0,
            "the map's own file is left alone"
        );
        let _ = fs::remove_dir_all(&folder);
    }
}
