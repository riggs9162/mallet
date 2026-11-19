//! What Mallet does when an error stops it. The report goes to `crash.log` in Mallet's
//! settings folder, every map with unsaved changes is copied to the autosave folder, and a
//! message says what went wrong and where the maps went before Mallet closes. The next
//! start offers the copies back.

use std::panic::{self, AssertUnwindSafe, PanicHookInfo};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::config;
use crate::editor::{Editor, Rescued};

/// The exit code of a Mallet that an error stopped, the one Rust programs end with.
const EXIT_CODE: i32 = 101;

/// The latest error on the main thread, as the panic hook saw it.
static LATEST: Mutex<Option<Report>> = Mutex::new(None);

/// An error that stopped the main thread.
#[derive(Debug, Clone)]
struct Report {
    /// What went wrong and where, on one line.
    summary: String,
    /// The file the full report was written to, when it could be.
    log: Option<PathBuf>,
}

/// Has Mallet write what went wrong to `crash.log` in its settings folder when an error
/// stops it, and keeps the error for the message [`stop`] shows. Only an error on the main
/// thread stops Mallet; the worker threads' are recovered from and left out.
pub fn install_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if std::thread::current().name() == Some("main") {
            let report = Report {
                summary: summary(info),
                log: write_log(info),
            };
            *LATEST.lock().unwrap_or_else(PoisonError::into_inner) = Some(report);
        }
        previous(info);
    }));
}

/// Writes the full report of an error, with where in the program it happened, and
/// returns the file it went to.
fn write_log(info: &PanicHookInfo<'_>) -> Option<PathBuf> {
    let folder = config::data_dir()?;
    let version = env!("CARGO_PKG_VERSION");
    let trace = std::backtrace::Backtrace::force_capture();
    let path = folder.join("crash.log");
    std::fs::create_dir_all(&folder).ok()?;
    std::fs::write(&path, format!("Mallet {version} stopped: {info}\n\n{trace}\n")).ok()?;
    Some(path)
}

/// What went wrong and where, on one line.
fn summary(info: &PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("an unexpected error");
    match info.location() {
        Some(location) => format!("{message} ({}, line {})", location.file(), location.line()),
        None => message.to_owned(),
    }
}

/// Ends Mallet after an error stopped its main thread: copies every map with unsaved
/// changes to the autosave folder, tells the user what happened and where the maps went,
/// and exits. The editor may have been in the middle of whatever went wrong, so nothing
/// else is done with it.
pub fn stop(editor: &mut Editor) -> ! {
    let rescued = panic::catch_unwind(AssertUnwindSafe(|| editor.rescue_maps())).ok();
    let report = LATEST.lock().unwrap_or_else(PoisonError::into_inner).clone();
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("Mallet has stopped")
        .set_description(message(report.as_ref(), rescued.as_deref()))
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
    std::process::exit(EXIT_CODE)
}

/// The message shown when Mallet stops: the error, each map with unsaved changes with the
/// copy made of it, and where the copies and the full report are. Folders get lines of
/// their own, as a message box breaks a long path in the middle wherever a line ends.
/// `rescued` is `None` when the maps could not even be looked at.
fn message(report: Option<&Report>, rescued: Option<&[Rescued]>) -> String {
    let mut text = "Mallet ran into an error and has to close.\n".to_owned();
    if let Some(report) = report {
        text.push_str(&format!("\n{}\n", report.summary));
    }
    match rescued {
        None => text.push_str("\nThe open maps could not be checked for unsaved changes.\n"),
        Some([]) => text.push_str("\nNo open map had unsaved changes.\n"),
        Some(maps) => {
            text.push_str("\nMaps with unsaved changes were copied to the autosave folder first:\n\n");
            for map in maps {
                match &map.copy {
                    Ok(copy) => {
                        let name = copy.file_name().unwrap_or(copy.as_os_str()).to_string_lossy();
                        text.push_str(&format!("    {}  ->  {name}\n", map.title));
                    }
                    Err(error) => text.push_str(&format!("    {} could not be copied: {error}\n", map.title)),
                }
            }
            let mut folders: Vec<&Path> = maps.iter().filter_map(|map| map.copy.as_ref().ok()?.parent()).collect();
            folders.sort();
            folders.dedup();
            if !folders.is_empty() {
                text.push_str("\nMallet offers to restore them when it starts again. The copies are in:\n");
                for folder in folders {
                    text.push_str(&format!("{}\n", folder.display()));
                }
            }
        }
    }
    if let Some(log) = report.and_then(|report| report.log.as_deref()) {
        text.push_str(&format!("\nThe full report is in:\n{}", log.display()));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_names_the_error_and_every_copy() {
        let report = Report {
            summary: "index out of bounds (crates/mallet/src/scene.rs, line 12)".to_owned(),
            log: Some(PathBuf::from("crash.log")),
        };
        let rescued = [
            Rescued {
                title: "city.vmf".to_owned(),
                copy: Ok(Path::new("backups").join("city_autosave_0003.vmf")),
            },
            Rescued {
                title: "town.vmf".to_owned(),
                copy: Ok(Path::new("backups").join("town_autosave_0001.vmf")),
            },
            Rescued {
                title: "untitled".to_owned(),
                copy: Err("the disk is full".to_owned()),
            },
        ];
        let text = message(Some(&report), Some(&rescued));
        assert!(text.contains("index out of bounds (crates/mallet/src/scene.rs, line 12)"));
        assert!(text.contains("    city.vmf  ->  city_autosave_0003.vmf\n"));
        assert!(text.contains("untitled could not be copied: the disk is full"));
        assert!(
            text.contains("The copies are in:\nbackups\n\n"),
            "a folder is named once"
        );
        assert!(text.ends_with("The full report is in:\ncrash.log"));

        let nothing = message(None, Some(&[]));
        assert!(nothing.contains("No open map had unsaved changes."));
        assert!(!nothing.contains("restore"));
        assert!(message(None, None).contains("could not be checked"));
    }
}
