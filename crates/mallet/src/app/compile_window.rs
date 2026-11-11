//! The compile window (F9): pick a preset, arrange its steps, tick options, and watch the
//! compile run with live output, progress per step and a list of problems that can take
//! you to the brush or place they name. Works like CompilePal, inside the editor.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use eframe::egui::{self, Color32, RichText};
use egui_phosphor::regular as icon;
use glam::DVec3;

use super::MalletApp;
use crate::compile::runner::{JobState, LineKind, Severity, StepStatus};
use crate::compile::{default_presets, plan_step, Build, Context, OptionSpec, Preset, Step, StepKind, VARIABLES};
use crate::config::{self, ToolKind};
use crate::editor::{Editor, SelectMode};
use crate::scene::Owner;

const ACCENT: Color32 = Color32::from_rgb(70, 140, 230);
const GOOD: Color32 = Color32::from_rgb(90, 190, 110);
const WARNING: Color32 = Color32::from_rgb(230, 170, 60);
const ERROR: Color32 = Color32::from_rgb(235, 90, 80);
const MUTED: Color32 = Color32::from_gray(140);
const STEP_LIST_WIDTH: f32 = 250.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Tab {
    #[default]
    Steps,
    Output,
    Problems,
}

/// What the compile window remembers between frames.
pub(super) struct CompileWindowState {
    pub(super) tab: Tab,
    preset: usize,
    step: Option<usize>,
    renaming: Option<String>,
    /// Keep the log scrolled to the newest line.
    follow: bool,
    /// Show only warnings and errors in the log.
    problems_only: bool,
    /// Show only the output of one step, picked from the strip above the log.
    only_step: Option<usize>,
    /// Whether the window was open last frame, to pick the last preset when it opens.
    was_open: bool,
}

impl Default for CompileWindowState {
    fn default() -> Self {
        Self {
            tab: Tab::Steps,
            preset: 0,
            step: Some(0),
            renaming: None,
            follow: true,
            problems_only: false,
            only_step: None,
            was_open: false,
        }
    }
}

/// Something to do once the window is drawn.
enum Command {
    Compile(String),
    SaveAsThenCompile(String),
    Cancel,
    OpenOptions,
    SelectBrush(u32),
    GoTo(DVec3),
    GoToLeak,
    ClearLeak,
    /// Back to Hammer's normal Run Map dialog.
    Normal,
}

/// Facts the window shows that come from the editor, gathered before drawing so the
/// presets can be edited in place.
struct Facts {
    context: Result<Context, String>,
    tools: Vec<(ToolKind, Option<PathBuf>)>,
    job: Option<Arc<Mutex<JobState>>>,
    running: bool,
    has_leak: bool,
    map_title: String,
    untitled: bool,
}

impl Facts {
    fn tool(&self, kind: ToolKind) -> Option<PathBuf> {
        self.tools
            .iter()
            .find(|(tool, _)| *tool == kind)
            .and_then(|(_, path)| path.clone())
    }
}

impl MalletApp {
    /// Opens the compile window on the preset used last.
    pub(super) fn open_compile_window(&mut self, editor: &mut Editor) {
        editor.compile_open = true;
        if let Some(index) = editor
            .compile_presets
            .iter()
            .position(|preset| preset.name == editor.config.compile.last_preset)
        {
            if self.compile.preset != index {
                self.compile.preset = index;
                self.compile.step = Some(0);
            }
        }
    }

