//! The tools that show AI agents the game's models: a contact sheet of thumbnails, and
//! everything about one model with pictures of it from several sides.

use std::sync::Arc;

use mallet_content::{parallel_map, MaterialLoader, ModelDetails, ModelVariant, SourceKind};
use serde_json::{json, Value};

use super::materials::side_arg;
use super::{
    optional_flag, optional_number, optional_string, string_arg, string_list, tool, whole_number, Filter, Job, Learned,
    NO_GAME,
};
use crate::editor::Editor;
use crate::mcp::{Content, ToolResult};
use crate::model_render::{render, LoadedModel, Orbit};
use crate::sheet::{columns_for, Behind, Sheet, Tile};

/// How many models `preview_models` shows when no limit is given, and at most.
const DEFAULT_SHOWN: u64 = 16;
const MAX_SHOWN: u64 = 48;
/// The side of a thumbnail on the sheet unless one is asked for, in pixels.
const DEFAULT_THUMBNAIL: u64 = 160;
/// The side of each picture `describe_model` draws unless one is asked for.
const DEFAULT_PICTURE: u64 = 256;
/// The largest side of the textures a sheet's models are painted with, in pixels.
const SHEET_TEXTURE: u32 = 128;
/// The largest side of the textures `describe_model` paints with.
const DETAIL_TEXTURE: u32 = 512;
/// How many times larger pictures are drawn before they are averaged down.
const SAMPLES: u32 = 2;
/// The most animation sequences `describe_model` names.
const MAX_SEQUENCES: usize = 40;
/// The look thumbnails show: the first skin, and the first choice of every body group,
/// as an entity without `skin` and `body` keys shows.
const DEFAULT_VARIANT: ModelVariant = ModelVariant { skin: 0, body: Some(0) };
/// The prop classes a map usually places a model with.
const PROP_CLASSES: [&str; 3] = ["prop_static", "prop_dynamic", "prop_physics"];
/// The classes that take a model the game would remove from the class they stand in for.
const OVERRIDE_CLASSES: [&str; 2] = ["prop_dynamic_override", "prop_physics_override"];

/// The `tools/list` entries of the model tools.
pub fn definitions() -> Vec<Value> {
    vec![
        tool(
            "preview_models",
            "Show what models look like: a picture of them in a grid, each with its number and name under it, \
             and a list that gives every number's full path, its size along its forward, left and up axes in \
             units, which of prop_static, prop_dynamic and prop_physics it works as (use_as; the compiler or \
             the game removes it from the others, and prop_dynamic_override takes any model) and how many of \
             its materials are missing. Pass models to see exactly those, or else the game's models that pass \
             filter, as list_models lists them, limit at a time from offset. Use it to pick a prop by its \
             looks before placing it.",
            json!({
                "models": { "type": "array", "items": { "type": "string" }, "description": "The models to show, such as models/props_c17/oildrum001.mdl." },
                "filter": { "type": "string", "description": "Words that must all appear in the path. A word starting with ! must not appear." },
                "used_only": { "type": "boolean", "description": "Only models the map's entities use." },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_SHOWN, "description": "Default 16." },
                "offset": { "type": "integer", "minimum": 0, "description": "How many matching models to skip; default 0." },
                "size": { "type": "integer", "minimum": 32, "maximum": 512, "description": "Side of a thumbnail in pixels; default 160." },
                "yaw": { "type": "number", "description": "Degrees around the model the camera looks from; 0 faces its front. Default 35." },
                "pitch": { "type": "number", "description": "Degrees above the horizon the camera looks from. Default 18." }
            }),
            &[],
        ),
        tool(
            "describe_model",
            "Everything about one model: where it comes from, its box around the entity's origin (x forward, \
             y left, z up, before the entity's angles turn it), its size, how far its origin lies above its \
             lowest point, the prop classes it works as (use_as) and why the compiler or the game removes it \
             from the others (not_as), by the Source SDK's rules for $staticprop, prop data and collision \
             models, its skins, body groups, surface property, bones, animation sequences, triangles, \
             materials and which of them are missing, and how many entities of the open map use it; with \
             pictures of it in perspective, from the front, from the side and from above, or with yaw or \
             pitch one picture from there.",
            json!({
                "model": { "type": "string", "description": "The model, such as models/props_c17/oildrum001.mdl." },
                "skin": { "type": "integer", "minimum": 0, "description": "The skin to show, as an entity's skin key numbers them; default 0." },
                "body": { "type": "integer", "minimum": 0, "description": "The body groups to show, as an entity's body key packs them; default 0." },
                "yaw": { "type": "number", "description": "Draw one picture from this many degrees around the model; 0 faces its front." },
                "pitch": { "type": "number", "description": "Draw one picture from this many degrees above the horizon." },
                "size": { "type": "integer", "minimum": 32, "maximum": 512, "description": "Side of each picture in pixels; default 256." },
                "picture": { "type": "boolean", "description": "false leaves the pictures out. Default true." }
            }),
            &["model"],
        ),
    ]
}

