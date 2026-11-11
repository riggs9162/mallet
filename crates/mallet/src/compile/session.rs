//! The editor's side of compiling: picking the programs, saving the map, starting and
//! cancelling the job, and reacting when it ends (log file, leak line).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use eframe::egui::Color32;
use glam::DVec3;
use mallet_vmf::geometry::Bounds;

use super::runner::read_pointfile;
use super::{plan, save_presets, CompileJob, Context, Outcome, Preset, StepKind};
use crate::config::{self, ToolKind};
use crate::document::MapFormat;
use crate::editor::Editor;
use crate::scene::{next_generation, WireMesh};

/// The default folder beside a `.mallet` map that holds the VMF copy its compiles read.
pub const COMPILE_FOLDER: &str = "mallet_compile";

/// The colour of the leak line, as in Hammer.
const POINTFILE_COLOR: Color32 = Color32::from_rgb(255, 40, 40);

impl Editor {
    /// The program used for a tool: the one chosen in Mallet Options, or the best one
    /// found next to the game.
    pub fn tool_path(&self, tool: ToolKind) -> Option<PathBuf> {
        self.config
            .tools
            .get(tool)
            .filter(|path| !path.as_os_str().is_empty())
            .cloned()
            .or_else(|| tool.candidates(self.game_dir()).into_iter().next())
    }

    /// What a compile of the open map would be about, or why it cannot run. The
    /// compilers read VMF only, so a `.mallet` map is compiled from a VMF copy in a
    /// folder beside it (`mallet_compile` unless Mallet Options names another), which
    /// [`Editor::start_compile`] writes; the BSP and logs land there too.
    pub fn compile_context(&self) -> Result<Context, String> {
        let document = self.document.as_ref().ok_or("no map is open")?;
        let path = document
            .path
            .clone()
            .ok_or("save the map first, so the compilers have a map to read")?;
        let vmf = match document.format() {
            MapFormat::Vmf => path,
            MapFormat::Mallet => {
                let name = path.file_stem().unwrap_or_default();
                let folder = path
                    .parent()
                    .unwrap_or(Path::new(""))
                    .join(self.config.compile.folder_name());
                folder.join(name).with_extension("vmf")
            }
        };
        let game_dir = self
            .game_dir()
            .map(|dir| dir.components().collect::<PathBuf>())
            .ok_or("choose a game folder first (Edit > Mallet Options > Game)")?;
        Ok(Context { vmf, game_dir })
    }

    pub fn preset(&self, name: &str) -> Option<&Preset> {
        self.compile_presets
            .iter()
            .find(|preset| preset.name.eq_ignore_ascii_case(name))
    }

    /// The command each enabled step of a preset would run, or why one cannot.
    pub fn preview_preset(&self, preset: &Preset) -> Result<Vec<String>, String> {
        let context = self.compile_context()?;
        let steps = plan(preset, &context, &|tool| self.tool_path(tool))?;
        Ok(steps.iter().map(|step| step.action.display()).collect())
    }

    pub fn compile_running(&self) -> bool {
        self.compile_job.as_ref().is_some_and(CompileJob::is_running)
    }

    /// Saves the map if needed and starts compiling it with a preset.
    pub fn start_compile(&mut self, preset_name: &str) -> Result<(), String> {
        if self.compile_running() {
            return Err("a compile is already running".to_owned());
        }
        let preset = self
            .preset(preset_name)
            .cloned()
            .ok_or_else(|| format!("there is no preset named \"{preset_name}\""))?;
        let dirty = self.document.as_ref().is_some_and(|document| document.dirty);
        if dirty && self.config.compile.save_before && !self.is_untitled() {
            self.save(None)?;
        }
        let context = self.compile_context()?;
        let steps = plan(&preset, &context, &|tool| self.tool_path(tool))?;
        self.write_compile_copy(&context)?;
        let pointfile = context.vmf.with_extension("lin");
        let notes = if preset
            .steps
            .iter()
            .any(|step| step.enabled && step.kind == StepKind::Vbsp)
        {
            self.leak_check_notes()
        } else {
            Vec::new()
        };
        self.clear_pointfile();
        self.compile_job = Some(CompileJob::start(
            context.map_name(),
            preset.name.clone(),
            steps,
            pointfile,
            self.config.compile.low_priority,
            self.config.compile.line_limit(),
            notes,
        ));
        if self.config.compile.last_preset != preset.name {
            self.config.compile.last_preset = preset.name.clone();
            let _ = self.config.save();
        }
        self.message = format!("Compiling {} with {}", context.map_name(), preset.name);
        Ok(())
    }

    /// Writes the VMF the compilers read, when the map itself is a `.mallet` file. The copy
    /// is of the map as it is now, saved or not.
    fn write_compile_copy(&self, context: &Context) -> Result<(), String> {
        let Some(document) = &self.document else {
            return Ok(());
        };
        if document.format() != MapFormat::Mallet {
            return Ok(());
        }
        if let Some(folder) = context.vmf.parent() {
            fs::create_dir_all(folder).map_err(|error| format!("could not create {}: {error}", folder.display()))?;
        }
        document
            .export(&context.vmf)
            .map_err(|error| format!("could not write {} for the compilers: {error}", context.vmf.display()))
    }