    pub(super) fn compile_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if editor.compile_running() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if let Some(tab) = editor.compile_tab.take() {
            self.compile.tab = match tab.as_str() {
                "output" => Tab::Output,
                "problems" => Tab::Problems,
                _ => Tab::Steps,
            };
        }
        if !editor.compile_open {
            self.compile.was_open = false;
            return;
        }
        if !self.compile.was_open {
            self.compile.was_open = true;
            self.open_compile_window(editor);
        }
        if editor.compile_presets.is_empty() {
            editor.compile_presets = default_presets();
        }
        let state = &mut self.compile;
        state.preset = state.preset.min(editor.compile_presets.len() - 1);
        let facts = Facts {
            context: editor.compile_context(),
            tools: ToolKind::ALL
                .into_iter()
                .map(|tool| (tool, editor.tool_path(tool)))
                .collect(),
            job: editor.compile_job.as_ref().map(|job| job.shared()),
            running: editor.compile_running(),
            has_leak: editor.pointfile.is_some(),
            map_title: editor
                .document
                .as_ref()
                .map_or_else(|| "No map".to_owned(), |document| document.file_name()),
            untitled: editor.is_untitled(),
        };
        let mut open = true;
        let mut commands = Vec::new();
        let mut changed = false;
        egui::Window::new(format!("{} Compile", icon::HAMMER))
            .id(egui::Id::new("compile_window"))
            .open(&mut open)
            .default_size([980.0, 640.0])
            .min_width(720.0)
            .min_height(420.0)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                let width = ui.available_width();
                header(ui, state, editor, &facts, &mut commands, &mut changed);
                ui.add_space(4.0);
                tabs(ui, state, &facts);
                ui.separator();
                match state.tab {
                    Tab::Steps => {
                        let preset = &mut editor.compile_presets[state.preset];
                        steps_tab(ui, state, preset, &facts, width, &mut commands, &mut changed);
                    }
                    Tab::Output => output_tab(ui, state, &facts),
                    Tab::Problems => problems_tab(ui, &facts, &mut commands),
                }
            });
        if !open {
            editor.compile_open = false;
        }
        if changed {
            editor.save_compile_presets();
        }
        for command in commands {
            match command {
                Command::Compile(preset) => match editor.start_compile(&preset) {
                    Ok(()) => self.compile.tab = Tab::Output,
                    Err(error) => editor.message = error,
                },
                Command::SaveAsThenCompile(preset) => {
                    super::save_as_dialog(editor);
                    if !editor.is_untitled() {
                        match editor.start_compile(&preset) {
                            Ok(()) => self.compile.tab = Tab::Output,
                            Err(error) => editor.message = error,
                        }
                    }
                }
                Command::Cancel => {
                    editor.cancel_compile();
                }
                Command::OpenOptions => {
                    editor.options_open = true;
                    editor.options_section = Some("tools".to_owned());
                }
                Command::SelectBrush(id) => {
                    editor.select([Owner::Solid(id)], SelectMode::Replace);
                    let bounds = editor.scene.selection_bounds();
                    if !bounds.is_empty() {
                        editor.camera.look_at(&bounds);
                        editor.center_2d_on(bounds.center());
                    } else {
                        editor.message = format!("No brush has id {id}");
                    }
                }
                Command::GoTo(position) => {
                    editor.center_2d_on(position);
                    editor.camera.position = position + DVec3::new(-128.0, -128.0, 96.0);
                    editor.camera.yaw = 45.0;
                    editor.camera.pitch = -30.0;
                }
                Command::GoToLeak => editor.go_to_leak(),
                Command::ClearLeak => editor.clear_pointfile(),
                Command::Normal => self.run_map_normal(editor),
            }
        }
    }
}

