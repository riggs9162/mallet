//! The maps the open map's `func_instance` entities place, read from disk and placed
//! where each instance puts them, so the views show and light them as the compiled map
//! will. They are only shown: the open map keeps just the instance entities, and saving
//! it writes nothing of what they place.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use mallet_vmf::instance::{self, Placement, MAX_DEPTH};
use mallet_vmf::{Entity, TopLevel, Vmf};

/// How often the files of placed instances are checked for changes.
const CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// When a file was last written and how long it was, to tell when it changes.
type Stamp = (SystemTime, u64);

/// One instance map read from disk.
struct LoadedFile {
    stamp: Option<Stamp>,
    contents: Result<Arc<Vmf>, String>,
}

/// The instance maps read so far, shared by every tab, and the files to watch.
#[derive(Default)]
pub struct InstanceLibrary {
    files: HashMap<PathBuf, LoadedFile>,
    /// Places an instance's file was looked for without being found, so it shows once
    /// it is saved there.
    missing: HashSet<PathBuf>,
    checked: Option<Instant>,
    /// Goes up whenever a map read before is forgotten, so scenes built before know to
    /// place their instances again.
    generation: u64,
}

/// One `func_instance` of the map with the contents it places.
pub struct PlacedInstance {
    /// The instance entity's id, which owns everything it places.
    pub entity: u32,
    /// Its map and each instance inside that map, every one already placed in the open
    /// map. They are kept apart because each map numbers its brush faces on its own.
    pub parts: Vec<Vmf>,
    /// The files it read, its own first.
    pub files: Vec<PathBuf>,
}

/// Something wrong with one of the map's instances, for Check for Problems.
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceProblem {
    /// The instance entity's id.
    pub entity: u32,
    pub detail: String,
}

/// Every instance of a map, placed, and what kept any of them from showing.
#[derive(Default)]
pub struct Placed {
    pub instances: Vec<PlacedInstance>,
    pub problems: Vec<InstanceProblem>,
}

impl Placed {
    /// Every entity the instances place, for loading their models and decal sizes.
    pub fn entities(&self) -> impl Iterator<Item = &Entity> {
        self.instances
            .iter()
            .flat_map(|placed| &placed.parts)
            .flat_map(Vmf::entities)
    }
}

/// What placing one instance gathers.
struct Gathered {
    parts: Vec<Vmf>,
    files: Vec<PathBuf>,
    problems: Vec<String>,
}

impl InstanceLibrary {
    /// Places every instance of `vmf`, the map saved at `map`, reading the maps they
    /// name. An instance whose map cannot be found or read keeps only its own marker and
    /// gets a problem.
    pub fn place_all(&mut self, vmf: &Vmf, map: Option<&Path>) -> Placed {
        let top = map.and_then(Path::parent).map(Path::to_path_buf);
        let mut placed = Placed::default();
        for entity in vmf.entities().filter(|entity| instance::is_instance(entity)) {
            let Some(id) = entity.id() else {
                continue;
            };
            let mut gathered = Gathered {
                parts: Vec::new(),
                files: Vec::new(),
                problems: Vec::new(),
            };
            let folders: Vec<PathBuf> = top.iter().cloned().collect();
            self.place_one(entity, &folders, top.as_deref(), &mut Vec::new(), &mut gathered);
            placed.problems.extend(
                gathered
                    .problems
                    .into_iter()
                    .map(|detail| InstanceProblem { entity: id, detail }),
            );
            if !gathered.parts.is_empty() {
                placed.instances.push(PlacedInstance {
                    entity: id,
                    parts: gathered.parts,
                    files: gathered.files,
                });
            }
        }
        placed
    }

    /// Places one instance, already in the open map's space, and the instances inside
    /// it. `folders` are where its `file` is looked for first; `chain` holds the files
    /// of the instances it sits in, so one that places itself is caught.
    fn place_one(
        &mut self,
        entity: &Entity,
        folders: &[PathBuf],
        top: Option<&Path>,
        chain: &mut Vec<PathBuf>,
        gathered: &mut Gathered,
    ) {
        let Some(file) = instance::instance_file(entity) else {
            if chain.is_empty() {
                gathered.problems.push("names no map in its File key".to_owned());
            }
            return;
        };
        if chain.len() >= MAX_DEPTH {
            gathered.problems.push(format!(
                "places {file} more than {MAX_DEPTH} instances deep, so it is left out"
            ));
            return;
        }
        let Some(path) = self.find(file, folders, top) else {
            let place = match top {
                Some(folder) => format!("beside {}", folder.display()),
                None => "because the map has not been saved".to_owned(),
            };
            gathered.problems.push(format!("could not find {file} {place}"));
            return;
        };
        if chain.contains(&path) {
            gathered.problems.push(format!(
                "{} places itself, so it is left out the second time",
                path.display()
            ));
            return;
        }
        let contents = match self.read(&path) {
            Ok(contents) => contents,
            Err(error) => {
                gathered
                    .problems
                    .push(format!("could not read {}: {error}", path.display()));
                return;
            }
        };
        let mut part = instance::place(&contents, &Placement::of(entity));
        let nested: Vec<Entity> = part
            .entities()
            .filter(|entity| instance::is_instance(entity))
            .cloned()
            .collect();
        part.items
            .retain(|item| !matches!(item, TopLevel::Entity(entity) if instance::is_instance(entity)));
        gathered.files.push(path.clone());
        gathered.parts.push(part);
        if nested.is_empty() {
            return;
        }
        let mut inner_folders: Vec<PathBuf> = path.parent().map(Path::to_path_buf).into_iter().collect();
        inner_folders.extend(top.map(Path::to_path_buf));
        chain.push(path);
        for entity in &nested {
            self.place_one(entity, &inner_folders, top, chain, gathered);
        }
        chain.pop();
    }

