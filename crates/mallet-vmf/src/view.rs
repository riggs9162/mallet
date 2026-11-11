//! Editor state Hammer keeps in a map: the 3D cameras (`cameras`) and the grid
//! (`viewsettings`). Saving them lets a map reopen where it was left, in Mallet and in
//! Hammer.

use glam::DVec3;
use mallet_kv::{Block, Item, Pair};

use crate::{TopLevel, Vmf};

/// A saved 3D camera: where it is and a point it looks at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SavedCamera {
    pub position: DVec3,
    pub look: DVec3,
}

impl Vmf {
    /// The camera `activecamera` points at, if it is valid.
    pub fn active_camera(&self) -> Option<SavedCamera> {
        let cameras = self.block("cameras")?;
        let active: usize = cameras.get("activecamera")?.trim().parse().ok()?;
        let camera = cameras.blocks_named("camera").nth(active)?;
        Some(SavedCamera {
            position: parse_bracketed(camera.get("position")?)?,
            look: parse_bracketed(camera.get("look")?)?,
        })
    }

    /// Stores the camera as the active one, replacing the active camera if there is one
    /// and keeping any others, as Hammer does.
    pub fn set_active_camera(&mut self, camera: SavedCamera) {
        let cameras = self.block_mut_or_insert("cameras", &["cordons"], true);
        let count = cameras.blocks_named("camera").count();
        let active = cameras
            .get("activecamera")
            .and_then(|value| value.trim().parse::<usize>().ok())
            .filter(|active| *active < count);
        let index = match active {
            Some(index) => index,
            None => {
                cameras.push_block(Block::new("camera"));
                count
            }
        };
        set_pair(cameras, "activecamera", index.to_string());
        let block = cameras
            .items
            .iter_mut()
            .filter_map(|item| match item {
                Item::Block(block) if block.name.eq_ignore_ascii_case("camera") => Some(block),
                _ => None,
            })
            .nth(index)
            .expect("the camera was found or added");
        set_pair(block, "position", format_bracketed(camera.position));
        set_pair(block, "look", format_bracketed(camera.look));
    }

    /// The grid size Hammer saved, from `viewsettings`.
    pub fn grid_spacing(&self) -> Option<f64> {
        self.block("viewsettings")?
            .get("nGridSpacing")?
            .trim()
            .parse()
            .ok()
            .filter(|size: &f64| *size > 0.0)
    }

    /// Stores the grid size in `viewsettings`, adding the block with Hammer's defaults
    /// when the map has none.
    pub fn set_grid_spacing(&mut self, size: f64) {
        set_pair(self.view_settings_mut(), "nGridSpacing", format_number(size));
    }

    /// A yes-or-no setting Hammer saved in `viewsettings`, such as `bSnapToGrid` or
    /// `bShowGrid`. `None` when the map has no such setting.
    pub fn view_flag(&self, key: &str) -> Option<bool> {
        match self.block("viewsettings")?.get(key)?.trim() {
            "0" => Some(false),
            "1" => Some(true),
            _ => None,
        }
    }

    /// Stores a yes-or-no setting in `viewsettings`, adding the block with Hammer's
    /// defaults when the map has none.
    pub fn set_view_flag(&mut self, key: &str, on: bool) {
        set_pair(self.view_settings_mut(), key, if on { "1" } else { "0" }.to_owned());
    }

    /// The `viewsettings` block, added with Hammer's defaults when the map has none.
    fn view_settings_mut(&mut self) -> &mut Block {
        let settings = self.block_mut_or_insert("viewsettings", &["world"], false);
        if settings.items.is_empty() {
            for (key, value) in [
                ("bSnapToGrid", "1"),
                ("bShowGrid", "1"),
                ("bShowLogicalGrid", "0"),
                ("nGridSpacing", "64"),
                ("bShow3DGrid", "0"),
            ] {
                settings.push_pair(key, value);
            }
        }
        settings
    }

