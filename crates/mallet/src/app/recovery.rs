//! The question Mallet asks when it starts after an earlier run did not shut down
//! correctly: which of the maps that had unsaved changes to open again from their
//! autosaved copies.

use std::time::SystemTime;

use eframe::egui::{self, Color32, RichText, Vec2};
use egui_phosphor::regular as icon;

use super::compile_window::open_folder;
use super::{count, MalletApp};
use crate::autosave::{self, LeftMap, Unfinished};
use crate::editor::Editor;

const MUTED: Color32 = Color32::from_gray(140);
const WARNING: Color32 = Color32::from_rgb(230, 180, 70);
/// How wide the question's text runs before it wraps.
const WIDTH: f32 = 460.0;

/// The copies earlier Mallets left behind, each with whether it is ticked to be restored.
pub(super) struct Recovery {
    sessions: Vec<Unfinished>,
    maps: Vec<(LeftMap, bool)>,
}

impl Recovery {
    /// Looks for copies left by Mallets that did not shut down correctly; `None` when
    /// there are none. A copy older than its map's own file starts unticked.
    pub(super) fn find() -> Option<Self> {
        let sessions = autosave::unfinished();
        let maps: Vec<(LeftMap, bool)> = sessions
            .iter()
            .flat_map(|session| session.maps.iter().cloned())
            .map(|map| {
                let restore = !map.saved_since;
                (map, restore)
            })
            .collect();
        (!maps.is_empty()).then_some(Self { sessions, maps })
    }

    /// Forgets the earlier Mallets' markers, so the copies are not offered again.
    fn forget(self) {
        for session in self.sessions {
            session.forget();
        }
    }
}

/// How long ago a time was, in words, such as `5 minutes ago`.
fn ago(time: SystemTime) -> String {
    let seconds = time.elapsed().map_or(0, |elapsed| elapsed.as_secs());
    let (number, unit) = match seconds {
        0..60 => return "less than a minute ago".to_owned(),
        60..3600 => (seconds / 60, "minute"),
        3600..86_400 => (seconds / 3600, "hour"),
        _ => (seconds / 86_400, "day"),
    };
    format!("{} ago", count(usize::try_from(number).unwrap_or(usize::MAX), unit))
}

impl MalletApp {
    /// Asks which autosaved copies to open again, while an earlier run has left some.
    pub(super) fn recovery_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        let Some(recovery) = &mut self.recovery else {
            return;
        };
        let mut restore = None;
        egui::Window::new("Restore unsaved maps")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_width(WIDTH);
                ui.label(
                    "Mallet did not shut down correctly the last time it was used. These maps had unsaved \
                     changes, which are kept in autosaved copies:",
                );
                ui.add_space(6.0);
                for (left, chosen) in &mut recovery.maps {
                    ui.checkbox(chosen, RichText::new(&left.map.title).strong());
                    ui.indent(&left.map.copy, |ui| {
                        let place = match &left.map.original {
                            Some(original) => original.display().to_string(),
                            None => "never saved".to_owned(),
                        };
                        ui.label(RichText::new(place).color(MUTED));
                        ui.label(RichText::new(format!("Copied {}", ago(left.copied))).color(MUTED));
                        if left.saved_since {
                            ui.label(RichText::new("The map has been saved since this copy was made.").color(WARNING));
                        }
                    });
                }
                ui.add_space(6.0);
                ui.label(
                    RichText::new(
                        "A restored map opens with unsaved changes, and its own file stays as it is until you \
                         save. The copies stay in the autosave folder either way.",
                    )
                    .color(MUTED),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let any = recovery.maps.iter().any(|(_, chosen)| *chosen);
                    if ui.add_enabled(any, egui::Button::new("Restore")).clicked() {
                        restore = Some(true);
                    }
                    if ui.button("Don't restore").clicked() {
                        restore = Some(false);
                    }
                    let folder = recovery.maps.first().and_then(|(left, _)| left.map.copy.parent());
                    if let Some(folder) = folder {
                        if ui
                            .button(format!("{} Open the autosave folder", icon::FOLDER_OPEN))
                            .clicked()
                        {
                            open_folder(folder);
                        }
                    }
                });
            });
        let Some(restore) = restore else {
            return;
        };
        let Some(recovery) = self.recovery.take() else {
            return;
        };
        if restore {
            let chosen = recovery.maps.iter().filter(|(_, chosen)| *chosen);
            let results: Vec<Result<(), String>> = chosen.map(|(left, _)| editor.restore_copy(&left.map)).collect();
            let restored = results.iter().filter(|result| result.is_ok()).count();
            if let Some(Err(failure)) = results.iter().find(|result| result.is_err()) {
                editor.message = failure.clone();
            } else if restored > 1 {
                editor.message = format!("Restored {restored} maps from their autosaved copies");
            }
        } else {
            editor.message = "Nothing was restored. The autosaved copies are still in the autosave folder.".to_owned();
        }
        recovery.forget();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn ages_read_as_words() {
        let before = |seconds| SystemTime::now() - Duration::from_secs(seconds);
        assert_eq!(ago(before(5)), "less than a minute ago");
        assert_eq!(ago(before(61)), "1 minute ago");
        assert_eq!(ago(before(3 * 3600 + 5)), "3 hours ago");
        assert_eq!(ago(before(2 * 86_400 + 5)), "2 days ago");
    }
}
