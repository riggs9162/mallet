//! The sky browser: every 2D skybox among the game's materials, with a live 3D preview
//! to look around in. Dragging the preview turns the view, the wheel zooms, and it can
//! turn by itself. Picking a sky makes it the map's, and the 3D views show it at once
//! behind the map.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};

use eframe::egui::{self, RichText};
use egui_phosphor::regular as icon;
use glam::Vec3;
use mallet_content::{GameFs, MaterialLoader};
use mallet_vmf::light::SkyPaint;

use super::asset_browser::{accent, muted, thousands, warning, Search, SearchBox};
use super::soundscape_browser::list_steps;
use super::MalletApp;
use crate::editor::Editor;
use crate::sky_render::{self, Look, SkyImages, SkyView, PAINTED, SUFFIXES};

const LIST_WIDTH: f32 = 260.0;
const ROW_HEIGHT: f32 = 20.0;
/// Pixels across each skybox image the preview loads.
const FACE_SIDE: u32 = 1024;
/// How many loaded skies are kept, so stepping back to one shows it at once.
const CACHED_SKIES: usize = 4;
/// The most pixels the preview draws, whatever the window's size.
const MAX_PREVIEW_PIXELS: f32 = 1_200_000.0;
/// Degrees the preview turns each second while it turns by itself.
const SPIN_SPEED: f32 = 6.0;

/// One sky of the list.
struct Entry {
    /// The name as worldspawn's `skyname` takes it, lower case.
    name: String,
    /// How many of the six side images the game has.
    sides: usize,
    /// The game also has an `_hdr` version, which it shows with HDR on.
    hdr: bool,
}

/// What the browser asks for once it is drawn.
enum Command {
    Use(String),
    Copy(String),
    Reload,
}

/// What the preview last drew, so it is drawn again only when something changed.
#[derive(PartialEq)]
struct Drawn {
    name: String,
    loaded: bool,
    look: [u32; 3],
    size: [u32; 2],
    sun: Option<[f32; 7]>,
    paint: Option<SkyPaint>,
}

/// What the sky browser remembers between frames.
#[derive(Default)]
pub(super) struct SkyBrowserState {
    /// Whether the window was open last frame, to tell when it opens.
    was_open: bool,
    filter: String,
    filter_history: Vec<String>,
    focus_search: bool,
    /// The game folder the list was made for, and the list, sorted by name.
    listing: Option<(PathBuf, Vec<Entry>)>,
    listing_load: Option<mpsc::Receiver<Vec<Entry>>>,
    selected: Option<String>,
    /// Loaded skies by name, oldest first.
    cache: Vec<(String, Arc<SkyImages>)>,
    loading: Option<(String, mpsc::Receiver<SkyImages>)>,
    look: Look,
    spin: bool,
    hide_sun: bool,
    preview: Option<egui::TextureHandle>,
    drawn: Option<Drawn>,
    reveal: bool,
    follow: bool,
    window: Option<egui::Rect>,
}

impl MalletApp {
    /// Opens the sky browser on the map's sky.
    pub(super) fn open_sky_browser(&mut self, editor: &mut Editor) {
        editor.sky_browser_open = true;
        let state = &mut self.sky_browser;
        state.was_open = true;
        state.focus_search = true;
        let current = editor.scene.lighting.sky.name.trim().to_ascii_lowercase();
        if !current.is_empty() {
            state.selected = Some(current);
            state.reveal = true;
        }
    }