/// Map name, preset picker and the compile button.
fn header(
    ui: &mut egui::Ui,
    state: &mut CompileWindowState,
    editor: &mut Editor,
    facts: &Facts,
    commands: &mut Vec<Command>,
    changed: &mut bool,
) {
    ui.horizontal(|ui| {
        if ui
            .button("Normal...")
            .on_hover_text("Hammer's normal Run Map dialog: run BSP, VIS and RAD, HDR and the game, nothing more")
            .clicked()
        {
            commands.push(Command::Normal);
        }
        ui.label(RichText::new(icon::FILE).size(18.0));
        let map = ui.label(RichText::new(&facts.map_title).strong());
        if let Ok(context) = &facts.context {
            map.on_hover_text(context.vmf.display().to_string());
        }
        ui.add_space(12.0);
        ui.label("Preset");
        let presets = &mut editor.compile_presets;
        if let Some(name) = &mut state.renaming {
            let response = ui.add(egui::TextEdit::singleline(name).desired_width(170.0));
            let done = response.lost_focus();
            response.request_focus();
            if done {
                let name = name.trim().to_owned();
                let taken = presets
                    .iter()
                    .enumerate()
                    .any(|(index, preset)| index != state.preset && preset.name.eq_ignore_ascii_case(&name));
                if !name.is_empty() && !taken {
                    presets[state.preset].name = name;
                    *changed = true;
                }
                state.renaming = None;
            }
        } else {
            let selected = presets[state.preset].name.clone();
            egui::ComboBox::from_id_salt("compile_preset")
                .selected_text(selected)
                .width(170.0)
                .show_ui(ui, |ui| {
                    for (index, preset) in presets.iter().enumerate() {
                        let item = ui.selectable_label(index == state.preset, &preset.name);
                        let item = if preset.description.is_empty() {
                            item
                        } else {
                            item.on_hover_text(&preset.description)
                        };
                        if item.clicked() {
                            state.preset = index;
                            state.step = Some(0);
                        }
                    }
                });
        }
        if ui.small_button(icon::PLUS).on_hover_text("New preset").clicked() {
            presets.push(Preset {
                name: unique_name(presets, "New preset"),
                description: String::new(),
                steps: vec![
                    Step::new(StepKind::Vbsp),
                    Step::new(StepKind::Vvis),
                    Step::new(StepKind::Vrad),
                    Step::new(StepKind::CopyToGame),
                ],
            });
            state.preset = presets.len() - 1;
            state.step = Some(0);
            *changed = true;
        }
        if ui
            .small_button(icon::COPY)
            .on_hover_text("Duplicate this preset")
            .clicked()
        {
            let mut copy = presets[state.preset].clone();
            copy.name = unique_name(presets, &format!("{} copy", copy.name));
            presets.insert(state.preset + 1, copy);
            state.preset += 1;
            *changed = true;
        }
        if ui
            .small_button(icon::PENCIL_SIMPLE)
            .on_hover_text("Rename this preset")
            .clicked()
        {
            state.renaming = Some(presets[state.preset].name.clone());
        }
        let can_delete = presets.len() > 1;
        if ui
            .add_enabled(can_delete, egui::Button::new(icon::TRASH).small())
            .on_hover_text("Delete this preset")
            .clicked()
        {
            presets.remove(state.preset);
            state.preset = state.preset.saturating_sub(1);
            state.step = Some(0);
            *changed = true;
        }
        ui.menu_button(icon::DOTS_THREE, |ui| {
            if ui
                .button(format!("{} Restore the default presets", icon::ARROWS_CLOCKWISE))
                .clicked()
            {
                let defaults = default_presets();
                for preset in defaults {
                    match presets.iter_mut().find(|existing| existing.name == preset.name) {
                        Some(existing) => *existing = preset,
                        None => presets.push(preset),
                    }
                }
                *changed = true;
                ui.close_menu();
            }
            if ui
                .button(format!("{} Open the compile logs folder", icon::FOLDER_OPEN))
                .clicked()
            {
                if let Some(folder) = config::data_dir().map(|dir| dir.join("logs")) {
                    open_folder(&folder);
                }
                ui.close_menu();
            }
            if let Ok(context) = &facts.context {
                if ui
                    .button(format!("{} Open the map's folder", icon::FOLDER_OPEN))
                    .clicked()
                {
                    open_folder(&context.map_dir());
                    ui.close_menu();
                }
                if ui
                    .button(format!("{} Open the game's maps folder", icon::FOLDER_OPEN))
                    .clicked()
                {
                    open_folder(&context.game_maps());
                    ui.close_menu();
                }
            }
            if ui.button(format!("{} Compile tools...", icon::GEAR)).clicked() {
                commands.push(Command::OpenOptions);
                ui.close_menu();
            }
        });

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let preset = editor.compile_presets[state.preset].name.clone();
            if facts.running {
                let button = egui::Button::new(
                    RichText::new(format!("{} Cancel", icon::STOP))
                        .size(15.0)
                        .color(Color32::WHITE),
                )
                .fill(ERROR)
                .min_size(egui::vec2(110.0, 28.0));
                if ui.add(button).clicked() {
                    commands.push(Command::Cancel);
                }
            } else {
                let ready = facts.context.is_ok() || (facts.untitled && editor.document.is_some());
                let button = egui::Button::new(
                    RichText::new(format!("{} Compile", icon::PLAY))
                        .size(15.0)
                        .color(Color32::WHITE),
                )
                .fill(ACCENT)
                .min_size(egui::vec2(110.0, 28.0));
                let response = ui.add_enabled(ready, button);
                let response = match &facts.context {
                    Ok(_) => response.on_hover_text(format!(
                        "Compile with \"{preset}\" (F9 opens this window, Shift+F9 compiles again)"
                    )),
                    Err(error) => response.on_hover_text(error).on_disabled_hover_text(error),
                };
                if response.clicked() {
                    commands.push(if facts.untitled {
                        Command::SaveAsThenCompile(preset)
                    } else {
                        Command::Compile(preset)
                    });
                }
            }
            if let Some(job) = &facts.job {
                let job = job.lock().unwrap_or_else(PoisonError::into_inner);
                let (text, color) = match &job.outcome {
                    None => (
                        format!("{:.0}%  {}", job.progress() * 100.0, clock(job.elapsed())),
                        ACCENT,
                    ),
                    Some(crate::compile::Outcome::Succeeded) => {
                        (format!("{} Done in {}", icon::CHECK_CIRCLE, clock(job.elapsed())), GOOD)
                    }
                    Some(crate::compile::Outcome::Failed(_)) => (format!("{} Failed", icon::X_CIRCLE), ERROR),
                    Some(crate::compile::Outcome::Cancelled) => (format!("{} Cancelled", icon::MINUS_CIRCLE), MUTED),
                };
                ui.label(RichText::new(text).color(color));
            }
        });
    });
    if let Err(error) = &facts.context {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} {error}", icon::WARNING)).color(WARNING));
        });
    }
}

fn tabs(ui: &mut egui::Ui, state: &mut CompileWindowState, facts: &Facts) {
    let (errors, warnings) = facts.job.as_ref().map_or((0, 0), |job| {
        let job = job.lock().unwrap_or_else(PoisonError::into_inner);
        (job.errors(), job.warnings())
    });
    ui.horizontal(|ui| {
        ui.selectable_value(&mut state.tab, Tab::Steps, format!("{} Steps", icon::LIST_NUMBERS));
        ui.selectable_value(&mut state.tab, Tab::Output, format!("{} Output", icon::TERMINAL_WINDOW));
        let problems = if errors > 0 {
            RichText::new(format!(
                "{} Problems ({}, {})",
                icon::WARNING_CIRCLE,
                count(errors, "error"),
                count(warnings, "warning")
            ))
            .color(ERROR)
        } else if warnings > 0 {
            RichText::new(format!(
                "{} Problems ({})",
                icon::WARNING_CIRCLE,
                count(warnings, "warning")
            ))
            .color(WARNING)
        } else {
            RichText::new(format!("{} Problems", icon::WARNING_CIRCLE))
        };
        ui.selectable_value(&mut state.tab, Tab::Problems, problems);
    });
}

