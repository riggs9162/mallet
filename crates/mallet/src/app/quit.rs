//! Closing Mallet while maps have unsaved changes. As in Hammer, the window stays open and
//! asks about each of those maps in turn: save it, leave its changes behind, or cancel and
//! keep working. Nothing is thrown away before the window really closes, so cancelling at
//! any question leaves every map as it was, apart from the ones that were just saved.

use eframe::egui::{self, Vec2, ViewportCommand};

use super::{save_map, MalletApp};
use crate::editor::{Editor, TabInfo};

/// The questions asked while Mallet is closing.
#[derive(Debug, Default)]
pub(super) struct Quit {
    /// Whether Mallet is closing: the window was asked to close, and the user has not
    /// cancelled since.
    active: bool,
    /// The tabs whose changes the user chose not to save, by their lasting ids.
    discard: Vec<u64>,
    /// The tab the question on screen is about, which was brought to the front for it.
    shown: Option<u64>,
}

/// The first tab that still has to be asked about: one with unsaved changes that the user
/// has not chosen to leave behind.
fn next_unsaved(tabs: &[TabInfo], discard: &[u64]) -> Option<usize> {
    tabs.iter().position(|tab| tab.dirty && !discard.contains(&tab.id))
}

impl MalletApp {
    /// Keeps the window open while maps have unsaved changes and asks what to do with each
    /// one, bringing its tab to the front and keeping the question above every other
    /// window. Once none is left to ask about, the window closes. A map that could not be
    /// saved, or whose Save As was cancelled, calls the closing off, like Cancel does. The
    /// question about closing one tab makes way while this one is asked.
    pub(super) fn quit_dialog(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        let requested = ctx.input(|input| input.viewport().close_requested());
        if requested && !self.quit.active {
            self.quit = Quit {
                active: true,
                ..Quit::default()
            };
        }
        if !self.quit.active {
            return;
        }
        self.closing = None;
        let tabs = editor.tabs();
        let Some(index) = next_unsaved(&tabs, &self.quit.discard) else {
            if !requested {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            return;
        };
        if requested {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        }
        let tab = &tabs[index];
        if self.quit.shown != Some(tab.id) {
            self.quit.shown = Some(tab.id);
            let _ = editor.switch_tab(index);
        }
        let mut answer = None;
        let question = egui::Window::new("Unsaved changes")
            .id(egui::Id::new("quit_unsaved_changes"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(format!("Save changes to {} before Mallet closes?", tab.title));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    for choice in ["Save", "Don't save", "Cancel"] {
                        if ui.button(choice).clicked() {
                            answer = Some(choice);
                        }
                    }
                });
            });
        if let Some(question) = question {
            ctx.move_to_top(question.response.layer_id);
        }
        match answer {
            Some("Save") => {
                if editor.switch_tab(index).is_ok() {
                    save_map(editor);
                }
                if editor.tabs().get(index).is_none_or(|tab| tab.dirty) {
                    self.quit = Quit::default();
                }
            }
            Some("Don't save") => self.quit.discard.push(tab.id),
            Some(_) => self.quit = Quit::default(),
            None => return,
        }
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: u64, dirty: bool) -> TabInfo {
        TabInfo {
            id,
            title: format!("map{id}.vmf"),
            path: String::new(),
            dirty,
            backup: None,
        }
    }

    #[test]
    fn every_unsaved_map_is_asked_about_once() {
        let tabs = [tab(4, false), tab(7, true), tab(2, true)];
        let mut discard = Vec::new();
        assert_eq!(next_unsaved(&tabs, &discard), Some(1), "saved maps are passed over");
        discard.push(7);
        assert_eq!(next_unsaved(&tabs, &discard), Some(2));
        discard.push(2);
        assert_eq!(
            next_unsaved(&tabs, &discard),
            None,
            "nothing is left to ask, so Mallet may close"
        );
        let saved = [tab(4, false), tab(7, false), tab(2, false)];
        assert_eq!(next_unsaved(&saved, &[]), None);
    }
}