    /// The first top-level block with this name, added when missing: before the first of
    /// `before` that exists, or at the end (`at_end`) or start otherwise.
    fn block_mut_or_insert(&mut self, name: &str, before: &[&str], at_end: bool) -> &mut Block {
        let position = self
            .items
            .iter()
            .position(|item| matches!(item, TopLevel::Other(block) if block.name.eq_ignore_ascii_case(name)));
        let index = match position {
            Some(index) => index,
            None => {
                let anchor = self.items.iter().position(|item| {
                    let item_name = match item {
                        TopLevel::World(_) => "world",
                        TopLevel::Other(block) => block.name.as_str(),
                        _ => "",
                    };
                    before.iter().any(|wanted| wanted.eq_ignore_ascii_case(item_name))
                });
                let index = anchor.unwrap_or(if at_end { self.items.len() } else { 0 });
                self.items.insert(index, TopLevel::Other(Block::new(name)));
                index
            }
        };
        match &mut self.items[index] {
            TopLevel::Other(block) => block,
            _ => unreachable!("the index points at the block"),
        }
    }
}

fn set_pair(block: &mut Block, key: &str, value: String) {
    let existing = block.items.iter_mut().find_map(|item| match item {
        Item::Pair(pair) if pair.key.eq_ignore_ascii_case(key) => Some(pair),
        _ => None,
    });
    match existing {
        Some(pair) => pair.value = value,
        None => block.items.push(Item::Pair(Pair::new(key, value))),
    }
}

/// Parses Hammer's `[x y z]`.
fn parse_bracketed(text: &str) -> Option<DVec3> {
    let numbers: Vec<f64> = text
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    match numbers.as_slice() {
        [x, y, z] => Some(DVec3::new(*x, *y, *z)),
        _ => None,
    }
}

/// A number with at most three decimals, like Hammer writes camera positions.
fn format_number(value: f64) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

fn format_bracketed(point: DVec3) -> String {
    format!(
        "[{} {} {}]",
        format_number(point.x),
        format_number(point.y),
        format_number(point.z)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_maps_gain_a_camera_and_keep_it() {
        let mut vmf = Vmf::new_map();
        assert_eq!(vmf.active_camera(), None);
        let camera = SavedCamera {
            position: DVec3::new(-339.39, -277.1634, 162.5),
            look: DVec3::new(-338.69, -276.539, 162.288),
        };
        vmf.set_active_camera(camera);
        vmf.set_grid_spacing(16.0);
        let text = vmf.to_text();
        assert!(text.contains("\"activecamera\" \"0\""), "{text}");
        assert!(text.contains("\"position\" \"[-339.39 -277.163 162.5]\""), "{text}");
        assert!(text.contains("\"nGridSpacing\" \"16\""), "{text}");
        assert_eq!(vmf.view_flag("bSnapToGrid"), Some(true));
        vmf.set_view_flag("bSnapToGrid", false);
        assert_eq!(vmf.view_flag("bSnapToGrid"), Some(false));
        assert_eq!(vmf.view_flag("bNoSuchSetting"), None);
        let text = vmf.to_text();
        let reloaded = Vmf::parse(&text).unwrap();
        let restored = reloaded.active_camera().unwrap();
        assert!((restored.position - camera.position).length() < 0.001);
        assert_eq!(reloaded.grid_spacing(), Some(16.0));

        // Saving again replaces the active camera instead of adding another.
        let mut again = reloaded;
        again.set_active_camera(SavedCamera {
            position: DVec3::ZERO,
            look: DVec3::X,
        });
        assert_eq!(again.block("cameras").unwrap().blocks_named("camera").count(), 1);
        assert_eq!(again.active_camera().unwrap().look, DVec3::X);
    }

    #[test]
    fn other_cameras_are_kept() {
        let text = r#"
world { "id" "1" "classname" "worldspawn" }
cameras
{
	"activecamera" "1"
	camera { "position" "[1 2 3]" "look" "[4 5 6]" }
	camera { "position" "[7 8 9]" "look" "[10 11 12]" }
}
"#;
        let mut vmf = Vmf::parse(text).unwrap();
        assert_eq!(vmf.active_camera().unwrap().position, DVec3::new(7.0, 8.0, 9.0));
        vmf.set_active_camera(SavedCamera {
            position: DVec3::splat(100.0),
            look: DVec3::splat(101.0),
        });
        let cameras: Vec<_> = vmf.block("cameras").unwrap().blocks_named("camera").collect();
        assert_eq!(cameras.len(), 2);
        assert_eq!(cameras[0].get("position"), Some("[1 2 3]"));
        assert_eq!(cameras[1].get("position"), Some("[100 100 100]"));
    }
}