#[allow(clippy::too_many_arguments)]
fn steps_tab(
    ui: &mut egui::Ui,
    state: &mut CompileWindowState,
    preset: &mut Preset,
    facts: &Facts,
    width: f32,
    commands: &mut Vec<Command>,
    changed: &mut bool,
) {
    let height = ui.available_height();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(STEP_LIST_WIDTH, height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(STEP_LIST_WIDTH);
                step_list(ui, state, preset, facts, changed);
            },
        );
        ui.separator();
        let editor_width = (width - STEP_LIST_WIDTH - 30.0).max(300.0);
        ui.allocate_ui_with_layout(
            egui::vec2(editor_width, height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(editor_width);
                egui::ScrollArea::vertical()
                    .id_salt("step_editor")
                    .auto_shrink([false, false])
                    .show(ui, |ui| match state.step.filter(|step| *step < preset.steps.len()) {
                        Some(index) => {
                            step_editor(ui, &mut preset.steps[index], facts, editor_width, commands, changed)
                        }
                        None => {
                            ui.label(RichText::new("Pick a step to change its options, or add one.").color(MUTED));
                        }
                    });
            },
        );
    });
}

fn step_list(
    ui: &mut egui::Ui,
    state: &mut CompileWindowState,
    preset: &mut Preset,
    facts: &Facts,
    changed: &mut bool,
) {
    if !preset.description.is_empty() {
        ui.label(RichText::new(&preset.description).small().color(MUTED));
        ui.add_space(4.0);
    }
    let statuses: Vec<(StepStatus, Option<Duration>, Option<f32>)> = facts
        .job
        .as_ref()
        .map(|job| {
            let job = job.lock().unwrap_or_else(PoisonError::into_inner);
            // Statuses only line up with this preset's steps when it is the one that ran.
            if job.preset != preset.name {
                return Vec::new();
            }
            job.steps
                .iter()
                .map(|step| (step.status, step.time(), step.progress))
                .collect()
        })
        .unwrap_or_default();
    // Run statuses line up with enabled steps only.
    let mut run_index = 0;
    let mut toggled = None;
    for (index, step) in preset.steps.iter().enumerate() {
        let status = if step.enabled && !statuses.is_empty() {
            let status = statuses.get(run_index).copied();
            run_index += 1;
            status
        } else {
            None
        };
        ui.horizontal(|ui| {
            let mut enabled = step.enabled;
            if ui.checkbox(&mut enabled, "").on_hover_text("Run this step").changed() {
                toggled = Some(index);
            }
            let text = format!("{}  {}", step_icon(step.kind), step.title());
            let text = if step.enabled {
                RichText::new(text)
            } else {
                RichText::new(text).color(MUTED).strikethrough()
            };
            if ui
                .selectable_label(state.step == Some(index), text)
                .on_hover_text(step.kind.description())
                .clicked()
            {
                state.step = Some(index);
            }
            if let Some((status, time, progress)) = status {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (glyph, color) = status_icon(status);
                    ui.label(RichText::new(glyph).color(color));
                    let mut detail = time.map(clock).unwrap_or_default();
                    if let (StepStatus::Running, Some(progress)) = (status, progress) {
                        detail = format!("{:.0}% {detail}", progress * 100.0);
                    }
                    ui.label(RichText::new(detail).small().color(MUTED));
                });
            }
        });
    }
    if let Some(index) = toggled {
        preset.steps[index].enabled = !preset.steps[index].enabled;
        *changed = true;
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.menu_button(format!("{} Add step", icon::PLUS), |ui| {
            for kind in StepKind::ALL {
                let item = ui
                    .button(format!("{}  {}", step_icon(kind), kind.label()))
                    .on_hover_text(kind.description());
                if item.clicked() {
                    let at = state
                        .step
                        .map_or(preset.steps.len(), |step| (step + 1).min(preset.steps.len()));
                    preset.steps.insert(at, Step::new(kind));
                    state.step = Some(at);
                    *changed = true;
                    ui.close_menu();
                }
            }
        });
        let selected = state.step.filter(|step| *step < preset.steps.len());
        if let Some(index) = selected {
            if ui
                .add_enabled(index > 0, egui::Button::new(icon::ARROW_UP).small())
                .on_hover_text("Move up")
                .clicked()
            {
                preset.steps.swap(index, index - 1);
                state.step = Some(index - 1);
                *changed = true;
            }
            let last = index + 1 >= preset.steps.len();
            if ui
                .add_enabled(!last, egui::Button::new(icon::ARROW_DOWN).small())
                .on_hover_text("Move down")
                .clicked()
            {
                preset.steps.swap(index, index + 1);
                state.step = Some(index + 1);
                *changed = true;
            }
            if ui.small_button(icon::COPY).on_hover_text("Duplicate step").clicked() {
                let copy = preset.steps[index].clone();
                preset.steps.insert(index + 1, copy);
                state.step = Some(index + 1);
                *changed = true;
            }
            if ui.small_button(icon::TRASH).on_hover_text("Remove step").clicked() {
                preset.steps.remove(index);
                state.step = if preset.steps.is_empty() {
                    None
                } else {
                    Some(index.min(preset.steps.len() - 1))
                };
                *changed = true;
            }
        }
    });
    ui.add_space(8.0);
    ui.label(RichText::new("Preset description").small().color(MUTED));
    if ui
        .add(
            egui::TextEdit::multiline(&mut preset.description)
                .desired_rows(2)
                .desired_width(STEP_LIST_WIDTH - 8.0),
        )
        .changed()
    {
        *changed = true;
    }
}

