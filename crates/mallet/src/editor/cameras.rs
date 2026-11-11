//! Hammer's Camera tool cameras: several saved viewpoints of a map, kept in its `cameras`
//! block as Hammer keeps them, one of which the 3D view shows.

use glam::DVec3;
use mallet_vmf::SavedCamera;

use super::Editor;

/// The shortest line a camera's aim is drawn with in the 2D views.
const MIN_LOOK_DISTANCE: f64 = 64.0;

impl Editor {
    /// The map's cameras, the active one taken from the 3D view as it is now, and the
    /// active one's index. A map without cameras has just the 3D view's.
    pub fn camera_list(&self) -> (Vec<SavedCamera>, usize) {
        let (mut list, active) = self
            .document
            .as_ref()
            .map(|document| document.vmf.cameras())
            .unwrap_or_default();
        let distance = active
            .and_then(|index| list.get(index))
            .map_or(MIN_LOOK_DISTANCE, |saved| saved.position.distance(saved.look).max(MIN_LOOK_DISTANCE));
        let live = SavedCamera {
            position: self.camera.position,
            look: self.camera.position + self.camera.forward() * distance,
        };
        match active {
            Some(index) => {
                list[index] = live;
                (list, index)
            }
            None => {
                list.push(live);
                let index = list.len() - 1;
                (list, index)
            }
        }
    }

    /// Hammer's Shift+drag with the Camera tool: adds a camera at `position` looking at
    /// `look` and shows it in the 3D view.
    pub fn add_camera(&mut self, position: DVec3, look: DVec3) {
        let (mut list, _) = self.camera_list();
        list.push(SavedCamera { position, look });
        let index = list.len() - 1;
        self.store_cameras(&list, index);
        self.camera.set_view(position, look);
        self.message = format!("Camera {} of {}", index + 1, list.len());
    }

    /// Moves the camera the 3D view shows, as dragging it or its aim in a 2D view does.
    pub fn place_active_camera(&mut self, position: DVec3, look: DVec3) {
        let (mut list, active) = self.camera_list();
        list[active] = SavedCamera { position, look };
        self.store_cameras(&list, active);
        self.camera.set_view(position, look);
    }

    /// Shows camera `index` in the 3D view.
    pub fn activate_camera(&mut self, index: usize) {
        let (list, _) = self.camera_list();
        let Some(chosen) = list.get(index).copied() else { return };
        self.store_cameras(&list, index);
        self.camera.set_view(chosen.position, chosen.look);
        self.message = format!("Camera {} of {}", index + 1, list.len());
    }

    /// Hammer's Page Up and Page Down with the Camera tool: the previous or next camera.
    pub fn step_camera(&mut self, forward: bool) {
        let (list, active) = self.camera_list();
        let count = list.len();
        let next = if forward { (active + 1) % count } else { (active + count - 1) % count };
        self.activate_camera(next);
    }

    /// Hammer's Delete with the Camera tool: removes the camera the 3D view shows and
    /// shows the one before it. The last camera stays.
    pub fn delete_camera(&mut self) -> Result<(), String> {
        let (mut list, active) = self.camera_list();
        if list.len() < 2 {
            return Err("the last camera cannot be deleted".to_owned());
        }
        list.remove(active);
        let next = active.saturating_sub(1).min(list.len() - 1);
        let shown = list[next];
        self.store_cameras(&list, next);
        self.camera.set_view(shown.position, shown.look);
        self.message = format!("Deleted the camera; now on camera {} of {}", next + 1, list.len());
        Ok(())
    }

    /// Writes the cameras into the map. Like the saved 3D view, this is not an edit: it
    /// does not dirty the map or go into the undo history.
    fn store_cameras(&mut self, list: &[SavedCamera], active: usize) {
        if let Some(document) = self.document.as_mut() {
            document.vmf.set_cameras(list, active);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cameras_are_added_switched_and_deleted() {
        let mut editor = Editor::default();
        editor.new_map();
        editor.add_camera(DVec3::new(0.0, 0.0, 64.0), DVec3::new(128.0, 0.0, 64.0));
        assert_eq!(editor.camera_list().0.len(), 2);
        assert_eq!(editor.camera.position, DVec3::new(0.0, 0.0, 64.0));
        editor.step_camera(true);
        assert_eq!(editor.camera_list().1, 0);
        editor.step_camera(false);
        assert_eq!(editor.camera_list().1, 1);
        editor.delete_camera().unwrap();
        assert_eq!(editor.camera_list().0.len(), 1);
        assert!(editor.delete_camera().is_err());
    }
}
