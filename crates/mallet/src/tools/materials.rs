//! The tools that show AI agents the game's materials: a contact sheet of thumbnails,
//! and everything about one material.

use std::sync::Arc;

use mallet_content::vtf::Image;
use mallet_content::{parallel_map, AlphaMode, Material, MaterialDetails, MaterialLoader, SourceKind, Surface};
use serde_json::{json, Value};

use super::{
    optional_flag, optional_string, string_arg, string_list, tool, whole_number, Filter, Job, Learned,
    NON_BRUSH_FOLDERS, NO_GAME,
};
use crate::editor::Editor;
use crate::lighting::TextureLights;
use crate::mcp::{Content, ToolResult};
use crate::model_render::Picture;
use crate::sheet::{columns_for, Behind, Sheet, Tile};

/// How many materials `preview_materials` shows when no limit is given, and at most.
const DEFAULT_SHOWN: u64 = 24;
const MAX_SHOWN: u64 = 64;
/// The side of a thumbnail on the sheet, in pixels: unless one is asked for, and the
/// range one may be asked for in.
const DEFAULT_THUMBNAIL: u64 = 128;
const THUMBNAIL_RANGE: (u64, u64) = (32, 512);
/// The side of the picture `describe_material` shows unless one is asked for.
const DEFAULT_PICTURE: u64 = 256;
/// World units per texel of a face with the texture scale Hammer gives new faces.
const DEFAULT_TEXTURE_SCALE: f64 = 0.25;

/// The `tools/list` entries of the material tools.
pub fn definitions() -> Vec<Value> {
    vec![
        tool(
            "preview_materials",
            "Show what materials look like: a picture of their thumbnails in a grid, each with its number and \
             name under it, and a list that gives every number's full name, its texture's size in pixels and \
             what is special about it (how it uses alpha, whether it is sky, nodraw or a tool texture, how many \
             faces of the map use it, a missing file). Pass materials to see exactly those, or else the game's \
             materials that pass filter, as list_materials lists them, limit at a time from offset. Use it to \
             pick a texture by its looks, and to check that the materials a map names exist.",
            json!({
                "materials": { "type": "array", "items": { "type": "string" }, "description": "The materials to show, such as brick/brickwall001a." },
                "filter": { "type": "string", "description": "Words that must all appear in the name. A word starting with ! must not appear." },
                "used_only": { "type": "boolean", "description": "Only materials the map's brushes use." },
                "all_folders": { "type": "boolean", "description": "Also model, particle and interface materials, which are left out otherwise." },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_SHOWN, "description": "Default 24." },
                "offset": { "type": "integer", "minimum": 0, "description": "How many matching materials to skip; default 0." },
                "size": { "type": "integer", "minimum": THUMBNAIL_RANGE.0, "maximum": THUMBNAIL_RANGE.1, "description": "Side of a thumbnail in pixels; default 128." }
            }),
            &[],
        ),
        tool(
            "describe_material",
            "Everything about one material: where it comes from, its shader, its textures and whether their \
             files exist, the texture's size in pixels and the units it covers at the default texture scale \
             of 0.25, how it uses alpha, what it is to the compilers (sky, nodraw or a tool texture), its \
             surface property, the light it gives off through lights.rad, how many faces of the open map use \
             it and its VMT as written, with a picture of its texture and of the other textures a blend \
             material fades into.",
            json!({
                "material": { "type": "string", "description": "The material, such as brick/brickwall001a." },
                "size": { "type": "integer", "minimum": THUMBNAIL_RANGE.0, "maximum": THUMBNAIL_RANGE.1, "description": "Side of the picture in pixels; default 256." },
                "picture": { "type": "boolean", "description": "false leaves the picture out. Default true." }
            }),
            &["material"],
        ),
    ]
}

/// A material as the tools name it: lower case, forward slashes, without `materials/`
/// or `.vmt`.
fn material_name(name: &str) -> String {
    let name = name.trim().replace('\\', "/").to_ascii_lowercase();
    let name = name.strip_prefix("materials/").unwrap_or(&name);
    name.strip_suffix(".vmt").unwrap_or(name).to_owned()
}

/// The side of a picture the `size` argument asks for, or `default`.
pub(super) fn side_arg(args: &Value, default: u64) -> Result<u32, String> {
    let side = whole_number(args, "size")?.unwrap_or(default);
    Ok(side.clamp(THUMBNAIL_RANGE.0, THUMBNAIL_RANGE.1) as u32)
}