fn step_editor(
    ui: &mut egui::Ui,
    step: &mut Step,
    facts: &Facts,
    width: f32,
    commands: &mut Vec<Command>,
    changed: &mut bool,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(step_icon(step.kind)).size(22.0));
        ui.heading(step.title());
    });
    ui.label(RichText::new(step.kind.description()).color(MUTED));
    ui.add_space(6.0);
    let field = (width - 170.0).max(200.0);

    if let Some(tool) = step.kind.tool() {
        ui.horizontal(|ui| {
            let path = step
                .program
                .clone()
                .filter(|path| !path.as_os_str().is_empty())
                .or_else(|| facts.tool(tool));
            match path {
                Some(path) if path.is_file() => {
                    ui.label(RichText::new(icon::CHECK_CIRCLE).color(GOOD));
                    ui.label(RichText::new(path.display().to_string()).small().monospace())
                        .on_hover_text("The program this step runs");
                }
                _ => {
                    ui.label(RichText::new(format!("{} {} was not found", icon::X_CIRCLE, tool.label())).color(ERROR));
                    if ui.button("Set it up...").clicked() {
                        commands.push(Command::OpenOptions);
                    }
                }
            }
        });
        egui::CollapsingHeader::new("Use another program for this step only")
            .id_salt("step_program_override")
            .show(ui, |ui| {
                if path_field(ui, &mut step.program, field, "exe") {
                    *changed = true;
                }
            });
        ui.add_space(4.0);
    }

    if step.kind == StepKind::Custom {
        egui::Grid::new("custom_step")
            .num_columns(2)
            .spacing([10.0, 6.0])
            .show(ui, |ui| {
                ui.label("Name");
                *changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut step.name)
                            .hint_text("Custom program")
                            .desired_width(field),
                    )
                    .changed();
                ui.end_row();
                ui.label("Program");
                *changed |= path_field(ui, &mut step.program, field - 70.0, "exe");
                ui.end_row();
                ui.label("Arguments");
                *changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut step.args)
                            .hint_text("\"{bsp}\" --flag")
                            .desired_width(field)
                            .font(egui::TextStyle::Monospace),
                    )
                    .changed();
                ui.end_row();
                ui.label("Working folder");
                *changed |= ui
                    .add(
                        egui::TextEdit::singleline(&mut step.working_dir)
                            .hint_text("{mapdir}")
                            .desired_width(field),
                    )
                    .changed();
                ui.end_row();
            });
        ui.add_space(6.0);
    }

    let specs = step.kind.options();
    if !specs.is_empty() {
        let build = step.kind.tool().and_then(|tool| {
            step.program
                .clone()
                .filter(|path| !path.as_os_str().is_empty())
                .or_else(|| facts.tool(tool))
                .map(|path| Build::of(&path))
        });
        let filter_id = ui.id().with("step_option_filter");
        let all_id = ui.id().with("step_option_show_all");
        let mut filter: String = ui.data_mut(|data| data.get_temp(filter_id).unwrap_or_default());
        let mut show_all: bool = ui.data_mut(|data| data.get_temp(all_id).unwrap_or(false));
        ui.horizontal(|ui| {
            ui.label(RichText::new("Options").strong());
            ui.add(
                egui::TextEdit::singleline(&mut filter)
                    .hint_text(format!("{} Search options", icon::MAGNIFYING_GLASS))
                    .desired_width(180.0),
            );
            if let Some(build) = build.filter(|build| specs.iter().any(|spec| !build.supports(spec.build))) {
                let other = match build {
                    Build::PlusPlus => "game tools",
                    _ => "Hammer++",
                };
                ui.checkbox(&mut show_all, format!("Show {other} options"))
                    .on_hover_text("Options the program this step runs does not understand");
            }
        });
        let needle = filter.trim().to_ascii_lowercase();
        let shown: Vec<&OptionSpec> = specs
            .iter()
            .filter(|spec| {
                show_all
                    || step.option(spec.flag).is_some()
                    || build.is_none_or(|build| build.supports(spec.build))
            })
            .filter(|spec| {
                needle.is_empty()
                    || [spec.flag, spec.name, spec.description]
                        .iter()
                        .any(|text| text.to_ascii_lowercase().contains(&needle))
            })
            .collect();
        ui.data_mut(|data| {
            data.insert_temp(filter_id, filter);
            data.insert_temp(all_id, show_all);
        });
        egui::Grid::new("step_options")
            .num_columns(3)
            .striped(true)
            .spacing([10.0, 4.0])
            .show(ui, |ui| {
                for spec in shown {
                    option_row(ui, step, spec, build, changed);
                    ui.end_row();
                }
            });
        let unknown: Vec<String> = step
            .options
            .iter()
            .filter(|option| !specs.iter().any(|spec| spec.flag == option.flag))
            .map(|option| option.flag.clone())
            .collect();
        for flag in unknown {
            ui.horizontal(|ui| {
                let mut on = true;
                if ui
                    .checkbox(&mut on, RichText::new(&flag).monospace())
                    .on_hover_text("An option Mallet no longer lists. Untick it to remove it.")
                    .changed()
                {
                    step.options.retain(|option| option.flag != flag);
                    *changed = true;
                }
            });
        }
        ui.add_space(6.0);
    }

    if !matches!(step.kind, StepKind::CopyToGame) {
        ui.horizontal(|ui| {
            ui.label("Extra arguments");
            *changed |= ui
                .add(
                    egui::TextEdit::singleline(&mut step.extra_args)
                        .hint_text("anything else, such as -threads 16")
                        .desired_width(field)
                        .font(egui::TextStyle::Monospace),
                )
                .changed();
        });
    }
    *changed |= ui
        .checkbox(&mut step.continue_on_error, "Keep compiling if this step fails")
        .changed();

    ui.add_space(8.0);
    ui.label(RichText::new("Command").strong());
    let preview = match &facts.context {
        Ok(context) => plan_step(step, context, &|tool| facts.tool(tool)).map(|planned| planned.action.display()),
        Err(error) => Err(error.clone()),
    };
    egui::Frame::group(ui.style())
        .fill(Color32::from_gray(22))
        .show(ui, |ui| {
            ui.set_width(width - 24.0);
            match &preview {
                Ok(command) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(command).monospace().small());
                    });
                    if ui.small_button(format!("{} Copy", icon::CLIPBOARD_TEXT)).clicked() {
                        ui.ctx().copy_text(command.clone());
                    }
                }
                Err(error) => {
                    ui.label(RichText::new(error).color(WARNING));
                }
            }
        });

    ui.add_space(6.0);
    egui::CollapsingHeader::new("Variables")
        .id_salt("compile_variables")
        .show(ui, |ui| {
            egui::Grid::new("variables")
                .num_columns(3)
                .spacing([12.0, 2.0])
                .show(ui, |ui| {
                    for (name, meaning) in VARIABLES {
                        ui.label(RichText::new(name).monospace());
                        ui.label(RichText::new(meaning).color(MUTED));
                        let value = facts
                            .context
                            .as_ref()
                            .map(|context| context.expand(name))
                            .unwrap_or_default();
                        ui.label(RichText::new(value).small().monospace());
                        ui.end_row();
                    }
                });
        });
}