    /// The first place `file` is found, looking in `folders` and then around the map's
    /// own folder. Places looked in are remembered, so the file shows once it is saved.
    fn find(&mut self, file: &str, folders: &[PathBuf], top: Option<&Path>) -> Option<PathBuf> {
        let mut roots: Vec<&Path> = folders.iter().map(PathBuf::as_path).collect();
        if let Some(top) = top.filter(|top| !roots.contains(top)) {
            roots.push(top);
        }
        let candidates = instance::candidate_paths(file, &roots);
        if let Some(found) = candidates
            .iter()
            .find(|path| self.files.contains_key(*path) || path.is_file())
        {
            return Some(found.clone());
        }
        self.missing.extend(candidates);
        None
    }

    /// The map at `path`, read again only when it is not read yet or has changed.
    fn read(&mut self, path: &Path) -> Result<Arc<Vmf>, String> {
        if let Some(loaded) = self.files.get(path) {
            return loaded.contents.clone();
        }
        let stamp = stamp(path);
        let contents = std::fs::read(path)
            .map_err(|error| error.to_string())
            .and_then(|bytes| Vmf::parse(&String::from_utf8_lossy(&bytes)).map_err(|error| error.to_string()))
            .map(Arc::new);
        self.files.insert(
            path.to_path_buf(),
            LoadedFile {
                stamp,
                contents: contents.clone(),
            },
        );
        contents
    }

    /// Whether an instance map read so far has changed on disk or a missing one has
    /// appeared since the last look, checking at most once a [`CHECK_INTERVAL`]. Changed
    /// maps are forgotten, so placing the instances again reads them anew.
    pub fn changed_on_disk(&mut self) -> bool {
        let now = Instant::now();
        if self
            .checked
            .is_some_and(|checked| now.duration_since(checked) < CHECK_INTERVAL)
        {
            return false;
        }
        self.checked = Some(now);
        let before = self.files.len();
        self.files.retain(|path, loaded| stamp(path) == loaded.stamp);
        let appeared = self.missing.iter().any(|path| path.is_file());
        if appeared {
            self.missing.clear();
        }
        let changed = appeared || self.files.len() != before;
        if changed {
            self.generation += 1;
        }
        changed
    }

    /// Forgets every map read, so they are all read again.
    pub fn forget(&mut self) {
        self.files.clear();
        self.missing.clear();
        self.generation += 1;
    }

    /// Changes whenever a map read before is forgotten.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether any map has instances to watch the files of.
    pub fn watching(&self) -> bool {
        !self.files.is_empty() || !self.missing.is_empty()
    }
}

fn stamp(path: &Path) -> Option<Stamp> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

#[cfg(test)]
mod tests {
    use glam::DVec3;

    use super::*;

    const INNER: &str = r#"world
{
	"id" "1"
	"classname" "worldspawn"
	solid
	{
		"id" "2"
		side
		{
			"id" "1"
			"plane" "(0 0 0) (16 0 0) (16 -16 0)"
			"material" "DEV/DEV_MEASUREGENERIC01B"
			"uaxis" "[1 0 0 0] 0.25"
			"vaxis" "[0 -1 0 0] 0.25"
			"rotation" "0"
			"lightmapscale" "16"
			"smoothing_groups" "0"
		}
	}
}
entity
{
	"id" "3"
	"classname" "light"
	"origin" "0 0 64"
}
"#;

    fn outer(file: &str) -> String {
        format!(
            "world\n{{\n\t\"id\" \"1\"\n\t\"classname\" \"worldspawn\"\n}}\nentity\n{{\n\t\"id\" \"9\"\n\t\"classname\" \"func_instance\"\n\t\"file\" \"{file}\"\n\t\"origin\" \"512 0 0\"\n}}\n"
        )
    }

    fn folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("mallet-instances-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(folder.join("instances/kit")).unwrap();
        folder
    }

