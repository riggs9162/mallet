//! Hammer's VisGroups in the window: the list in the side panel with its User and Auto
//! tabs, the Move Selection to VisGroup dialog and the Edit VisGroups window.

use eframe::egui::{self, Color32, RichText, Sense, Vec2};
use mallet_vmf::visgroup::AutoGroup;

use super::MalletApp;
use crate::editor::{Editor, GroupState, MoveOptions, UserGroupRow};

/// Indent of a nested group in the lists, per level.
const INDENT: f32 = 14.0;
/// Height the group list may take in the side panel before it scrolls.
const LIST_HEIGHT: f32 = 200.0;
/// Height of the group list in the Edit VisGroups window.
const EDIT_LIST_HEIGHT: f32 = 200.0;
const MUTED: Color32 = Color32::from_gray(140);

/// The tabs of the VisGroups panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    User,
    Auto,
}

/// What Move Selection to VisGroup asks.
struct MoveDialog {
    /// The group to move to, or `None` for a new one.
    target: Option<u32>,
    /// The new group's name.
    name: String,
    options: MoveOptions,
    /// Set when the dialog opens, so the name field takes the keyboard once.
    focus: bool,
}

/// The name and colour typed into Edit VisGroups for one group.
struct Draft {
    id: u32,
    name: String,
    color: [u8; 3],
    /// The group's name and colour in the map when the draft was made, to notice an undo
    /// or another change underneath it.
    from: (String, [u8; 3]),
}

/// What the VisGroups panel and its windows remember between frames.
#[derive(Default)]
pub(super) struct VisGroupsUi {
    tab: Tab,
    /// The line picked in the User list, which Mark and Edit act on.
    selected: Option<u32>,
    /// The line picked in the Auto list.
    selected_auto: Option<AutoGroup>,
    move_dialog: Option<MoveDialog>,
    edit_open: bool,
    draft: Option<Draft>,
}

/// Something chosen in the panel or a window, run after it is drawn.
enum Command {
    Show(u32, bool),
    ShowAuto(AutoGroup, bool),
    ShowAll,
    Mark(u32),
    MarkAuto(AutoGroup),
    Add(u32),
    Remove(u32),
    Delete(u32),
    New,
    Purge,
    Edit(u32, String, [u8; 3]),
    OpenEdit,
    OpenMove(Option<u32>),
}

impl MalletApp {
    /// View > Move Selection to VisGroup: asks which group, or the name of a new one.
    pub(super) fn open_move_to_visgroup(&mut self, editor: &mut Editor, target: Option<u32>) {
        if editor.selection.is_empty() {
            editor.message = "Select the objects to move to a VisGroup first".to_owned();
            return;
        }
        self.visgroups.move_dialog = Some(MoveDialog {
            target,
            name: String::new(),
            options: MoveOptions::default(),
            focus: true,
        });
    }

    /// View > Edit VisGroups.
    pub(super) fn open_edit_visgroups(&mut self) {
        self.visgroups.edit_open = true;
    }

