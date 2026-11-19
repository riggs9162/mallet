//! The tools that tell AI agents about soundscapes: the ones the game's scripts define,
//! what one of them plays, and which one a player hears at a point of the map.
//!
//! The first two read the scripts themselves, every time, so they need neither the sound
//! preview nor the soundscape browser and always show a script as it is on disk.

use std::collections::{BTreeMap, HashMap};

use glam::DVec3;
use mallet_vmf::sound::{player_hull, split_sound_name, OutlineKind, SoundscapeChooser, SoundscapeKind};
use serde_json::{json, Value};

use super::{optional_flag, optional_string, string_arg, tool, whole_number, Filter, Job, NO_GAME};
use crate::editor::Editor;
use crate::mcp::ToolResult;
use crate::sound::{learn_solid_materials, load_scripts, sight_blocked};

/// How many soundscape names `list_soundscapes` gives when no limit is asked for.
const DEFAULT_LISTED: u64 = 300;
/// The most points `soundscape_at` takes at once.
const MAX_POINTS: usize = 64;
/// How many of the nearest soundscape entities `soundscape_at` lists for each point.
const NEAREST_LISTED: usize = 6;
/// How many names like a soundscape that was not found an error suggests.
const SUGGESTIONS: usize = 8;

/// The `tools/list` entries of the soundscape tools.
pub fn definitions() -> Vec<Value> {
    let vec3 = json!({ "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 });
    vec![
        tool(
            "list_soundscapes",
            "The soundscapes the game's scripts define, script file by script file as the game reads them \
             (scripts/soundscapes_manifest.txt, then the open map's own scripts/soundscapes_<map>.txt), for an \
             env_soundscape's soundscape key; and the soundscapes the open map's entities name, with the \
             entities and whether a script defines each. The scripts are read afresh on every call, and \
             neither the sound preview nor the soundscape browser has to be on.",
            json!({
                "filter": { "type": "string", "description": "Words that must all appear in the name. A word starting with ! must not appear." },
                "used_only": { "type": "boolean", "description": "Only soundscapes the map's entities name." },
                "limit": { "type": "integer", "minimum": 1, "description": "Default 300." }
            }),
            &[],
        ),
        tool(
            "describe_soundscape",
            "What one soundscape plays, line by line as the game plays it: its room effect, its looping \
             sounds, its sets of random sounds with their timing and files (waves), and the soundscapes it \
             plays inside itself, each with its volume, pitch and where it is heard; the sound positions it \
             needs (position0 to \
             position7 of its env_soundscape); the sound files it names that the game does not have; the \
             entities of the open map that name it and the positions each lacks; and its script as written.",
            json!({ "name": { "type": "string", "description": "The soundscape's name, from list_soundscapes." } }),
            &["name"],
        ),
        tool(
            "soundscape_at",
            "Which soundscape a player hears at points of the open map, by the game's own rules: the nearest \
             enabled soundscape entity that is in range (its radius) and in sight (no solid brush in between), \
             or the soundscape of a trigger_soundscape the player stands in. A point is where the player's \
             eyes are, 64 units above the floor. Each answer is for a player who starts at that point; one \
             who walks there from a place with a soundscape keeps hearing that one where none reaches. Also \
             lists the nearest soundscape entities with their distance and whether they are in range and in \
             sight. Use it to find places a map's soundscapes do not cover.",
            json!({
                "points": { "type": "array", "items": vec3, "minItems": 1, "maxItems": MAX_POINTS, "description": "Where the player's eyes are." }
            }),
            &["points"],
        ),
    ]
}

/// A soundscape entity of the open map, as far as the soundscape tools need it.
struct MapEntity {
    entity: u32,
    soundscape: String,
    positions: [bool; 8],
}

/// The open map's soundscape entities that name a soundscape, and the map's name for
/// its own script file.
fn map_entities(editor: &Editor) -> (Vec<MapEntity>, String) {
    let entities = editor
        .scene
        .sounds
        .soundscapes
        .iter()
        .filter(|soundscape| !soundscape.soundscape.is_empty())
        .map(|soundscape| MapEntity {
            entity: soundscape.entity,
            soundscape: soundscape.soundscape.clone(),
            positions: soundscape.positions.map(|position| position.is_some()),
        })
        .collect();
    let map_name = editor
        .document
        .as_ref()
        .and_then(|document| document.path.as_ref())
        .and_then(|path| path.file_stem())
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    (entities, map_name)
}

/// The `list_soundscapes` tool: gathers what it needs from the editor and leaves the
/// reading to the job.
pub fn list(editor: &mut Editor, args: &Value) -> Result<Job, String> {
    let filesystem = editor.materials.as_ref().ok_or(NO_GAME)?.shared_filesystem();
    let filter = Filter::parse(optional_string(args, "filter")?.unwrap_or(""));
    let used_only = optional_flag(args, "used_only")? == Some(true);
    let limit = whole_number(args, "limit")?.unwrap_or(DEFAULT_LISTED).max(1) as usize;
    let (entities, map_name) = map_entities(editor);
    Ok(Box::new(move |_| {
        let scripts = load_scripts(&filesystem, &map_name, false);
        let mut users: BTreeMap<String, (String, Vec<u32>)> = BTreeMap::new();
        for entity in &entities {
            users
                .entry(entity.soundscape.to_ascii_lowercase())
                .or_insert_with(|| (entity.soundscape.clone(), Vec::new()))
                .1
                .push(entity.entity);
        }
        let mut files: Vec<(String, Vec<&str>)> = Vec::new();
        let mut total = 0;
        let mut shown = 0;
        for soundscape in &scripts.soundscapes.soundscapes {
            let wanted = filter.matches(&soundscape.name)
                && (!used_only || users.contains_key(&soundscape.name.to_ascii_lowercase()));
            if !wanted {
                continue;
            }
            total += 1;
            if shown >= limit {
                continue;
            }
            shown += 1;
            match files.last_mut() {
                Some((file, names)) if *file == soundscape.file => names.push(&soundscape.name),
                _ => files.push((soundscape.file.clone(), vec![&soundscape.name])),
            }
        }
        let in_map: Vec<Value> = users
            .values()
            .map(|(name, entities)| {
                json!({
                    "soundscape": name,
                    "entities": entities,
                    "defined": scripts.soundscapes.find(name).is_some(),
                })
            })
            .collect();
        let mut summary = json!({
            "script_files": scripts.soundscape_files,
            "total": total,
            "shown": shown,
            "files": files
                .iter()
                .map(|(file, names)| json!({ "file": file, "soundscapes": names }))
                .collect::<Vec<_>>(),
            "in_map": in_map,
        });
        if !scripts.errors.is_empty() {
            summary["problems"] = json!(scripts.errors);
        }
        ToolResult::json(&summary)
    }))
}

/// The `describe_soundscape` tool.
pub fn describe(editor: &mut Editor, args: &Value) -> Result<Job, String> {
    let filesystem = editor.materials.as_ref().ok_or(NO_GAME)?.shared_filesystem();
    let name = string_arg(args, "name")?.trim().to_owned();
    if name.is_empty() {
        return Err("\"name\" names no soundscape".to_owned());
    }
    let (entities, map_name) = map_entities(editor);
    Ok(Box::new(move |_| {
        let scripts = load_scripts(&filesystem, &map_name, false);
        let library = &scripts.soundscapes;
        let (Some(soundscape), Some(outline)) = (library.find(&name), library.outline(&name)) else {
            let wanted = name.to_ascii_lowercase();
            let alike: Vec<&str> = library
                .soundscapes
                .iter()
                .map(|soundscape| soundscape.name.as_str())
                .filter(|other| other.to_ascii_lowercase().contains(&wanted))
                .take(SUGGESTIONS)
                .collect();
            let hint = if alike.is_empty() {
                "find one with list_soundscapes".to_owned()
            } else {
                format!("did you mean one of: {}", alike.join(", "))
            };
            return ToolResult::error(format!(
                "none of the {} script files read defines the soundscape \"{name}\"; {hint}",
                scripts.soundscape_files
            ));
        };
        let mut present: HashMap<String, bool> = HashMap::new();
        let mut missing: Vec<&str> = Vec::new();
        let mut plays: Vec<Value> = Vec::new();
        for row in &outline.rows {
            let lost = row.wave.as_ref().is_some_and(|wave| {
                let (_, file) = split_sound_name(wave);
                !*present
                    .entry(file.clone())
                    .or_insert_with(|| filesystem.exists(&format!("sound/{file}")))
            });
            if let (true, Some(wave)) = (lost, &row.wave) {
                if !missing.contains(&wave.as_str()) {
                    missing.push(wave);
                }
            }
            let set = plays.last_mut().filter(|line| line["kind"] == "random");
            if let (OutlineKind::Wave, Some(set)) = (row.kind, set) {
                let lists: &[&str] = if lost { &["waves", "missing"] } else { &["waves"] };
                for list in lists {
                    match set[*list].as_array_mut() {
                        Some(waves) => waves.push(json!(row.text)),
                        None => set[*list] = json!([row.text]),
                    }
                }
                continue;
            }
            let mut line = json!({
                "depth": row.depth,
                "kind": match row.kind {
                    OutlineKind::Dsp => "dsp",
                    OutlineKind::Loop => "loop",
                    OutlineKind::Random => "random",
                    OutlineKind::Wave => "wave",
                    OutlineKind::Soundscape => "soundscape",
                    OutlineKind::Problem => "problem",
                },
                "text": row.text,
            });
            if !row.detail.is_empty() {
                line["detail"] = json!(row.detail);
            }
            if lost {
                line["missing"] = json!(true);
            }
            plays.push(line);
        }
        let users: Vec<Value> = entities
            .iter()
            .filter(|entity| entity.soundscape.eq_ignore_ascii_case(&soundscape.name))
            .map(|entity| {
                let lacking: Vec<i32> = outline
                    .positions
                    .iter()
                    .copied()
                    .filter(|position| {
                        !usize::try_from(*position).is_ok_and(|index| entity.positions.get(index) == Some(&true))
                    })
                    .collect();
                let mut user = json!({ "entity": entity.entity });
                if !lacking.is_empty() {
                    user["missing_positions"] = json!(lacking);
                }
                user
            })
            .collect();
        ToolResult::json(&json!({
            "soundscape": soundscape.name,
            "file": soundscape.file,
            "positions": outline.positions,
            "plays": plays,
            "missing_sounds": missing,
            "entities": users,
            "script": mallet_vmf::kv::write_block(&soundscape.block).replace("\r\n", "\n"),
        }))
    }))
}

/// The `soundscape_at` tool.
pub fn at_points(editor: &mut Editor, args: &Value) -> Result<ToolResult, String> {
    let error = || format!("\"points\" must be 1 to {MAX_POINTS} points of [x, y, z]");
    let points: Vec<DVec3> = args
        .get("points")
        .and_then(Value::as_array)
        .filter(|points| !points.is_empty() && points.len() <= MAX_POINTS)
        .ok_or_else(error)?
        .iter()
        .map(|point| {
            let numbers: Option<Vec<f64>> = point.as_array()?.iter().map(Value::as_f64).collect();
            let [x, y, z]: [f64; 3] = numbers?.try_into().ok()?;
            Some(DVec3::new(x, y, z))
        })
        .collect::<Option<Vec<DVec3>>>()
        .ok_or_else(error)?;
    if editor.document.is_none() {
        return Err("no map is open".to_owned());
    }
    let mut solid = HashMap::new();
    learn_solid_materials(&mut solid, &editor.scene, editor.materials.as_mut());
    let scene = &editor.scene;
    let map = &scene.sounds;
    let blocked = |from: DVec3, to: DVec3| sight_blocked(scene, &solid, from, to);
    let round = |value: f64| (value * 10.0).round() / 10.0;
    let answers: Vec<Value> = points
        .iter()
        .map(|ear| {
            let mut chooser = SoundscapeChooser::default();
            chooser.update(map, *ear, &blocked);
            let heard = chooser.audio();
            let mut nearest: Vec<(f64, Value)> = map
                .soundscapes
                .iter()
                .map(|soundscape| {
                    let distance = soundscape.origin.distance(*ear);
                    let entry = json!({
                        "entity": soundscape.entity,
                        "class": match soundscape.kind {
                            SoundscapeKind::Normal => "env_soundscape",
                            SoundscapeKind::Proxy => "env_soundscape_proxy",
                            SoundscapeKind::Triggerable => "env_soundscape_triggerable",
                        },
                        "soundscape": soundscape.soundscape,
                        "distance": round(distance),
                        "radius": soundscape.radius,
                        "in_range": soundscape.radius == -1.0 || soundscape.radius > distance,
                        "in_sight": !blocked(soundscape.origin, *ear),
                        "enabled": soundscape.enabled,
                    });
                    (distance, entry)
                })
                .collect();
            nearest.sort_by(|a, b| a.0.total_cmp(&b.0));
            let hull = player_hull(*ear);
            let triggers: Vec<u32> = map
                .triggers
                .iter()
                .filter(|trigger| trigger.enabled && trigger.touches(&hull))
                .map(|trigger| trigger.entity)
                .collect();
            json!({
                "point": ear.to_array(),
                "soundscape": heard.map(|audio| audio.soundscape.clone()),
                "entity": heard.and_then(|audio| audio.entity),
                "inside_triggers": triggers,
                "nearest": nearest.into_iter().take(NEAREST_LISTED).map(|(_, entry)| entry).collect::<Vec<_>>(),
            })
        })
        .collect();
    let silent = answers.iter().filter(|answer| answer["soundscape"].is_null()).count();
    Ok(ToolResult::json(&json!({
        "soundscape_entities": map.soundscapes.len(),
        "points_without_soundscape": silent,
        "points": answers,
    })))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use glam::DVec3;
    use serde_json::json;

    use super::super::test_game::{call, error_of, json_of, TestGame};
    use super::super::run;
    use crate::editor::Editor;

    const SCRIPT: &str = r#"
"test.room"
{
	"dsp" "1"
	"playlooping"
	{
		"volume" "0.4"
		"pitch" "100"
		"wave" "ambient/hum.wav"
	}
	"playrandom"
	{
		"time" "5,10"
		"volume" "0.5,0.8"
		"position" "1"
		"rndwave"
		{
			"wave" "ambient/creak1.wav"
			"wave" "ambient/nowhere.wav"
		}
	}
}
"test.yard"
{
	"playsoundscape"
	{
		"name" "test.room"
		"volume" "0.5"
	}
}
"#;

    fn game(name: &str) -> TestGame {
        let game = TestGame::new(name);
        game.write(
            "scripts/soundscapes_manifest.txt",
            b"soundscapes_manifest\n{\n\t\"file\" \"scripts/soundscapes_test.txt\"\n}\n",
        );
        game.write("scripts/soundscapes_test.txt", SCRIPT.as_bytes());
        game.write("sound/ambient/hum.wav", b"RIFF");
        game.write("sound/ambient/creak1.wav", b"RIFF");
        game.material(
            "test/wall",
            "LightmappedGeneric\n{\n\t\"$basetexture\" \"test/wall\"\n}\n",
            None,
        );
        game
    }

    fn soundscape(editor: &mut Editor, origin: DVec3, name: &str, radius: &str) -> u32 {
        let keys = [
            ("soundscape".to_owned(), name.to_owned()),
            ("radius".to_owned(), radius.to_owned()),
        ];
        editor.create_entity("env_soundscape", origin, &keys).unwrap()
    }

    #[test]
    fn soundscapes_are_listed_without_the_preview() {
        let game = game("soundscapes-list");
        let mut editor = game.editor();
        let room = soundscape(&mut editor, DVec3::ZERO, "Test.Room", "512");
        let lost = soundscape(&mut editor, DVec3::X * 64.0, "test.gone", "512");
        let shared = Mutex::new(editor);
        let all = json_of(&call(&shared, "list_soundscapes", &json!({})));
        assert_eq!(all["script_files"], 1);
        assert_eq!(all["total"], 2);
        assert_eq!(all["files"][0]["file"], "scripts/soundscapes_test.txt");
        assert_eq!(all["files"][0]["soundscapes"], json!(["test.room", "test.yard"]));
        assert_eq!(
            all["in_map"],
            json!([
                { "soundscape": "test.gone", "entities": [lost], "defined": false },
                { "soundscape": "Test.Room", "entities": [room], "defined": true },
            ])
        );
        let used = json_of(&call(&shared, "list_soundscapes", &json!({ "used_only": true })));
        assert_eq!(used["files"][0]["soundscapes"], json!(["test.room"]));
        let filtered = json_of(&call(&shared, "list_soundscapes", &json!({ "filter": "test !room" })));
        assert_eq!(filtered["files"][0]["soundscapes"], json!(["test.yard"]));
        let capped = json_of(&call(&shared, "list_soundscapes", &json!({ "limit": 1 })));
        assert_eq!((capped["total"].as_u64(), capped["shown"].as_u64()), (Some(2), Some(1)));
    }

    #[test]
    fn a_soundscape_is_described_with_what_it_lacks() {
        let game = game("soundscapes-describe");
        let mut editor = game.editor();
        let room = soundscape(&mut editor, DVec3::ZERO, "test.room", "512");
        let described = json_of(&run(
            &mut editor,
            "describe_soundscape",
            &json!({ "name": "TEST.ROOM" }),
        ));
        assert_eq!(described["soundscape"], "test.room");
        assert_eq!(described["file"], "scripts/soundscapes_test.txt");
        assert_eq!(described["positions"], json!([1]));
        let plays = described["plays"].as_array().unwrap();
        assert_eq!(plays[0]["kind"], "dsp");
        assert_eq!(plays[1]["text"], "ambient/hum.wav");
        assert!(plays[1].get("missing").is_none());
        assert_eq!(plays.len(), 3, "a random set's files are listed with the set");
        assert_eq!(plays[2]["kind"], "random");
        assert_eq!(plays[2]["waves"], json!(["ambient/creak1.wav", "ambient/nowhere.wav"]));
        assert_eq!(plays[2]["missing"], json!(["ambient/nowhere.wav"]));
        assert_eq!(described["missing_sounds"], json!(["ambient/nowhere.wav"]));
        assert_eq!(
            described["entities"],
            json!([{ "entity": room, "missing_positions": [1] }])
        );
        assert!(described["script"].as_str().unwrap().contains("playlooping\n"));

        let nested = json_of(&run(
            &mut editor,
            "describe_soundscape",
            &json!({ "name": "test.yard" }),
        ));
        assert_eq!(nested["plays"][0]["kind"], "soundscape");
        assert_eq!(nested["plays"][1]["depth"], 1);

        let unknown = run(&mut editor, "describe_soundscape", &json!({ "name": "yard" }));
        assert!(error_of(&unknown).contains("did you mean one of: test.yard"));
    }

    #[test]
    fn the_soundscape_at_a_point_follows_range_and_sight() {
        let game = game("soundscapes-at");
        let mut editor = game.editor();
        editor.material = "test/wall".to_owned();
        editor
            .create_block(DVec3::new(240.0, -256.0, -256.0), DVec3::new(272.0, 256.0, 256.0))
            .unwrap();
        let near = soundscape(&mut editor, DVec3::ZERO, "test.room", "400");
        let far = soundscape(&mut editor, DVec3::new(1024.0, 0.0, 0.0), "test.yard", "-1");
        let points = json!({ "points": [[128, 0, 0], [512, 0, 0], [128, 4096, 0], [-512, 0, 0]] });
        let answer = json_of(&run(&mut editor, "soundscape_at", &points));
        assert_eq!(answer["soundscape_entities"], 2);
        let heard: Vec<(Option<&str>, Option<u64>)> = answer["points"]
            .as_array()
            .unwrap()
            .iter()
            .map(|point| (point["soundscape"].as_str(), point["entity"].as_u64()))
            .collect();
        assert_eq!(
            heard[0],
            (Some("test.room"), Some(u64::from(near))),
            "in range and in sight"
        );
        assert_eq!(
            heard[1],
            (Some("test.yard"), Some(u64::from(far))),
            "behind the wall the far one is in sight"
        );
        assert_eq!(
            heard[2],
            (Some("test.yard"), Some(u64::from(far))),
            "out of the near one's range"
        );
        assert_eq!(
            heard[3],
            (None, None),
            "out of range of one and out of sight of the other"
        );
        assert_eq!(answer["points_without_soundscape"], 1);
        let nearest = &answer["points"][3]["nearest"];
        assert_eq!(nearest[0]["entity"], near);
        assert_eq!(nearest[0]["in_range"], false);
        assert_eq!(nearest[1]["in_sight"], false);
        assert!(error_of(&run(&mut editor, "soundscape_at", &json!({ "points": [] }))).contains("1 to 64"));
    }
}