/// One option: a checkbox, its value when it takes one, and its warning.
fn option_row(ui: &mut egui::Ui, step: &mut Step, spec: &OptionSpec, build: Option<Build>, changed: &mut bool) {
    let mut on = step.option(spec.flag).is_some();
    let hover = match spec.build {
        Build::Any => format!("{}\n\n{}", spec.flag, spec.description),
        only => format!("{}\n\n{}\n\n{}", spec.flag, spec.description, only.requirement()),
    };
    if ui.checkbox(&mut on, spec.name).on_hover_text(&hover).changed() {
        step.set_option(spec.flag, on);
        *changed = true;
    }
    match (
        spec.value,
        step.options.iter_mut().find(|option| option.flag == spec.flag),
    ) {
        (Some(_), Some(option)) => {
            *changed |= ui
                .add(egui::TextEdit::singleline(&mut option.value).desired_width(120.0))
                .changed();
        }
        (Some(default), None) => {
            ui.label(RichText::new(default).small().color(MUTED));
        }
        _ => {
            ui.label("");
        }
    }
    let unsupported = build.is_some_and(|build| !build.supports(spec.build));
    ui.horizontal(|ui| {
        ui.label(RichText::new(spec.flag).monospace().small().color(MUTED));
        if spec.build != Build::Any {
            let color = if unsupported { WARNING } else { MUTED };
            ui.label(RichText::new(spec.build.tag()).small().color(color))
                .on_hover_text(spec.build.requirement());
        }
        if on && unsupported {
            ui.label(
                RichText::new(format!("{} This program does not understand it", icon::WARNING))
                    .small()
                    .color(WARNING),
            );
        } else if on && !spec.warning.is_empty() {
            ui.label(
                RichText::new(format!("{} {}", icon::WARNING, spec.warning))
                    .small()
                    .color(WARNING),
            );
        }
    });
}