    /// Problems VBSP's leak check will have, found before compiling: it needs a point
    /// entity inside the map, and skips any at exactly 0 0 0.
    pub fn leak_check_notes(&self) -> Vec<(String, Option<DVec3>)> {
        let Some(document) = &self.document else {
            return Vec::new();
        };
        let origins: Vec<(String, DVec3)> = document
            .vmf
            .entities()
            .filter(|entity| entity.solids().next().is_none())
            .filter_map(|entity| Some((entity.classname().to_owned(), entity.origin()?)))
            .collect();
        if origins.iter().any(|(_, origin)| *origin != DVec3::ZERO) {
            return Vec::new();
        }
        match origins.first() {
            None => vec![(
                concat!(
                    "The map has no point entities. VBSP needs one inside the map, such as an ",
                    "info_player_start, or it reports a leak."
                )
                .to_owned(),
                None,
            )],
            Some((classname, _)) => vec![(
                format!(
                    "The {classname} is at exactly 0 0 0, which VBSP's leak check skips, and no other entity is \
                     placed, so VBSP will report a leak. Move it off the origin, even to 0 0 1."
                ),
                Some(DVec3::ZERO),
            )],
        }
    }

    pub fn cancel_compile(&mut self) -> bool {
        match &self.compile_job {
            Some(job) if job.is_running() => {
                job.cancel();
                self.message = "Cancelling the compile".to_owned();
                true
            }
            _ => false,
        }
    }

    /// Reacts once to a compile that has just ended: writes its log, shows the leak line
    /// and reports the outcome. Returns the outcome the first time it is seen.
    pub fn poll_compile(&mut self) -> Option<Outcome> {
        let job = self.compile_job.as_ref()?;
        let (outcome, leaked, pointfile, log, map, errors, warnings, seconds) = {
            let mut state = job.lock();
            if state.handled {
                return None;
            }
            let outcome = state.outcome.clone()?;
            state.handled = true;
            (
                outcome,
                state.leaked,
                state.pointfile.clone(),
                self.config
                    .compile
                    .keep_logs
                    .then(|| (state.log_text(), state.started_at)),
                state.map.clone(),
                state.errors(),
                state.warnings(),
                state.elapsed().as_secs_f32(),
            )
        };
        if let Some((text, started_at)) = log {
            if let Some(folder) = config::data_dir().map(|dir| dir.join("logs")) {
                let stamp = started_at.duration_since(UNIX_EPOCH).map_or(0, |time| time.as_secs());
                if fs::create_dir_all(&folder).is_ok() {
                    let _ = fs::write(folder.join(format!("{map}-{stamp}.log")), text);
                }
            }
        }
        if leaked && self.config.compile.load_pointfile {
            if let Some(points) = read_pointfile(&pointfile) {
                self.set_pointfile(points);
            }
        }
        self.message = match &outcome {
            Outcome::Succeeded => format!(
                "Compiled {map} in {seconds:.1} s ({}, {})",
                crate::app::count(errors, "error"),
                crate::app::count(warnings, "warning")
            ),
            Outcome::Failed(error) => format!("Compile of {map} failed: {error}"),
            Outcome::Cancelled => format!("Compile of {map} cancelled"),
        };
        Some(outcome)
    }

    pub fn save_compile_presets(&mut self) {
        if let Err(error) = save_presets(&self.compile_presets) {
            self.message = format!("Could not save compile presets: {error}");
        }
    }

    /// Shows a leak line in the views and points the 3D camera along it.
    pub fn set_pointfile(&mut self, points: Vec<DVec3>) {
        self.pointfile = Some(points);
        self.add_pointfile_to_scene();
        self.go_to_leak();
    }

    pub fn clear_pointfile(&mut self) {
        if self.pointfile.take().is_some() {
            self.scene
                .meshes
                .retain(|mesh| mesh.owner.is_some() || mesh.color != POINTFILE_COLOR);
            self.scene.geometry_generation = next_generation();
        }
    }

    /// Puts the camera at the start of the leak line, looking along it.
    pub fn go_to_leak(&mut self) {
        let Some(points) = &self.pointfile else { return };
        let (start, next) = (points[0], points[1]);
        let direction = (next - start).normalize_or_zero();
        self.camera.position = start - direction * 64.0;
        let flat = direction.truncate().length();
        self.camera.yaw = direction.y.atan2(direction.x).to_degrees();
        self.camera.pitch = direction.z.atan2(flat).to_degrees();
        let mut bounds = Bounds::EMPTY;
        for point in points {
            bounds.add_point(*point);
        }
        for index in 0..self.views.len() {
            let rect = self.viewport_rects[1];
            self.views[index].fit(&bounds, rect);
        }
    }

    /// Adds the leak line to a freshly built scene. It belongs to nothing, so it cannot
    /// be picked or selected.
    pub fn add_pointfile_to_scene(&mut self) {
        let Some(points) = &self.pointfile else { return };
        let edges: Vec<[DVec3; 2]> = points.windows(2).map(|pair| [pair[0], pair[1]]).collect();
        let mut bounds = Bounds::EMPTY;
        for point in points {
            bounds.add_point(*point);
        }
        self.scene.meshes.push(WireMesh {
            color: POINTFILE_COLOR,
            edges,
            faces: Vec::new(),
            bounds,
            planes: Vec::new(),
            owner: None,
            solid: None,
            selected: false,
            model: None,
            sprite: None,
            origin: None,
            first_face: None,
            decal: false,
        });
        self.scene.geometry_generation = next_generation();
    }
}