/// A model's path as the tools name it: lower case, forward slashes, under `models/`
/// and ending in `.mdl`.
fn model_path(name: &str) -> String {
    let name = name.trim().replace('\\', "/").to_ascii_lowercase();
    let name = name.trim_start_matches('/');
    let name = name.strip_suffix(".mdl").unwrap_or(name);
    if name.starts_with("models/") {
        format!("{name}.mdl")
    } else {
        format!("models/{name}.mdl")
    }
}

/// A number rounded to a tenth, as sizes in units are given.
fn tenth(value: f32) -> f64 {
    (f64::from(value) * 10.0).round() / 10.0
}

/// Runs model work that must not take the tool down with it: a model the parser or the
/// painter chokes on becomes a reason to show.
fn guarded<T>(work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
        .unwrap_or_else(|_| Err("this model could not be drawn".to_owned()))
}

/// The camera the `yaw` and `pitch` arguments ask for, when either is given.
fn orbit_arg(args: &Value) -> Result<Option<Orbit>, String> {
    let (yaw, pitch) = (optional_number(args, "yaw")?, optional_number(args, "pitch")?);
    if yaw.is_none() && pitch.is_none() {
        return Ok(None);
    }
    let usual = Orbit::default();
    Ok(Some(
        Orbit {
            yaw: yaw.map_or(usual.yaw, |yaw| yaw as f32),
            pitch: pitch.map_or(usual.pitch, |pitch| pitch as f32),
            distance: 1.0,
        }
        .clamped(),
    ))
}