    pub(super) fn sky_browser_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if let Some(filter) = editor.sky_browser_filter.take() {
            self.sky_browser.filter = filter;
        }
        if !editor.sky_browser_open {
            if self.sky_browser.was_open {
                self.sky_browser.was_open = false;
                self.sky_browser.preview = None;
                self.sky_browser.drawn = None;
            }
            return;
        }
        let Some(materials) = editor.materials.as_ref() else {
            editor.sky_browser_open = false;
            editor.message = "Choose a game folder first, so there are skies to browse".to_owned();
            return;
        };
        let loader = materials.loader();
        let filesystem = materials.shared_filesystem();
        if !self.sky_browser.was_open {
            self.open_sky_browser(editor);
        }
        let state = &mut self.sky_browser;
        state.list(&filesystem);
        state.load_selected(&loader);
        let current = editor.scene.lighting.sky.name.trim().to_ascii_lowercase();
        let over_window = ctx
            .pointer_hover_pos()
            .zip(state.window)
            .is_some_and(|(pointer, window)| window.contains(pointer));
        let steps = if over_window && !ctx.wants_keyboard_input() {
            list_steps(ctx)
        } else {
            0
        };
        let mut open = true;
        let mut commands = Vec::new();
        let shown = egui::Window::new(format!("{} Skies: pick the map's 2D skybox", icon::CLOUD_SUN))
            .id(egui::Id::new("sky_browser"))
            .open(&mut open)
            .default_size([1000.0, 620.0])
            .max_height(ctx.screen_rect().height() - 60.0)
            .min_width(620.0)
            .min_height(340.0)
            .resizable(true)
            .collapsible(false)
            .show(ctx, |ui| {
                egui::TopBottomPanel::bottom("sky_browser_bar")
                    .show_inside(ui, |ui| state.bottom_bar(ui, &mut commands));
                egui::SidePanel::left("sky_browser_list")
                    .resizable(true)
                    .default_width(LIST_WIDTH)
                    .width_range(180.0..=420.0)
                    .show_inside(ui, |ui| state.list_pane(ui, &current, steps, &mut commands));
                egui::CentralPanel::default()
                    .show_inside(ui, |ui| state.preview_pane(ui, editor, &current, &mut commands));
            });
        state.window = shown.map(|shown| shown.response.rect);
        for command in commands {
            match command {
                Command::Use(name) => {
                    if let Err(error) = use_sky(editor, &name) {
                        editor.message = error;
                    }
                }
                Command::Copy(name) => {
                    editor.message = format!("Copied {name}");
                    editor.system_clipboard = Some(name);
                }
                Command::Reload => {
                    self.sky_browser.listing = None;
                    self.sky_browser.cache.clear();
                    self.sky_browser.drawn = None;
                }
            }
        }
        if !open {
            editor.sky_browser_open = false;
        }
    }
}

/// Makes `name` the map's sky: worldspawn's `skyname`, as one undoable step.
fn use_sky(editor: &mut Editor, name: &str) -> Result<(), String> {
    let world = editor
        .document
        .as_ref()
        .and_then(|document| document.vmf.world())
        .and_then(mallet_vmf::Entity::id)
        .ok_or("Open a map first, so there is a sky to set")?;
    editor.set_key_on(&[world], "skyname", name)?;
    editor.message = format!("The map's sky is now {name}");
    Ok(())
}

/// Every sky among the game's materials: the names whose side images are in
/// `materials/skybox`, without the `_hdr` versions of skies that have a plain one.
fn find_skies(filesystem: &GameFs) -> Vec<Entry> {
    let mut sides: BTreeMap<String, usize> = BTreeMap::new();
    for path in filesystem.list("materials/skybox", ".vmt") {
        let Some(name) = path
            .strip_prefix("materials/skybox/")
            .and_then(|name| name.strip_suffix(".vmt"))
        else {
            continue;
        };
        if let Some(base) = SUFFIXES.iter().find_map(|suffix| name.strip_suffix(suffix)) {
            if !base.is_empty() {
                *sides.entry(base.to_owned()).or_default() += 1;
            }
        }
    }
    let mut entries: Vec<Entry> = sides
        .iter()
        .filter(|(name, _)| name.strip_suffix("_hdr").is_none_or(|plain| !sides.contains_key(plain)))
        .map(|(name, count)| Entry {
            name: name.clone(),
            sides: *count,
            hdr: sides.contains_key(&format!("{name}_hdr")),
        })
        .collect();
    if !entries.iter().any(|entry| entry.name == PAINTED) && filesystem.exists("materials/skybox/painted.vmt") {
        entries.push(Entry {
            name: PAINTED.to_owned(),
            sides: 6,
            hdr: false,
        });
        entries.sort_by(|a, b| a.name.cmp(&b.name));
    }
    entries
}