    #[test]
    fn places_nested_instances_relative_to_their_own_file() {
        let root = folder("nested");
        std::fs::write(root.join("instances/kit/inner.vmf"), INNER).unwrap();
        std::fs::write(root.join("instances/kit/outer.vmf"), outer("inner.vmf")).unwrap();
        let map = Vmf::parse(&outer("instances/kit/outer.vmf")).unwrap();
        let mut library = InstanceLibrary::default();
        let placed = library.place_all(&map, Some(&root.join("map.vmf")));
        assert!(placed.problems.is_empty(), "{:?}", placed.problems);
        assert_eq!(placed.instances.len(), 1);
        let instance = &placed.instances[0];
        assert_eq!(instance.entity, 9);
        assert_eq!(instance.parts.len(), 2);
        let light = instance.parts[1].entities().next().unwrap();
        assert_eq!(light.origin(), Some(DVec3::new(1024.0, 0.0, 64.0)));
        assert!(instance.parts[0]
            .entities()
            .all(|entity| !instance::is_instance(entity)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_editor_shows_lights_and_picks_instances_without_saving_them() {
        use crate::editor::Editor;
        use crate::problems::ProblemKind;
        use crate::scene::Owner;

        let root = folder("editor");
        let mut kit = Editor::default();
        kit.new_map();
        kit.create_block(DVec3::new(-64.0, -64.0, -16.0), DVec3::new(64.0, 64.0, 0.0))
            .unwrap();
        kit.create_entity("light", DVec3::new(0.0, 0.0, 64.0), &[]).unwrap();
        kit.save(Some(root.join("instances/kit/tile.vmf"))).unwrap();

        let mut editor = Editor::default();
        editor.new_map();
        let file = ("file".to_owned(), "instances/kit/tile.vmf".to_owned());
        let id = editor
            .create_entity("func_instance", DVec3::new(512.0, 0.0, 0.0), &[file])
            .unwrap();
        assert!(editor.scene.meshes.iter().all(|mesh| !mesh.instanced));
        let map = root.join("map.vmf");
        editor.save(Some(map.clone())).unwrap();

        let instanced: Vec<_> = editor.scene.meshes.iter().filter(|mesh| mesh.instanced).collect();
        assert!(!instanced.is_empty());
        assert!(instanced
            .iter()
            .all(|mesh| mesh.owner == Some(Owner::Entity(id)) && mesh.solid.is_none()));
        let light = |editor: &Editor| {
            editor
                .scene
                .lighting
                .lights
                .iter()
                .find(|light| light.entity == Some(id))
                .map(|light| light.origin)
        };
        assert_eq!(light(&editor), Some(DVec3::new(512.0, 0.0, 64.0)));
        let down = editor
            .scene
            .surface_where(DVec3::new(500.0, 10.0, 256.0), DVec3::NEG_Z, |_| true);
        let hit = down.expect("a line traced down meets the instance's floor");
        assert!(hit.mesh.instanced && hit.face.side.is_none());
        assert!((hit.point.z - 0.0).abs() < 1e-6);
        assert!(editor
            .scene
            .surface_face(DVec3::new(500.0, 10.0, 256.0), DVec3::NEG_Z)
            .is_none());
        let picked = editor
            .scene
            .pick_ray(DVec3::new(500.0, 10.0, 256.0), DVec3::NEG_Z, &editor.models, false);
        assert_eq!(picked, Some(Owner::Entity(id)));

        let saved = std::fs::read_to_string(&map).unwrap();
        assert!(saved.contains("func_instance"));
        assert!(
            !saved.contains("\"light\""),
            "the instance's contents are not saved into the map"
        );

        editor.set_key_on(&[id], "origin", "1024 0 0").unwrap();
        assert_eq!(light(&editor), Some(DVec3::new(1024.0, 0.0, 64.0)));

        kit.create_entity("light", DVec3::new(32.0, 0.0, 64.0), &[]).unwrap();
        kit.save(None).unwrap();
        editor.instances.checked = None;
        assert!(editor.take_instance_changes(), "a saved instance map is read again");
        assert_eq!(editor.scene.lighting.lights.iter().filter(|light| light.entity == Some(id)).count(), 2);

        editor.set_key_on(&[id], "file", "instances/kit/gone.vmf").unwrap();
        assert!(editor.scene.meshes.iter().all(|mesh| !mesh.instanced));
        assert!(light(&editor).is_none());
        let problems = editor.check_for_problems();
        assert!(problems
            .iter()
            .any(|problem| problem.kind == ProblemKind::MissingInstance && problem.object == Some(Owner::Entity(id))));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reports_missing_files_and_cycles() {
        let root = folder("missing");
        std::fs::write(root.join("instances/kit/loop.vmf"), outer("loop.vmf")).unwrap();
        let mut library = InstanceLibrary::default();
        let missing = Vmf::parse(&outer("instances/kit/nowhere.vmf")).unwrap();
        let placed = library.place_all(&missing, Some(&root.join("map.vmf")));
        assert!(placed.instances.is_empty());
        assert_eq!(placed.problems.len(), 1);
        assert!(placed.problems[0].detail.contains("could not find"));
        let looping = Vmf::parse(&outer("instances/kit/loop.vmf")).unwrap();
        let placed = library.place_all(&looping, Some(&root.join("map.vmf")));
        assert_eq!(placed.instances.len(), 1);
        assert!(placed
            .problems
            .iter()
            .any(|problem| problem.detail.contains("places itself")));
        std::fs::write(root.join("instances/kit/nowhere.vmf"), INNER).unwrap();
        library.checked = None;
        assert!(library.changed_on_disk(), "a missing file that appears is noticed");
        let _ = std::fs::remove_dir_all(&root);
    }
}