fn output_tab(ui: &mut egui::Ui, state: &mut CompileWindowState, facts: &Facts) {
    let Some(job) = &facts.job else {
        ui.add_space(12.0);
        ui.label(RichText::new("Nothing compiled yet. Press Compile to start.").color(MUTED));
        return;
    };
    let job = job.lock().unwrap_or_else(PoisonError::into_inner);

    // A strip of steps with their status, then the overall progress bar.
    ui.horizontal_wrapped(|ui| {
        for (index, step) in job.steps.iter().enumerate() {
            let (glyph, color) = status_icon(step.status);
            let time = step.time().map(clock).unwrap_or_default();
            let text = RichText::new(format!("{glyph} {} {} {time}", step_icon(step.kind), step.title)).color(color);
            let chip = ui
                .selectable_label(state.only_step == Some(index), text)
                .on_hover_text(format!(
                    "{}

Click to show only this step's output.",
                    step.command
                ));
            if chip.clicked() {
                state.only_step = if state.only_step == Some(index) {
                    None
                } else {
                    Some(index)
                };
            }
            ui.label(RichText::new(icon::CARET_RIGHT).color(MUTED));
        }
    });
    let running = job.steps.iter().find(|step| step.status == StepStatus::Running);
    let label = match (&job.outcome, running) {
        (None, Some(step)) => format!("{} {}", step.title, job.partial.trim()),
        (None, None) => "Starting".to_owned(),
        (Some(_), _) => format!("{} in {}", job.preset, clock(job.elapsed())),
    };
    ui.add(
        egui::ProgressBar::new(job.progress())
            .text(truncate(&label, 120))
            .animate(job.is_running()),
    );

    ui.horizontal(|ui| {
        ui.toggle_value(&mut state.follow, format!("{} Follow", icon::ARROW_LINE_DOWN))
            .on_hover_text("Keep showing the newest output");
        ui.toggle_value(&mut state.problems_only, format!("{} Problems only", icon::FUNNEL));
        if ui.button(format!("{} Copy log", icon::CLIPBOARD_TEXT)).clicked() {
            ui.ctx().copy_text(job.log_text());
        }
        ui.label(RichText::new(format!("{} lines", job.lines.len())).small().color(MUTED));
    });

    let lines: Vec<usize> = job
        .lines
        .iter()
        .enumerate()
        .filter(|(_, line)| state.only_step.is_none() || line.step == state.only_step)
        .filter(|(_, line)| {
            !state.problems_only || matches!(line.kind, LineKind::Warning | LineKind::Error | LineKind::Command)
        })
        .map(|(index, _)| index)
        .collect();
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
    egui::Frame::NONE
        .fill(Color32::from_gray(18))
        .inner_margin(6.0)
        .show(ui, |ui| {
            egui::ScrollArea::both()
                .id_salt("compile_log")
                .auto_shrink([false, false])
                .stick_to_bottom(state.follow)
                .show_rows(ui, row_height, lines.len(), |ui, rows| {
                    for row in rows {
                        let line = &job.lines[lines[row]];
                        let (color, prefix) = match line.kind {
                            LineKind::Command => (ACCENT, "> "),
                            LineKind::Info => (MUTED, "-- "),
                            LineKind::Warning => (WARNING, ""),
                            LineKind::Error => (ERROR, ""),
                            LineKind::Output => (Color32::from_gray(205), ""),
                        };
                        ui.label(RichText::new(format!("{prefix}{}", line.text)).monospace().color(color));
                    }
                });
        });
}