/// A decoded texture as a picture for a sheet.
fn picture_of(image: Image) -> Picture {
    Picture {
        width: image.pixel_width,
        height: image.pixel_height,
        rgba: image.rgba,
    }
}

/// How a material uses its texture's alpha, in a word.
fn alpha_word(alpha: AlphaMode) -> &'static str {
    match alpha {
        AlphaMode::Opaque => "opaque",
        AlphaMode::Test(_) => "alpha test",
        AlphaMode::Blend(_) => "translucent",
        AlphaMode::Add { .. } => "additive",
    }
}

/// What a material's faces are to the compilers, in a word; `None` for ordinary ones.
fn surface_word(surface: Surface) -> Option<&'static str> {
    match surface {
        Surface::Normal => None,
        Surface::Sky => Some("sky"),
        Surface::Nodraw => Some("nodraw"),
        Surface::Tool => Some("tool"),
    }
}

/// Whether a material's alpha lets what is behind it show through in a thumbnail.
fn see_through(material: &Material) -> bool {
    matches!(material.alpha, AlphaMode::Test(_) | AlphaMode::Blend(_))
}

/// One material of a sheet: what it is, when the game has it, and its thumbnail or why
/// it has none.
struct Shown {
    name: String,
    found: Option<(Material, MaterialDetails)>,
    picture: Result<Picture, String>,
}

impl Shown {
    fn load(loader: &MaterialLoader, name: &str, side: u32) -> Self {
        let found = loader.details(name);
        let picture = match &found {
            None => Err("no such material".to_owned()),
            Some((_, details)) => loader
                .thumbnail(name, side)
                .map(picture_of)
                .ok_or_else(|| match &details.texture {
                    Some(texture) if !details.texture_found => format!("texture {texture} is missing"),
                    Some(texture) => format!("texture {texture} cannot be read"),
                    None => "names no texture".to_owned(),
                }),
        };
        Self {
            name: name.to_owned(),
            found,
            picture,
        }
    }

    fn tile(&self, number: usize) -> Tile {
        let (folder, leaf) = match self.name.rsplit_once('/') {
            Some((folder, leaf)) => (format!("{folder}/"), leaf),
            None => (String::new(), self.name.as_str()),
        };
        Tile {
            picture: self.picture.clone(),
            behind: match &self.found {
                Some((material, _)) if see_through(material) => Behind::Pattern,
                _ => Behind::Nothing,
            },
            caption: vec![format!("{number} {leaf}"), folder],
        }
    }

    fn entry(&self, number: usize, faces: Option<usize>) -> Value {
        let mut entry = json!({ "index": number, "material": self.name });
        if let Some((material, _)) = &self.found {
            entry["size"] = json!(material.size);
            if material.alpha != AlphaMode::Opaque {
                entry["alpha"] = json!(alpha_word(material.alpha));
            }
            if let Some(kind) = surface_word(material.surface) {
                entry["kind"] = json!(kind);
            }
        }
        if let Some(faces) = faces {
            entry["faces"] = json!(faces);
        }
        if let Err(problem) = &self.picture {
            entry["problem"] = json!(problem);
        }
        entry
    }
}