    /// The VisGroups section of the side panel.
    pub(super) fn visgroups_section(&mut self, ui: &mut egui::Ui, editor: &mut Editor) {
        let mut commands = Vec::new();
        let state = &mut self.visgroups;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut state.tab, Tab::User, "User");
            ui.selectable_value(&mut state.tab, Tab::Auto, "Auto");
        });
        let has_selection = !editor.selection.is_empty();
        egui::ScrollArea::vertical()
            .id_salt("visgroups_list")
            .max_height(LIST_HEIGHT)
            .auto_shrink([false, true])
            .show(ui, |ui| match state.tab {
                Tab::User => {
                    if editor.visgroups.user.is_empty() {
                        ui.label(
                            RichText::new("No VisGroups yet. Select objects and press New to make one.").color(MUTED),
                        );
                    }
                    for row in &editor.visgroups.user {
                        let response = group_row(
                            ui,
                            row.depth,
                            row.state,
                            Some(row.color),
                            &format!("{} ({})", row.name, row.members),
                            state.selected == Some(row.id),
                            |shown| commands.push(Command::Show(row.id, shown)),
                        );
                        if response.clicked() {
                            state.selected = Some(row.id);
                        }
                        if response.double_clicked() {
                            commands.push(Command::Mark(row.id));
                        }
                        response.context_menu(|ui| {
                            let mut choose = |ui: &mut egui::Ui, enabled: bool, text: &str, command: Command| {
                                if ui.add_enabled(enabled, egui::Button::new(text)).clicked() {
                                    commands.push(command);
                                    ui.close_menu();
                                }
                            };
                            choose(ui, has_selection, "Add Selection", Command::Add(row.id));
                            choose(ui, has_selection, "Remove Selection", Command::Remove(row.id));
                            choose(ui, row.members > 0, "Select Members", Command::Mark(row.id));
                            ui.separator();
                            choose(ui, true, "Delete", Command::Delete(row.id));
                        });
                    }
                }
                Tab::Auto => {
                    for row in &editor.visgroups.auto {
                        let response = group_row(
                            ui,
                            row.depth,
                            row.state,
                            None,
                            &format!("{} ({})", row.group.label(), row.members),
                            state.selected_auto == Some(row.group),
                            |shown| commands.push(Command::ShowAuto(row.group, shown)),
                        );
                        if response.clicked() {
                            state.selected_auto = Some(row.group);
                        }
                        if response.double_clicked() {
                            commands.push(Command::MarkAuto(row.group));
                        }
                    }
                }
            });
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Show All")
                .on_hover_text("Show every VisGroup again, the automatic ones too")
                .clicked()
            {
                commands.push(Command::ShowAll);
            }
            match state.tab {
                Tab::User => {
                    let picked = state
                        .selected
                        .filter(|id| editor.visgroups.user.iter().any(|row| row.id == *id));
                    if ui
                        .add_enabled(has_selection, egui::Button::new("New"))
                        .on_hover_text("Move the selected objects to a new VisGroup")
                        .on_disabled_hover_text("Select the objects for the new VisGroup first")
                        .clicked()
                    {
                        commands.push(Command::OpenMove(None));
                    }
                    if ui
                        .button("Edit")
                        .on_hover_text("Rename, recolour, add and remove VisGroups")
                        .clicked()
                    {
                        commands.push(Command::OpenEdit);
                    }
                    if ui
                        .add_enabled(picked.is_some(), egui::Button::new("Mark"))
                        .on_hover_text("Select the members of the picked VisGroup")
                        .clicked()
                    {
                        commands.extend(picked.map(Command::Mark));
                    }
                }
                Tab::Auto => {
                    if ui
                        .add_enabled(state.selected_auto.is_some(), egui::Button::new("Mark"))
                        .on_hover_text("Select the members of the picked group")
                        .clicked()
                    {
                        commands.extend(state.selected_auto.map(Command::MarkAuto));
                    }
                }
            }
        });
        if editor.visgroups.quick_hidden > 0 {
            ui.label(RichText::new(format!("QuickHide hides {} objects", editor.visgroups.quick_hidden)).color(MUTED));
        }
        self.run_visgroup_commands(commands, editor);
    }

    /// Draws the Move Selection to VisGroup dialog and the Edit VisGroups window.
    pub(super) fn visgroup_windows(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        self.move_to_visgroup_window(ctx, editor);
        self.edit_visgroups_window(ctx, editor);
    }

    fn move_to_visgroup_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        let Some(dialog) = &mut self.visgroups.move_dialog else {
            return;
        };
        let mut ok = false;
        let mut cancel = false;
        let focus = std::mem::take(&mut dialog.focus);
        let rows = &editor.visgroups.user;
        if dialog.target.is_some_and(|id| !rows.iter().any(|row| row.id == id)) {
            dialog.target = None;
        }
        egui::Window::new("Move Selection to VisGroup")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                let current = dialog
                    .target
                    .and_then(|id| rows.iter().find(|row| row.id == id))
                    .map_or("New VisGroup", |row| row.name.as_str());
                ui.horizontal(|ui| {
                    ui.label("Move to:");
                    egui::ComboBox::from_id_salt("move_to_visgroup")
                        .selected_text(current)
                        .width(180.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut dialog.target, None, "New VisGroup");
                            for row in rows {
                                ui.selectable_value(&mut dialog.target, Some(row.id), &row.name);
                            }
                        });
                });
                let mut entered = false;
                if dialog.target.is_none() {
                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        let field = ui.add(
                            egui::TextEdit::singleline(&mut dialog.name)
                                .hint_text("VisGroup")
                                .desired_width(180.0),
                        );
                        if focus {
                            field.request_focus();
                        }
                        entered = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    });
                }
                ui.checkbox(&mut dialog.options.leave_others, "Remove from all other VisGroups");
                ui.checkbox(&mut dialog.options.hide, "Hide the objects");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ok = ui.button("OK").clicked() || entered;
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if ok {
            let result = match dialog.target {
                Some(id) => editor.selection_to_visgroup(id, dialog.options).map(|_| id),
                None => editor.selection_to_new_visgroup(&dialog.name, dialog.options),
            };
            match result {
                Ok(id) => {
                    self.visgroups.selected = Some(id);
                    self.visgroups.tab = Tab::User;
                    self.visgroups.move_dialog = None;
                }
                Err(error) => editor.message = error,
            }
        } else if cancel || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.visgroups.move_dialog = None;
        }
    }

    fn edit_visgroups_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if !self.visgroups.edit_open {
            return;
        }
        let state = &mut self.visgroups;
        let rows = &editor.visgroups.user;
        if state.selected.is_none_or(|id| !rows.iter().any(|row| row.id == id)) {
            state.selected = rows.first().map(|row| row.id);
        }
        let picked = state.selected.and_then(|id| rows.iter().find(|row| row.id == id));
        sync_draft(&mut state.draft, picked);
        let mut commands = Vec::new();
        let mut open = true;
        egui::Window::new("Edit VisGroups")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.set_height(EDIT_LIST_HEIGHT);
                    ui.vertical(|ui| {
                        ui.set_width(200.0);
                        egui::ScrollArea::vertical()
                            .id_salt("edit_visgroups_list")
                            .max_height(EDIT_LIST_HEIGHT)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if rows.is_empty() {
                                    ui.label(RichText::new("The map has no VisGroups.").color(MUTED));
                                }
                                for row in rows {
                                    ui.horizontal(|ui| {
                                        ui.add_space(INDENT * row.depth as f32);
                                        swatch(ui, row.color);
                                        let label = format!("{} ({})", row.name, row.members);
                                        if ui.selectable_label(state.selected == Some(row.id), label).clicked() {
                                            state.selected = Some(row.id);
                                        }
                                    });
                                }
                            });
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_width(190.0);
                        if let Some(draft) = &mut state.draft {
                            ui.label("Name:");
                            let field = ui.add(egui::TextEdit::singleline(&mut draft.name).desired_width(170.0));
                            ui.horizontal(|ui| {
                                ui.label("Color:");
                                ui.color_edit_button_srgb(&mut draft.color);
                            });
                            let settled = !field.has_focus() && !ui.input(|input| input.pointer.any_down());
                            let changed = draft.name.trim() != draft.from.0 || draft.color != draft.from.1;
                            if settled && changed && !draft.name.trim().is_empty() {
                                commands.push(Command::Edit(draft.id, draft.name.clone(), draft.color));
                            }
                            ui.add_space(6.0);
                            if ui
                                .button("Remove")
                                .on_hover_text("Remove the group. Its objects stay in the map.")
                                .clicked()
                            {
                                commands.push(Command::Delete(draft.id));
                            }
                        } else {
                            ui.label(RichText::new("Pick a VisGroup to change it.").color(MUTED));
                        }
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("New Group").clicked() {
                        commands.push(Command::New);
                    }
                    if ui
                        .button("Purge Empty Groups")
                        .on_hover_text("Remove every VisGroup without objects")
                        .clicked()
                    {
                        commands.push(Command::Purge);
                    }
                });
            });
        self.visgroups.edit_open = open;
        self.run_visgroup_commands(commands, editor);
    }

    fn run_visgroup_commands(&mut self, commands: Vec<Command>, editor: &mut Editor) {
        for command in commands {
            let result = match command {
                Command::Show(id, shown) => editor.set_visgroup_shown(id, shown).map(|_| ()),
                Command::ShowAuto(group, shown) => editor.set_auto_visgroup_shown(group, shown).map(|_| ()),
                Command::ShowAll => editor.show_all_visgroups().map(|_| ()),
                Command::Mark(id) => editor.select_visgroup(id).map(|_| ()),
                Command::MarkAuto(group) => editor.select_auto_visgroup(group).map(|_| ()),
                Command::Add(id) => editor.set_selection_in_visgroup(id, true).map(|_| ()),
                Command::Remove(id) => editor.set_selection_in_visgroup(id, false).map(|_| ()),
                Command::Delete(id) => editor.delete_visgroup(id),
                Command::New => editor.create_visgroup("", None).map(|id| {
                    self.visgroups.selected = Some(id);
                }),
                Command::Purge => editor.purge_empty_visgroups().map(|_| ()),
                Command::Edit(id, name, color) => editor.edit_visgroup(id, &name, color),
                Command::OpenEdit => {
                    self.open_edit_visgroups();
                    Ok(())
                }
                Command::OpenMove(target) => {
                    self.open_move_to_visgroup(editor, target);
                    Ok(())
                }
            };
            if let Err(error) = result {
                editor.message = error;
            }
        }
    }
}

