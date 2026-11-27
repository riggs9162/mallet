//! Notices when game files the editor has read are saved again while it runs, so edited
//! materials, textures and hotspot `.rect` files show without restarting.

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Weak};
use std::thread;
use std::time::Duration;

use mallet_content::GameFs;

/// How often the files read so far are looked at.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// A background thread watching the loose files of one mounted game. It stops once the
/// game's files are let go of or this is dropped.
pub struct ContentWatch {
    changes: Receiver<Vec<String>>,
}

impl ContentWatch {
    /// Starts watching the loose files `filesystem` reads.
    pub fn start(filesystem: &Arc<GameFs>) -> Self {
        let (sender, changes) = mpsc::channel();
        let filesystem: Weak<GameFs> = Arc::downgrade(filesystem);
        let _ = thread::Builder::new()
            .name("mallet content watch".to_owned())
            .spawn(move || loop {
                thread::sleep(POLL_INTERVAL);
                let Some(filesystem) = filesystem.upgrade() else {
                    return;
                };
                let changed = filesystem.changed_files();
                drop(filesystem);
                if !changed.is_empty() && sender.send(changed).is_err() {
                    return;
                }
            });
        Self { changes }
    }

    /// The game paths of the files that changed since this was last asked, such as
    /// `materials/brick/brickwall001a.vtf`.
    pub fn take(&self) -> HashSet<String> {
        self.changes.try_iter().flatten().collect()
    }
}