/// The `preview_materials` tool: gathers what it needs from the editor and leaves the
/// reading and drawing to the job.
pub fn preview(editor: &mut Editor, args: &Value) -> Result<Job, String> {
    let loader = editor.materials.as_ref().ok_or(NO_GAME)?.loader();
    let game = editor.game_dir().ok_or(NO_GAME)?.to_owned();
    let side = side_arg(args, DEFAULT_THUMBNAIL)?;
    let limit = whole_number(args, "limit")?
        .unwrap_or(DEFAULT_SHOWN)
        .clamp(1, MAX_SHOWN) as usize;
    let offset = whole_number(args, "offset")?.unwrap_or(0) as usize;
    let named = string_list(args, "materials")?;
    let filter = Filter::parse(optional_string(args, "filter")?.unwrap_or(""));
    let used_only = optional_flag(args, "used_only")? == Some(true);
    let all_folders = optional_flag(args, "all_folders")? == Some(true);
    let used: Vec<(String, usize)> = if used_only {
        editor
            .used_materials()
            .into_iter()
            .map(|(name, faces)| (material_name(&name), faces))
            .collect()
    } else {
        Vec::new()
    };
    let listed = editor.listed_materials();
    Ok(Box::new(move |learn| {
        let mut faces: Vec<Option<usize>> = Vec::new();
        let (total, names): (usize, Vec<String>) = if let Some(named) = named {
            let names: Vec<String> = named.iter().map(|name| material_name(name)).collect();
            (names.len(), names.into_iter().skip(offset).take(limit).collect())
        } else if used_only {
            let matching: Vec<&(String, usize)> = used.iter().filter(|(name, _)| filter.matches(name)).collect();
            let page = matching.iter().skip(offset).take(limit);
            faces = page.clone().map(|(_, faces)| Some(*faces)).collect();
            (matching.len(), page.map(|(name, _)| name.clone()).collect())
        } else {
            let list = listed.unwrap_or_else(|| {
                let list = Arc::new(loader.list());
                learn(Learned::Materials {
                    game,
                    list: Arc::clone(&list),
                });
                list
            });
            let matching: Vec<&String> = list
                .iter()
                .filter(|name| all_folders || !NON_BRUSH_FOLDERS.iter().any(|folder| name.starts_with(folder)))
                .filter(|name| filter.matches(name))
                .collect();
            (
                matching.len(),
                matching
                    .iter()
                    .skip(offset)
                    .take(limit)
                    .map(|name| (*name).clone())
                    .collect(),
            )
        };
        let shown = parallel_map(&names, |name| Shown::load(&loader, name, side));
        let entries: Vec<Value> = shown
            .iter()
            .enumerate()
            .map(|(index, shown)| shown.entry(index + 1, faces.get(index).copied().flatten()))
            .collect();
        let columns = columns_for(side, shown.len());
        let mut summary = json!({
            "total": total,
            "offset": offset,
            "shown": shown.len(),
            "materials": entries,
        });
        if offset + shown.len() < total {
            summary["next_offset"] = json!(offset + shown.len());
        }
        let mut content = vec![Content::Text(
            serde_json::to_string_pretty(&summary).unwrap_or_default(),
        )];
        if !shown.is_empty() {
            let tiles: Vec<Tile> = shown
                .iter()
                .enumerate()
                .map(|(index, shown)| shown.tile(index + 1))
                .collect();
            match Sheet::of_tiles(&tiles, side, columns, 2).png() {
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

/// How many brush faces of the open map use a material, by its name as the tools name it.
fn faces_using(editor: &Editor, name: &str) -> usize {
    let Some(document) = &editor.document else {
        return 0;
    };
    document
        .vmf
        .all_entities()
        .flat_map(|entity| entity.solids())
        .flat_map(|solid| solid.sides())
        .filter(|side| material_name(&side.material) == name)
        .count()
}

/// The `describe_material` tool.
pub fn describe(editor: &mut Editor, args: &Value) -> Result<Job, String> {
    let loader = editor.materials.as_ref().ok_or(NO_GAME)?.loader();
    let name = material_name(string_arg(args, "material")?);
    if name.is_empty() {
        return Err("\"material\" names no material".to_owned());
    }
    let side = side_arg(args, DEFAULT_PICTURE)?;
    let with_picture = optional_flag(args, "picture")? != Some(false);
    let lights: Arc<TextureLights> = Arc::clone(&editor.texture_lights);
    let faces = faces_using(editor, &name);
    Ok(Box::new(move |_| {
        let Some((material, details)) = loader.details(&name) else {
            return ToolResult::error(format!(
                "the game has no material \"{name}\" (materials/{name}.vmt); find one with list_materials"
            ));
        };
        let summary = description(&loader, &name, &material, &details, &lights, faces);
        let mut content = vec![Content::Text(
            serde_json::to_string_pretty(&summary).unwrap_or_default(),
        )];
        if with_picture {
            let mut tiles = vec![Shown::load(&loader, &name, side).tile(1)];
            if let Some(size) = material.size {
                tiles[0].caption = vec![
                    details.texture.clone().unwrap_or_else(|| name.clone()),
                    format!("{} x {}", size[0], size[1]),
                ];
            }
            for (number, texture) in details.blend_textures.iter().enumerate() {
                tiles.push(Tile {
                    picture: loader
                        .texture_image(texture, side)
                        .map(picture_of)
                        .ok_or_else(|| format!("texture {texture} is missing")),
                    behind: Behind::Nothing,
                    caption: vec![format!("texture {}", number + 2), texture.clone()],
                });
            }
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

/// Everything `describe_material` says about a material.
fn description(
    loader: &MaterialLoader,
    name: &str,
    material: &Material,
    details: &MaterialDetails,
    lights: &TextureLights,
    faces: usize,
) -> Value {
    let source = loader.material_source(name).map(|source| {
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
    let mut problems: Vec<String> = Vec::new();
    match &details.texture {
        Some(texture) if !details.texture_found => {
            problems.push(format!(
                "Its texture materials/{texture}.vtf is missing, so it shows as a checkerboard."
            ));
        }
        None if material.water.is_none() => problems.push("It names no texture.".to_owned()),
        _ => {}
    }
    let mut summary = json!({
        "material": name,
        "source": source,
        "file": loader.locate(name).map(|path| path.display().to_string()),
        "shader": details.shader,
        "texture": details.texture,
        "texture_found": details.texture_found,
        "size": material.size,
        "size_in_units": material.size.map(|size| size.map(|side| f64::from(side) * DEFAULT_TEXTURE_SCALE)),
        "alpha": alpha_word(material.alpha),
        "kind": surface_word(material.surface).unwrap_or("normal"),
        "surface_prop": details.surface_prop,
        "texture_light": lights.get(name).map(|light| json!({ "ldr": light.ldr.to_array(), "hdr": light.hdr.to_array() })),
        "faces_in_map": faces,
        "vmt": details.text,
    });
    match material.alpha {
        AlphaMode::Test(reference) => summary["alpha_reference"] = json!(reference),
        AlphaMode::Blend(opacity) | AlphaMode::Add { opacity, .. } if opacity < 1.0 => {
            summary["opacity"] = json!(opacity);
        }
        _ => {}
    }
    if !details.blend_textures.is_empty() {
        summary["blend_textures"] = json!(details.blend_textures);
        summary["four_texture_blend"] = json!(material.four_way);
    }
    if let Some(bump_map) = &details.bump_map {
        summary["bump_map"] = json!(bump_map);
    }
    if !details.keywords.is_empty() {
        summary["keywords"] = json!(details.keywords);
    }
    if let Some(rectangle_map) = &details.rectangle_map {
        summary["hotspot_file"] = json!(format!("materials/{rectangle_map}.rect"));
    }
    let traits: Vec<&str> = [
        (material.unlit, "unlit"),
        (material.self_illum.is_some(), "self-illuminated"),
        (details.envmap, "reflective"),
        (material.water.is_some(), "water"),
    ]
    .into_iter()
    .filter_map(|(set, word)| set.then_some(word))
    .collect();
    if !traits.is_empty() {
        summary["traits"] = json!(traits);
    }
    if material.decal_scale != 1.0 {
        summary["decal_scale"] = json!(material.decal_scale);
    }
    if !problems.is_empty() {
        summary["problems"] = json!(problems);
    }
    summary
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use glam::DVec3;
    use serde_json::json;

    use super::super::test_game::{error_of, json_of, picture_of, TestGame};
    use super::super::{call, run};

    fn game(name: &str) -> TestGame {
        let game = TestGame::new(name);
        game.material(
            "test/wall",
            "LightmappedGeneric\n{\n\t\"$basetexture\" \"Test/Wall\"\n\t\"$surfaceprop\" \"Brick\"\n}\n",
            Some((64, 32, [200, 60, 40, 255])),
        );
        game.material(
            "test/glass",
            "LightmappedGeneric\n{\n\t\"$basetexture\" \"test/glass\"\n\t\"$translucent\" \"1\"\n}\n",
            Some((16, 16, [90, 140, 220, 96])),
        );
        game.material(
            "test/broken",
            "LightmappedGeneric\n{\n\t\"$basetexture\" \"test/nowhere\"\n}\n",
            None,
        );
        game.material(
            "tools/toolsnodraw",
            "LightmappedGeneric\n{\n\t\"%compilenodraw\" \"1\"\n}\n",
            None,
        );
        game.material(
            "models/test/crate",
            "VertexLitGeneric\n{\n\t\"$basetexture\" \"test/wall\"\n}\n",
            None,
        );
        game
    }

    #[test]
    fn named_materials_come_back_numbered_with_a_picture() {
        let game = game("preview-named");
        let mut editor = game.editor();
        let args = json!({ "materials": ["TEST\\WALL", "test/glass", "test/broken", "test/none"], "size": 64 });
        let result = run(&mut editor, "preview_materials", &args);
        let summary = json_of(&result);
        assert_eq!(summary["total"], 4);
        assert_eq!(summary["shown"], 4);
        let materials = summary["materials"].as_array().unwrap();
        assert_eq!(
            materials[0],
            json!({ "index": 1, "material": "test/wall", "size": [64, 32] })
        );
        assert_eq!(materials[1]["alpha"], "translucent");
        assert_eq!(materials[2]["problem"], "texture test/nowhere is missing");
        assert_eq!(
            materials[3],
            json!({ "index": 4, "material": "test/none", "problem": "no such material" })
        );
        let [width, height] = picture_of(&result).expect("a sheet is drawn");
        assert_eq!(width, 8 + 4 * (64 + 8));
        assert!(height > 64 && height < 140);
    }

    #[test]
    fn the_filter_pages_through_the_game_materials() {
        let game = game("preview-filter");
        let shared = Mutex::new(game.editor());
        let first = call(
            &shared,
            "preview_materials",
            &json!({ "filter": "test !broken", "limit": 1 }),
        );
        let summary = json_of(&first);
        assert_eq!(summary["total"], 2, "model materials are left out: {summary}");
        assert_eq!(summary["materials"][0]["material"], "test/glass");
        assert_eq!(summary["next_offset"], 1);
        assert!(shared.lock().unwrap().listed_materials().is_some(), "the list is kept");
        let rest = json_of(&call(
            &shared,
            "preview_materials",
            &json!({ "filter": "test !broken", "offset": 1 }),
        ));
        assert_eq!(rest["materials"][0]["material"], "test/wall");
        assert!(rest.get("next_offset").is_none());
        let all = json_of(&call(
            &shared,
            "preview_materials",
            &json!({ "filter": "crate", "all_folders": true }),
        ));
        assert_eq!(all["materials"][0]["material"], "models/test/crate");
        let none = call(&shared, "preview_materials", &json!({ "filter": "nothing-like-this" }));
        assert_eq!(json_of(&none)["total"], 0);
        assert!(picture_of(&none).is_none(), "an empty sheet is not drawn");
    }

    #[test]
    fn used_materials_come_with_their_face_counts() {
        let game = game("preview-used");
        let mut editor = game.editor();
        editor.material = "TEST/WALL".to_owned();
        editor.create_block(DVec3::ZERO, DVec3::splat(64.0)).unwrap();
        let summary = json_of(&run(&mut editor, "preview_materials", &json!({ "used_only": true })));
        assert_eq!(summary["total"], 1);
        assert_eq!(summary["materials"][0]["material"], "test/wall");
        assert_eq!(summary["materials"][0]["faces"], 6);
    }

    #[test]
    fn a_material_is_described_in_full() {
        let game = game("describe");
        let mut editor = game.editor();
        editor.material = "test/wall".to_owned();
        editor.create_block(DVec3::ZERO, DVec3::splat(64.0)).unwrap();
        let result = run(
            &mut editor,
            "describe_material",
            &json!({ "material": "materials/Test/Wall.vmt" }),
        );
        let summary = json_of(&result);
        assert_eq!(summary["material"], "test/wall");
        assert_eq!(summary["shader"], "lightmappedgeneric");
        assert_eq!(summary["texture"], "test/wall");
        assert_eq!(summary["texture_found"], true);
        assert_eq!(summary["size"], json!([64, 32]));
        assert_eq!(summary["size_in_units"], json!([16.0, 8.0]));
        assert_eq!(summary["alpha"], "opaque");
        assert_eq!(summary["kind"], "normal");
        assert_eq!(summary["surface_prop"], "brick");
        assert_eq!(summary["faces_in_map"], 6);
        assert_eq!(summary["source"]["kind"], "game");
        assert!(summary["vmt"].as_str().unwrap().contains("$surfaceprop"));
        assert!(summary["file"].as_str().unwrap().ends_with("wall.vmt"));
        assert!(picture_of(&result).is_some());

        let nodraw = run(
            &mut editor,
            "describe_material",
            &json!({ "material": "tools/toolsnodraw", "picture": false }),
        );
        assert_eq!(json_of(&nodraw)["kind"], "nodraw");
        assert_eq!(json_of(&nodraw)["problems"][0], "It names no texture.");
        assert!(picture_of(&nodraw).is_none());

        let missing = run(&mut editor, "describe_material", &json!({ "material": "test/none" }));
        assert!(error_of(&missing).contains("no material \"test/none\""));
    }

    #[test]
    fn content_tools_need_a_game_folder() {
        let mut editor = crate::editor::Editor::default();
        editor.new_map();
        let result = run(&mut editor, "preview_materials", &json!({}));
        assert!(error_of(&result).contains("choose a game folder first"));
    }
}