/// Keeps the draft on the picked group, starting it again when another group is picked
/// or the group changed in the map.
fn sync_draft(draft: &mut Option<Draft>, picked: Option<&UserGroupRow>) {
    let Some(row) = picked else {
        *draft = None;
        return;
    };
    let current = (row.name.clone(), row.color);
    if draft
        .as_ref()
        .is_some_and(|draft| draft.id == row.id && draft.from == current)
    {
        return;
    }
    *draft = Some(Draft {
        id: row.id,
        name: row.name.clone(),
        color: row.color,
        from: current,
    });
}

/// A small square in a group's colour.
fn swatch(ui: &mut egui::Ui, color: [u8; 3]) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, 2.0, Color32::from_rgb(color[0], color[1], color[2]));
}

/// One line of a group list: a tick box that shows or hides the group, half ticked when
/// only some members show, then the colour and the name. `toggle` gets whether the group
/// should show from now on. Returns the name's response, for picking the line.
fn group_row(
    ui: &mut egui::Ui,
    depth: usize,
    state: GroupState,
    color: Option<[u8; 3]>,
    label: &str,
    selected: bool,
    mut toggle: impl FnMut(bool),
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.add_space(INDENT * depth as f32);
        let mut ticked = state != GroupState::Hidden;
        let tick = egui::Checkbox::without_text(&mut ticked).indeterminate(state == GroupState::Partial);
        let hint = match state {
            GroupState::Shown => "Hide the group",
            GroupState::Partial => "Some of the group is hidden. Click to show all of it.",
            GroupState::Hidden => "Show the group",
        };
        if ui.add(tick).on_hover_text(hint).clicked() {
            toggle(state != GroupState::Shown);
        }
        if let Some(color) = color {
            swatch(ui, color);
        }
        ui.selectable_label(selected, label)
    })
    .inner
}