/// Those of `classes` that take a model, in order.
fn classes_taking(details: &ModelDetails, collision: bool, classes: &[&'static str]) -> Vec<&'static str> {
    classes
        .iter()
        .copied()
        .filter(|class| details.prop_class_problem(class, collision).is_none())
        .collect()
}

/// One model of a sheet: what was read of it, or why it could not be read.
struct Shown {
    path: String,
    loaded: Result<LoadedModel, String>,
    collision: bool,
}

impl Shown {
    fn load(loader: &MaterialLoader, path: &str, variant: ModelVariant, texture_side: u32) -> Self {
        Self {
            path: path.to_owned(),
            loaded: guarded(|| LoadedModel::load(loader, path, variant, texture_side)),
            collision: loader.model_has_collision(path),
        }
    }

    fn tile(&self, number: usize, orbit: Orbit, side: u32) -> Tile {
        let name = self.path.strip_prefix("models/").unwrap_or(&self.path);
        let name = name.strip_suffix(".mdl").unwrap_or(name);
        let (folder, leaf) = match name.rsplit_once('/') {
            Some((folder, leaf)) => (format!("{folder}/"), leaf),
            None => (String::new(), name),
        };
        Tile {
            picture: self
                .loaded
                .as_ref()
                .map_err(Clone::clone)
                .and_then(|model| guarded(|| Ok(render(model, orbit, side, side, SAMPLES)))),
            behind: Behind::Backdrop,
            caption: vec![format!("{number} {leaf}"), folder],
        }
    }

    fn entry(&self, number: usize, entities: Option<usize>) -> Value {
        let mut entry = json!({ "index": number, "model": self.path });
        match &self.loaded {
            Ok(model) => {
                entry["size"] = json!(model.size().map(tenth));
                entry["use_as"] = json!(classes_taking(&model.details, self.collision, &PROP_CLASSES));
                let missing = model.paints.iter().filter(|paint| paint.missing).count();
                if missing > 0 {
                    entry["missing_materials"] = json!(missing);
                }
            }
            Err(problem) => entry["problem"] = json!(problem),
        }
        if let Some(entities) = entities {
            entry["entities"] = json!(entities);
        }
        entry
    }
}

/// The `preview_models` tool: gathers what it needs from the editor and leaves the
/// reading and drawing to the job.
pub fn preview(editor: &mut Editor, args: &Value) -> Result<Job, String> {
    let loader = editor.materials.as_ref().ok_or(NO_GAME)?.loader();
    let game = editor.game_dir().ok_or(NO_GAME)?.to_owned();
    let side = side_arg(args, DEFAULT_THUMBNAIL)?;
    let limit = whole_number(args, "limit")?
        .unwrap_or(DEFAULT_SHOWN)
        .clamp(1, MAX_SHOWN) as usize;
    let offset = whole_number(args, "offset")?.unwrap_or(0) as usize;
    let named = string_list(args, "models")?;
    let filter = Filter::parse(optional_string(args, "filter")?.unwrap_or(""));
    let used_only = optional_flag(args, "used_only")? == Some(true);
    let orbit = orbit_arg(args)?.unwrap_or_default();
    let used = if used_only { editor.used_models() } else { Vec::new() };
    let listed = editor.listed_models();
    Ok(Box::new(move |learn| {
        let mut entities: Vec<Option<usize>> = Vec::new();
        let (total, paths): (usize, Vec<String>) = if let Some(named) = named {
            let paths: Vec<String> = named.iter().map(|name| model_path(name)).collect();
            (paths.len(), paths.into_iter().skip(offset).take(limit).collect())
        } else if used_only {
            let matching: Vec<&(String, usize)> = used.iter().filter(|(path, _)| filter.matches(path)).collect();
            let page = matching.iter().skip(offset).take(limit);
            entities = page.clone().map(|(_, entities)| Some(*entities)).collect();
            (matching.len(), page.map(|(path, _)| path.clone()).collect())
        } else {
            let list = listed.unwrap_or_else(|| {
                let models: Vec<String> = loader.list_models().into_iter().map(|(path, _)| path).collect();
                let list = Arc::new(models);
                learn(Learned::Models {
                    game,
                    list: Arc::clone(&list),
                });
                list
            });
            let matching: Vec<&String> = list.iter().filter(|path| filter.matches(path)).collect();
            (
                matching.len(),
                matching
                    .iter()
                    .skip(offset)
                    .take(limit)
                    .map(|path| (*path).clone())
                    .collect(),
            )
        };
        let numbered: Vec<(usize, &String)> = paths.iter().enumerate().collect();
        let drawn = parallel_map(&numbered, |(index, path)| {
            let shown = Shown::load(&loader, path, DEFAULT_VARIANT, SHEET_TEXTURE);
            (shown.tile(index + 1, orbit, side), shown)
        });
        let entries: Vec<Value> = drawn
            .iter()
            .enumerate()
            .map(|(index, (_, shown))| shown.entry(index + 1, entities.get(index).copied().flatten()))
            .collect();
        let mut summary = json!({
            "total": total,
            "offset": offset,
            "shown": entries.len(),
            "models": entries,
        });
        if offset + paths.len() < total {
            summary["next_offset"] = json!(offset + paths.len());
        }
        let mut content = vec![Content::Text(crate::mcp::json_text(&summary))];
        if !paths.is_empty() {
            let tiles: Vec<Tile> = drawn.into_iter().map(|(tile, _)| tile).collect();
            match Sheet::of_tiles(&tiles, side, columns_for(side, tiles.len()), 2).png() {
                Ok(png) => content.push(Content::Png(png)),
                Err(error) => return ToolResult::error(error),
            }
        }
        ToolResult {
            content,
            is_error: false,
        }
    }))
}

/// The pictures `describe_model` draws when no camera is asked for: what each is called,
/// the axes it shows, and where it is seen from.
const VIEWS: [(&str, &str, Orbit); 4] = [
    (
        "perspective",
        "from the front left",
        Orbit {
            yaw: 35.0,
            pitch: 18.0,
            distance: 1.0,
        },
    ),
    (
        "front",
        "+Y right, +Z up",
        Orbit {
            yaw: 0.0,
            pitch: 0.0,
            distance: 1.0,
        },
    ),
    (
        "side",
        "+X right, +Z up",
        Orbit {
            yaw: 270.0,
            pitch: 0.0,
            distance: 1.0,
        },
    ),
    (
        "top",
        "+X right, +Y up",
        Orbit {
            yaw: 270.0,
            pitch: 89.0,
            distance: 1.0,
        },
    ),
];

/// The `describe_model` tool.
pub fn describe(editor: &mut Editor, args: &Value) -> Result<Job, String> {
    let loader = editor.materials.as_ref().ok_or(NO_GAME)?.loader();
    let path = model_path(string_arg(args, "model")?);
    let side = side_arg(args, DEFAULT_PICTURE)?;
    let with_picture = optional_flag(args, "picture")? != Some(false);
    let variant = ModelVariant {
        skin: whole_number(args, "skin")?.unwrap_or(0) as usize,
        body: Some(whole_number(args, "body")?.unwrap_or(0) as u32),
    };
    let orbit = orbit_arg(args)?;
    let entities = editor
        .used_models()
        .into_iter()
        .find(|(used, _)| *used == path)
        .map_or(0, |(_, entities)| entities);
    Ok(Box::new(move |_| {
        let shown = Shown::load(&loader, &path, variant, DETAIL_TEXTURE);
        let model = match &shown.loaded {
            Ok(model) => model,
            Err(error) => {
                return ToolResult::error(format!(
                    "the model \"{path}\" could not be read: {error}; find one with list_models"
                ))
            }
        };
        let summary = description(&loader, &shown, model, variant, entities);
        let mut content = vec![Content::Text(crate::mcp::json_text(&summary))];
        if with_picture {
            let draw = |orbit: Orbit| guarded(|| Ok(render(model, orbit, side, side, SAMPLES)));
            let tiles: Vec<Tile> = match orbit {
                Some(orbit) => vec![Tile {
                    picture: draw(orbit),
                    behind: Behind::Backdrop,
                    caption: vec![format!("yaw {}, pitch {}", orbit.yaw, orbit.pitch)],
                }],
                None => parallel_map(&VIEWS, |(name, axes, orbit)| Tile {
                    picture: draw(*orbit),
                    behind: Behind::Backdrop,
                    caption: vec![(*name).to_owned(), (*axes).to_owned()],
                }),
            };
            match Sheet::of_tiles(&tiles, side, columns_for(side, tiles.len()), 2).png() {
                Ok(png) => content.push(Content::Png(png)),
                Err(error) => return ToolResult::error(error),
            }
        }
        ToolResult {
            content,
            is_error: false,
        }
    }))
}

/// Everything `describe_model` says about a model.
fn description(
    loader: &MaterialLoader,
    shown: &Shown,
    model: &LoadedModel,
    variant: ModelVariant,
    entities: usize,
) -> Value {
    let details = &model.details;
    let source = loader.model_source(&shown.path).map(|source| {
        json!({
            "name": source.name,
            "kind": match source.kind {
                SourceKind::Game => "game",
                SourceKind::MountedGame => "mounted game",
                SourceKind::Addon => "addon",
                SourceKind::Download => "download",
            },
            "path": source.path.display().to_string(),
        })
    });
    let mut materials: Vec<(&str, bool)> = model
        .paints
        .iter()
        .map(|paint| (paint.material.as_str(), paint.missing))
        .collect();
    materials.sort();
    materials.dedup();
    let body_groups: Vec<Value> = details
        .body_groups
        .iter()
        .map(|group| {
            json!(group
                .choices
                .iter()
                .map(|choice| choice.trim_end_matches(".smd"))
                .collect::<Vec<_>>())
        })
        .collect();
    let mut summary = json!({
        "model": shown.path,
        "source": source,
        "file": loader.locate_model(&shown.path).map(|path| path.display().to_string()),
        "bounds": { "min": model.model.min.map(tenth), "max": model.model.max.map(tenth) },
        "size": model.size().map(tenth),
        "origin_above_base": tenth(-model.model.min[2]),
        "static_prop": details.static_prop,
        "prop_data": details.prop_data.as_ref().map(|data| json!({
            "base": data.base,
            "allow_static": data.allow_static,
        })),
        "collision_model": shown.collision,
        "use_as": classes_taking(details, shown.collision, &PROP_CLASSES)
            .into_iter()
            .chain(classes_taking(details, shown.collision, &OVERRIDE_CLASSES))
            .collect::<Vec<_>>(),
        "not_as": PROP_CLASSES
            .iter()
            .filter_map(|class| Some(((*class).to_owned(), json!(details.prop_class_problem(class, shown.collision)?))))
            .collect::<serde_json::Map<String, Value>>(),
        "skins": details.skins,
        "skin_shown": variant.skin.min(details.skins.saturating_sub(1)),
        "body_groups": body_groups,
        "surface_prop": details.surface_prop,
        "bones": details.bones,
        "sequence_count": details.sequences.len(),
        "sequences": details.sequences.iter().take(MAX_SEQUENCES).collect::<Vec<_>>(),
        "triangles": model.model.triangle_count(),
        "vertices": details.vertices,
        "materials": materials
            .iter()
            .map(|(material, missing)| {
                let mut entry = json!({ "material": material });
                if *missing {
                    entry["missing"] = json!(true);
                }
                entry
            })
            .collect::<Vec<_>>(),
        "entities_in_map": entities,
    });
    if materials.iter().any(|(_, missing)| *missing) {
        summary["problems"] = json!(["Some of its materials or their textures are missing, so those parts show grey here and as a checkerboard in game."]);
    }
    summary
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::run;
    use super::super::test_game::{error_of, json_of, picture_of, TestGame};
    use super::*;

    #[test]
    fn model_names_are_made_whole() {
        assert_eq!(model_path("props_c17/oildrum001"), "models/props_c17/oildrum001.mdl");
        assert_eq!(
            model_path("Models\\Props_C17\\OilDrum001.MDL"),
            "models/props_c17/oildrum001.mdl"
        );
        assert_eq!(model_path("/models/a.mdl"), "models/a.mdl");
    }

    #[test]
    fn the_prop_classes_a_model_works_as_are_listed() {
        let plain = ModelDetails {
            static_prop: true,
            ..ModelDetails::default()
        };
        assert_eq!(
            classes_taking(&plain, true, &PROP_CLASSES),
            ["prop_static", "prop_dynamic"]
        );
        assert_eq!(
            classes_taking(&plain, false, &OVERRIDE_CLASSES),
            ["prop_dynamic_override"]
        );
        let physics = ModelDetails {
            static_prop: true,
            prop_data: Some(mallet_content::PropData::default()),
            ..ModelDetails::default()
        };
        assert_eq!(classes_taking(&physics, true, &PROP_CLASSES), ["prop_physics"]);
        assert!(classes_taking(&physics, false, &PROP_CLASSES).is_empty());
    }

    #[test]
    fn a_camera_is_only_made_when_asked_for() {
        assert_eq!(orbit_arg(&json!({})).unwrap(), None);
        let orbit = orbit_arg(&json!({ "yaw": 450, "pitch": 120 })).unwrap().unwrap();
        assert_eq!((orbit.yaw, orbit.pitch), (90.0, 89.0));
        assert_eq!(
            orbit_arg(&json!({ "pitch": 5 })).unwrap().unwrap().yaw,
            Orbit::default().yaw
        );
        assert!(orbit_arg(&json!({ "yaw": "left" })).is_err());
    }

    #[test]
    fn models_that_cannot_be_read_say_why() {
        let game = TestGame::new("models-missing");
        game.write("models/test/broken.mdl", b"not a model");
        let mut editor = game.editor();
        let result = run(
            &mut editor,
            "preview_models",
            &json!({ "models": ["test/none", "test/broken"], "size": 64 }),
        );
        let summary = json_of(&result);
        assert_eq!(summary["total"], 2);
        assert_eq!(summary["models"][0]["model"], "models/test/none.mdl");
        assert!(summary["models"][0]["problem"].as_str().unwrap().contains("not found"));
        assert!(summary["models"][1]["problem"].is_string());
        assert!(
            picture_of(&result).is_some(),
            "the sheet says why in each model's place"
        );

        let listed = json_of(&run(&mut editor, "preview_models", &json!({ "filter": "broken" })));
        assert_eq!(listed["total"], 1);
        assert!(editor.listed_models().is_some(), "the list is kept");

        let described = run(&mut editor, "describe_model", &json!({ "model": "test/none" }));
        assert!(error_of(&described).contains("could not be read"));
    }

    #[test]
    fn a_real_model_is_drawn_and_described() {
        let Some(dir) = std::env::var_os("MALLET_GAME_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        let mut editor = Editor::default();
        editor.new_map();
        editor.config.unmounted_games = mallet_content::mounted_games(&dir)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        editor.set_game_dir(&dir).unwrap();
        let model = "models/props_c17/oildrum001.mdl";
        let sheet = run(&mut editor, "preview_models", &json!({ "models": [model] }));
        let entry = &json_of(&sheet)["models"][0];
        assert!(
            entry["size"][2].as_f64().unwrap() > 30.0,
            "an oil drum is taller than 30 units: {entry}"
        );
        assert_eq!(picture_of(&sheet).map(|size| size[0]), Some(8 + 160 + 8));
        let described = run(&mut editor, "describe_model", &json!({ "model": model }));
        let summary = json_of(&described);
        assert_eq!(summary["static_prop"], true);
        assert_eq!(summary["prop_data"]["base"], "Metal.Medium");
        assert_eq!(
            summary["use_as"],
            json!(["prop_physics", "prop_dynamic_override", "prop_physics_override"])
        );
        assert!(summary["not_as"]["prop_static"].as_str().unwrap().contains("VBSP"));
        assert!(summary["triangles"].as_u64().unwrap() > 50);
        assert_eq!(picture_of(&described).map(|size| size[0]), Some(8 + 4 * (256 + 8)));
        let turned = run(
            &mut editor,
            "describe_model",
            &json!({ "model": model, "yaw": 180, "size": 128 }),
        );
        assert_eq!(picture_of(&turned).map(|size| size[0]), Some(8 + 128 + 8));
    }
}