impl SkyBrowserState {
    /// Looks through the game's skyboxes on another thread when the game folder changed,
    /// and takes the list in once it is ready.
    fn list(&mut self, filesystem: &Arc<GameFs>) {
        let game_dir = filesystem.game_dir().to_path_buf();
        let current = self.listing.as_ref().is_some_and(|(dir, _)| *dir == game_dir);
        if let Some(receiver) = &self.listing_load {
            match receiver.try_recv() {
                Ok(entries) => {
                    self.listing = Some((game_dir, entries));
                    self.listing_load = None;
                    self.reveal = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.listing_load = None,
            }
            return;
        }
        if current {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let filesystem = Arc::clone(filesystem);
        let spawned = std::thread::Builder::new()
            .name("mallet sky list".to_owned())
            .spawn(move || {
                let _ = sender.send(find_skies(&filesystem));
            });
        if spawned.is_ok() {
            self.listing_load = Some(receiver);
        }
    }

    /// The loaded images of the sky `name`, when they are loaded.
    fn cached(&self, name: &str) -> Option<Arc<SkyImages>> {
        self.cache
            .iter()
            .find(|(cached, _)| cached == name)
            .map(|(_, images)| Arc::clone(images))
    }

    /// Takes in the sky that finished loading, and starts loading the selected one when
    /// it is not loaded yet.
    fn load_selected(&mut self, loader: &MaterialLoader) {
        if let Some((name, receiver)) = self.loading.take() {
            match receiver.try_recv() {
                Ok(images) => {
                    self.cache.retain(|(cached, _)| *cached != name);
                    self.cache.push((name, Arc::new(images)));
                    if self.cache.len() > CACHED_SKIES {
                        self.cache.remove(0);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.loading = Some((name, receiver));
                    return;
                }
                Err(mpsc::TryRecvError::Disconnected) => {}
            }
        }
        let Some(name) = self.selected.clone().filter(|name| name != PAINTED) else {
            return;
        };
        if self.cached(&name).is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let loader = loader.clone();
        let sky = name.clone();
        let spawned = std::thread::Builder::new()
            .name("mallet sky preview".to_owned())
            .spawn(move || {
                let _ = sender.send(SkyImages::load(&loader, &sky, FACE_SIDE));
            });
        if spawned.is_ok() {
            self.loading = Some((name, receiver));
        }
    }

    fn filtered(&self) -> Vec<usize> {
        let Some((_, entries)) = &self.listing else {
            return Vec::new();
        };
        let search = Search::parse(&self.filter);
        entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| search.matches(&entry.name))
            .map(|(index, _)| index)
            .collect()
    }

    /// The list of skies: a click previews one, a double-click makes it the map's, and
    /// `steps` rows down (or up, when negative) is where the arrow keys move.
    fn list_pane(&mut self, ui: &mut egui::Ui, current: &str, steps: i32, commands: &mut Vec<Command>) {
        let shown = self.filtered();
        let Some((_, entries)) = &self.listing else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Looking through the game's skyboxes...");
            });
            return;
        };
        if entries.is_empty() {
            ui.label(RichText::new("The game has no skybox materials.").color(muted(ui)));
            return;
        }
        if shown.is_empty() {
            ui.label(RichText::new("No sky matches the filter.").color(muted(ui)));
            return;
        }
        let row_of = |selected: &Option<String>| {
            let selected = selected.as_ref()?;
            shown.iter().position(|index| entries[*index].name == *selected)
        };
        if steps != 0 {
            let last = shown.len() as i64 - 1;
            let row = match row_of(&self.selected) {
                Some(row) => (row as i64 + i64::from(steps)).clamp(0, last),
                None if steps > 0 => 0,
                None => last,
            };
            let name = entries[shown[row as usize]].name.clone();
            if self.selected.as_ref() != Some(&name) {
                self.selected = Some(name);
                self.follow = true;
            }
        }
        let selected_row = row_of(&self.selected);
        let mut area = egui::ScrollArea::vertical()
            .id_salt("sky_list")
            .auto_shrink([false, false]);
        if std::mem::take(&mut self.reveal) {
            if let Some(row) = selected_row {
                let spacing = ui.spacing().item_spacing.y;
                area = area.vertical_scroll_offset((row as f32 * (ROW_HEIGHT + spacing) - 60.0).max(0.0));
            }
        }
        let follow = std::mem::take(&mut self.follow);
        let mut followed = false;
        let mut clicked = None;
        area.show_rows(ui, ROW_HEIGHT, shown.len(), |ui, rows| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            for row in rows {
                let entry = &entries[shown[row]];
                let chosen = self.selected.as_deref() == Some(entry.name.as_str());
                let in_use = entry.name == current;
                ui.horizontal(|ui| {
                    ui.set_height(ROW_HEIGHT);
                    let mut text = RichText::new(&entry.name);
                    if in_use && !chosen {
                        text = text.color(accent(ui)).strong();
                    } else if in_use {
                        text = text.strong();
                    }
                    let label = ui.selectable_label(chosen, text);
                    if follow && chosen {
                        label.scroll_to_me(None);
                        followed = true;
                    }
                    if label.double_clicked() {
                        commands.push(Command::Use(entry.name.clone()));
                    } else if label.clicked() {
                        clicked = Some(entry.name.clone());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if in_use {
                            ui.label(RichText::new("in use").small().color(muted(ui)));
                        }
                        if entry.sides < SUFFIXES.len() && entry.name != PAINTED {
                            ui.label(RichText::new(icon::WARNING).color(warning(ui)))
                                .on_hover_text(format!("Only {} of the six sides are in the game", entry.sides));
                        }
                        if entry.hdr {
                            ui.label(RichText::new("HDR").small().color(muted(ui)))
                                .on_hover_text("Has an HDR version, which the game shows with HDR on");
                        }
                    });
                });
            }
        });
        if let Some(name) = clicked {
            self.selected = Some(name);
        }
        if follow && !followed && selected_row.is_some() {
            self.reveal = true;
            ui.ctx().request_repaint();
        }
    }

    /// The selected sky as seen from inside it, with the buttons to use it.
    fn preview_pane(&mut self, ui: &mut egui::Ui, editor: &Editor, current: &str, commands: &mut Vec<Command>) {
        let Some(name) = self.selected.clone() else {
            ui.add_space(8.0);
            ui.label(RichText::new("Click a sky to look around in it.").color(muted(ui)));
            return;
        };
        let painted = name == PAINTED;
        let images = (!painted).then(|| self.cached(&name)).flatten();
        let sky = &editor.scene.lighting.sky;
        let paint = sky.paint.unwrap_or_else(sky_render::default_paint);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(&name).strong().size(15.0));
            if name == current {
                ui.label(RichText::new("the map's sky").small().color(muted(ui)));
            } else if ui
                .button(format!("{} Use as the map's sky", icon::CHECK))
                .on_hover_text("Set the map's sky name to this sky. The 3D views show it behind the map.")
                .clicked()
            {
                commands.push(Command::Use(name.clone()));
            }
            if ui.button(icon::COPY).on_hover_text("Copy the name").clicked() {
                commands.push(Command::Copy(name.clone()));
            }
        });
        let details = match (&images, painted) {
            (_, true) if sky.paint.is_some() => {
                "Garry's Mod's painted sky, as the map's env_skypaint sets it".to_owned()
            }
            (_, true) => {
                "Garry's Mod's painted sky with env_skypaint's defaults; add an env_skypaint to change it".to_owned()
            }
            (Some(images), false) => images
                .resolution()
                .map_or_else(String::new, |[width, height]| format!("{width} x {height} per side")),
            (None, false) => String::new(),
        };
        let missing = images.as_ref().map(|images| images.missing()).unwrap_or_default();
        ui.horizontal_wrapped(|ui| {
            if !details.is_empty() {
                ui.label(RichText::new(details).small().color(muted(ui)));
            }
            if !missing.is_empty() {
                ui.label(
                    RichText::new(format!("Missing sides: {}", missing.join(", ")))
                        .small()
                        .color(warning(ui)),
                );
            }
        });
        let area = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(area, egui::Sense::click_and_drag());
        self.steer(ui, &response);
        let loading = !painted && images.is_none();
        let sun = if self.hide_sun {
            None
        } else if painted {
            Some((
                paint.sun_normal.as_vec3(),
                paint.sun_color.as_vec3() * 4.0,
                paint.sun_size.max(0.5) as f32,
            ))
        } else {
            sky.sun
                .map(|(toward, color, size)| (toward.as_vec3(), color.as_vec3(), size as f32))
        };
        let pixels_per_point = ui.ctx().pixels_per_point();
        let mut size = area.size() * pixels_per_point;
        let fit = (MAX_PREVIEW_PIXELS / (size.x * size.y).max(1.0)).sqrt().min(1.0);
        size *= fit;
        let size = [size.x.max(1.0) as u32, size.y.max(1.0) as u32];
        if !loading {
            self.draw_preview(ui.ctx(), &name, images.as_deref(), painted.then_some(&paint), sun, size);
        }
        let painter = ui.painter_at(area);
        painter.rect_filled(area, 0.0, egui::Color32::from_gray(12));
        if let (Some(texture), false) = (&self.preview, loading) {
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            painter.image(texture.id(), area, uv, egui::Color32::WHITE);
        }
        if loading {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
            painter.text(
                area.center(),
                egui::Align2::CENTER_CENTER,
                "Loading the sky...",
                egui::FontId::proportional(14.0),
                egui::Color32::from_gray(200),
            );
        }
        let hint = "Drag to look around, scroll to zoom, double-click to look ahead again";
        painter.text(
            area.left_bottom() + egui::vec2(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            hint,
            egui::FontId::proportional(11.0),
            egui::Color32::from_white_alpha(150),
        );
        let heading = format!(
            "{:.0}°  {:+.0}°  {:.0}° wide",
            self.look.yaw, self.look.pitch, self.look.fov
        );
        painter.text(
            area.right_bottom() + egui::vec2(-8.0, -8.0),
            egui::Align2::RIGHT_BOTTOM,
            heading,
            egui::FontId::monospace(11.0),
            egui::Color32::from_white_alpha(150),
        );
    }

    /// Turns and zooms the view as the preview is dragged and scrolled, and turns it by
    /// itself when that is on.
    fn steer(&mut self, ui: &egui::Ui, response: &egui::Response) {
        let degrees_per_point = self.look.fov / response.rect.height().max(1.0);
        if response.dragged() {
            let delta = response.drag_delta();
            self.look.yaw += delta.x * degrees_per_point;
            self.look.pitch += delta.y * degrees_per_point;
        }
        if response.double_clicked() {
            self.look = Look {
                yaw: self.look.yaw,
                ..Look::default()
            };
        }
        if response.hovered() {
            let scroll = ui.input(|input| input.raw_scroll_delta.y);
            if scroll != 0.0 {
                self.look.fov *= (-scroll * 0.002).exp();
            }
        }
        if self.spin && !response.dragged() {
            let seconds = ui.input(|input| input.stable_dt).min(0.1);
            self.look.yaw += SPIN_SPEED * seconds;
            ui.ctx().request_repaint();
        }
        self.look = self.look.clamped();
    }

    /// Draws the preview into its texture when what it shows changed.
    fn draw_preview(
        &mut self,
        ctx: &egui::Context,
        name: &str,
        images: Option<&SkyImages>,
        paint: Option<&SkyPaint>,
        sun: Option<(Vec3, Vec3, f32)>,
        size: [u32; 2],
    ) {
        let drawn = Drawn {
            name: name.to_owned(),
            loaded: images.is_some(),
            look: [self.look.yaw, self.look.pitch, self.look.fov].map(f32::to_bits),
            size,
            sun: sun.map(|(toward, color, size)| [toward.x, toward.y, toward.z, color.x, color.y, color.z, size]),
            paint: paint.copied(),
        };
        if self.drawn.as_ref() == Some(&drawn) && self.preview.is_some() {
            return;
        }
        let view = SkyView { images, paint, sun };
        let pixels = sky_render::render(&view, self.look, size[0], size[1]);
        let image = egui::ColorImage::from_rgba_unmultiplied([size[0] as usize, size[1] as usize], &pixels);
        match &mut self.preview {
            Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
            None => self.preview = Some(ctx.load_texture("sky_preview", image, egui::TextureOptions::LINEAR)),
        }
        self.drawn = Some(drawn);
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui, commands: &mut Vec<Command>) {
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            SearchBox {
                hint: "such as day !hdr",
                help: "Every word must be in the sky's name. Start a word with ! to leave out skies that have it. \
                       * stands for any text and ? for one character.",
                width: 200.0,
            }
            .show(ui, &mut self.filter, &mut self.filter_history, &mut self.focus_search);
            ui.separator();
            ui.checkbox(&mut self.spin, "Turn slowly")
                .on_hover_text("Turn the preview round by itself");
            let mut sun = !self.hide_sun;
            if ui
                .checkbox(&mut sun, "Sun")
                .on_hover_text("Draw the sun where the map's env_sun, or the painted sky, puts it")
                .changed()
            {
                self.hide_sun = !sun;
            }
            ui.separator();
            if ui
                .button(format!("{} Reload", icon::ARROWS_CLOCKWISE))
                .on_hover_text("Look through the game's skyboxes again, after adding or changing one")
                .clicked()
            {
                commands.push(Command::Reload);
            }
        });
        ui.horizontal(|ui| {
            let text = match &self.listing {
                Some((_, entries)) => format!("{} skies", thousands(entries.len())),
                None => "Looking through the game's skyboxes...".to_owned(),
            };
            ui.label(RichText::new(text).small().color(muted(ui)));
            if let Some(name) = &self.selected {
                ui.separator();
                ui.label(RichText::new(name).monospace().small());
                if ui.small_button("Use as the map's sky").clicked() {
                    commands.push(Command::Use(name.clone()));
                }
            }
        });
    }
}