fn problems_tab(ui: &mut egui::Ui, facts: &Facts, commands: &mut Vec<Command>) {
    if facts.has_leak {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{} The map leaks.", icon::DROP))
                        .color(ERROR)
                        .strong(),
                );
                ui.label(
                    RichText::new("The red line runs from an entity out to the void. Seal the gap it passes through.")
                        .color(MUTED),
                );
            });
            ui.horizontal(|ui| {
                if ui.button(format!("{} Go to the leak", icon::CROSSHAIR)).clicked() {
                    commands.push(Command::GoToLeak);
                }
                if ui.button(format!("{} Hide the leak line", icon::EYE_SLASH)).clicked() {
                    commands.push(Command::ClearLeak);
                }
            });
        });
        ui.add_space(6.0);
    }
    let Some(job) = &facts.job else {
        ui.label(RichText::new("Problems from the last compile show here.").color(MUTED));
        return;
    };
    let job = job.lock().unwrap_or_else(PoisonError::into_inner);
    if job.problems.is_empty() {
        let text = if job.is_running() {
            "No problems so far."
        } else {
            "No problems."
        };
        ui.label(RichText::new(format!("{} {text}", icon::CHECK_CIRCLE)).color(GOOD));
        return;
    }
    let mut problems: Vec<_> = job.problems.iter().collect();
    problems.sort_by_key(|problem| std::cmp::Reverse(problem.severity));
    egui::ScrollArea::vertical()
        .id_salt("compile_problems")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Grid::new("problems")
                .num_columns(4)
                .striped(true)
                .spacing([10.0, 4.0])
                .show(ui, |ui| {
                    for problem in problems {
                        let (glyph, color) = match problem.severity {
                            Severity::Error => (icon::X_CIRCLE, ERROR),
                            Severity::Warning => (icon::WARNING, WARNING),
                        };
                        ui.label(RichText::new(glyph).color(color));
                        ui.label(
                            RichText::new(job.steps.get(problem.step).map_or("", |step| step.title.as_str()))
                                .color(MUTED),
                        );
                        ui.label(RichText::new(truncate(&problem.text, 140)).color(color))
                            .on_hover_text(&problem.text);
                        ui.horizontal(|ui| {
                            if let Some(brush) = problem.brush {
                                if ui.small_button(format!("{} Brush {brush}", icon::CUBE)).clicked() {
                                    commands.push(Command::SelectBrush(brush));
                                }
                            }
                            if let Some(position) = problem.position {
                                if ui.small_button(format!("{} Show", icon::CROSSHAIR)).clicked() {
                                    commands.push(Command::GoTo(position));
                                }
                            }
                        });
                        ui.end_row();
                    }
                });
        });
}

/// A path field with a browse button. Returns whether the path changed.
pub(super) fn path_field(ui: &mut egui::Ui, path: &mut Option<PathBuf>, width: f32, extension: &str) -> bool {
    let mut text = path.as_ref().map(|path| path.display().to_string()).unwrap_or_default();
    let mut changed = false;
    ui.horizontal(|ui| {
        if ui
            .add(
                egui::TextEdit::singleline(&mut text)
                    .desired_width(width)
                    .hint_text("automatic"),
            )
            .changed()
        {
            *path = (!text.trim().is_empty()).then(|| PathBuf::from(text.trim()));
            changed = true;
        }
        if ui.button(icon::FOLDER_OPEN).on_hover_text("Browse").clicked() {
            let mut dialog = rfd::FileDialog::new().add_filter("Program", &[extension]);
            if let Some(folder) = path.as_ref().and_then(|path| path.parent()) {
                dialog = dialog.set_directory(folder);
            }
            if let Some(picked) = dialog.pick_file() {
                *path = Some(picked);
                changed = true;
            }
        }
    });
    changed
}

pub(super) fn step_icon(kind: StepKind) -> &'static str {
    match kind {
        StepKind::Vbsp => icon::CUBE,
        StepKind::Vvis => icon::EYE,
        StepKind::Vrad => icon::SUN,
        StepKind::Vbspinfo => icon::INFO,
        StepKind::SourceCubemap => icon::GLOBE_SIMPLE,
        StepKind::Repack => icon::PACKAGE,
        StepKind::CopyToGame => icon::COPY,
        StepKind::RunGame => icon::GAME_CONTROLLER,
        StepKind::Custom => icon::TERMINAL_WINDOW,
    }
}

fn status_icon(status: StepStatus) -> (&'static str, Color32) {
    match status {
        StepStatus::Pending => (icon::CIRCLE, MUTED),
        StepStatus::Running => (icon::CIRCLE_NOTCH, ACCENT),
        StepStatus::Succeeded => (icon::CHECK_CIRCLE, GOOD),
        StepStatus::Failed => (icon::X_CIRCLE, ERROR),
        StepStatus::Skipped => (icon::MINUS_CIRCLE, MUTED),
        StepStatus::Cancelled => (icon::STOP_CIRCLE, MUTED),
    }
}

/// A duration as `4.2 s` under a minute, then `m:ss`, or `h:mm:ss` past an hour.
pub(super) fn clock(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        format!("{:.1} s", duration.as_secs_f32())
    } else if seconds >= 3600 {
        format!("{}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

/// `1 error`, `2 errors`.
pub(crate) fn count(number: usize, noun: &str) -> String {
    if number == 1 {
        format!("1 {noun}")
    } else {
        format!("{number} {noun}s")
    }
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        text.to_owned()
    } else {
        format!("{}...", text.chars().take(limit).collect::<String>())
    }
}

fn unique_name(presets: &[Preset], base: &str) -> String {
    let taken = |name: &str| presets.iter().any(|preset| preset.name.eq_ignore_ascii_case(name));
    if !taken(base) {
        return base.to_owned();
    }
    (2..)
        .map(|number| format!("{base} {number}"))
        .find(|name| !taken(name))
        .expect("some number is free")
}

/// Opens a folder in Explorer, creating it first so the window has something to show.
pub(super) fn open_folder(folder: &Path) {
    let _ = std::fs::create_dir_all(folder);
    let _ = std::process::Command::new("explorer").arg(folder).spawn();
}
