//! Hammer's VisGroups: the groups listed in the side panel, showing and hiding them, and
//! moving objects in and out of them. Every change is one undo step.

use std::collections::{BTreeSet, HashMap};

use eframe::egui::ecolor::Hsva;
use mallet_vmf::edit::Edit;
use mallet_vmf::visgroup::{self, AutoGroup, Change, Object, VisGroup};

use super::{Editor, SelectMode};
use crate::scene::Owner;

/// How much of a group shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GroupState {
    /// Every member shows, or the group is empty.
    #[default]
    Shown,
    /// Some members show and some are hidden.
    Partial,
    /// Every member is hidden.
    Hidden,
}

impl GroupState {
    fn of(members: usize, hidden: usize) -> Self {
        match hidden {
            0 => Self::Shown,
            hidden if hidden == members => Self::Hidden,
            _ => Self::Partial,
        }
    }
}

/// One line of the User list of the VisGroups panel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserGroupRow {
    /// The group's `visgroupid`.
    pub id: u32,
    pub name: String,
    pub color: [u8; 3],
    /// How deep the group is nested, 0 for a top-level group.
    pub depth: usize,
    /// How many objects are in the group or in a group nested inside it.
    pub members: usize,
    pub state: GroupState,
}

/// One line of the Auto list of the VisGroups panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoGroupRow {
    pub group: AutoGroup,
    /// 1 for a group listed under another one, else 0.
    pub depth: usize,
    pub members: usize,
    pub state: GroupState,
}

/// What the VisGroups panel lists, worked out again after every change to the map.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VisGroupList {
    pub user: Vec<UserGroupRow>,
    pub auto: Vec<AutoGroupRow>,
    /// How many objects QuickHide hides.
    pub quick_hidden: usize,
}

/// Where the selection goes with Move Selection to VisGroup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MoveOptions {
    /// Take the objects out of every other VisGroup first.
    pub leave_others: bool,
    /// Hide the group once the objects are in it.
    pub hide: bool,
}

impl Default for MoveOptions {
    fn default() -> Self {
        Self {
            leave_others: true,
            hide: false,
        }
    }
}

impl Editor {
    /// Works out the VisGroups panel's lists from the map.
    pub(super) fn refresh_visgroups(&mut self) {
        let Some(document) = &self.document else {
            self.visgroups = VisGroupList::default();
            return;
        };
        let objects = visgroup::objects(&document.vmf);
        let mut user = Vec::new();
        user_rows(&visgroup::groups(&document.vmf), 0, &objects, &mut user);
        let auto = AutoGroup::ALL
            .into_iter()
            .map(|group| {
                let members = objects.iter().filter(|object| object.is_in(group));
                let (count, hidden) = members.fold((0, 0), |(count, hidden), object| {
                    (count + 1, hidden + usize::from(!object.auto_shown))
                });
                AutoGroupRow {
                    group,
                    depth: usize::from(group.parent().is_some()),
                    members: count,
                    state: GroupState::of(count, hidden),
                }
            })
            .collect();
        self.visgroups = VisGroupList {
            user,
            auto,
            quick_hidden: objects.iter().filter(|object| object.quick_hidden()).count(),
        };
    }

    /// Shows or hides a VisGroup and the groups nested inside it. Returns how many
    /// objects changed.
    pub fn set_visgroup_shown(&mut self, id: u32, shown: bool) -> Result<usize, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let groups = visgroup::groups(vmf);
        let group = visgroup::find(&groups, id).ok_or_else(|| format!("no VisGroup has id {id}"))?;
        let ids = group.ids();
        let name = group.name.clone();
        let changes: Vec<Change> = visgroup::objects(vmf)
            .iter()
            .filter(|object| object.shown != shown && object.groups.iter().any(|group| ids.contains(group)))
            .map(|object| Change {
                shown: Some(shown),
                ..Change::of(object)
            })
            .collect();
        let count = self.apply_object_changes(Vec::new(), &changes)?;
        let verb = if shown { "Showed" } else { "Hid" };
        self.message = format!("{verb} VisGroup \"{name}\" ({count} objects)");
        Ok(count)
    }

    /// Shows or hides one of the automatic groups. Returns how many objects changed.
    pub fn set_auto_visgroup_shown(&mut self, group: AutoGroup, shown: bool) -> Result<usize, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let changes: Vec<Change> = visgroup::objects(vmf)
            .iter()
            .filter(|object| object.auto_shown != shown && object.is_in(group))
            .map(|object| Change {
                auto_shown: Some(shown),
                ..Change::of(object)
            })
            .collect();
        let count = self.apply_object_changes(Vec::new(), &changes)?;
        let verb = if shown { "Showed" } else { "Hid" };
        self.message = format!("{verb} {} ({count} objects)", group.label());
        Ok(count)
    }

    /// Shows every VisGroup again, the automatic ones too. Objects QuickHide hides stay
    /// hidden. Returns how many objects were shown.
    pub fn show_all_visgroups(&mut self) -> Result<usize, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let changes: Vec<Change> = visgroup::objects(vmf)
            .iter()
            .filter(|object| !object.shown || !object.auto_shown)
            .map(|object| Change {
                shown: Some(true),
                auto_shown: Some(true),
                ..Change::of(object)
            })
            .collect();
        let count = self.apply_object_changes(Vec::new(), &changes)?;
        self.message = if count == 0 {
            "Every VisGroup shows already".to_owned()
        } else {
            format!("Showed every VisGroup ({count} objects)")
        };
        Ok(count)
    }

    /// Adds an empty VisGroup and returns its id. A blank name becomes "VisGroup" and its
    /// number, and no colour picks one that differs from group to group.
    pub fn create_visgroup(&mut self, name: &str, color: Option<[u8; 3]>) -> Result<u32, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let mut groups = visgroup::groups(vmf);
        let group = new_group(&groups, name, color);
        let id = group.id;
        self.message = format!("Created VisGroup \"{}\"", group.name);
        groups.push(group);
        self.apply_edit(&visgroup::set_groups(&groups))?;
        Ok(id)
    }

    /// Hammer's Move Selection to VisGroup: puts the selected objects in a VisGroup, with
    /// the group's colour. Returns how many objects there are.
    pub fn selection_to_visgroup(&mut self, id: u32, options: MoveOptions) -> Result<usize, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let groups = visgroup::groups(vmf);
        let group = visgroup::find(&groups, id).ok_or_else(|| format!("no VisGroup has id {id}"))?;
        let name = group.name.clone();
        let changes = self.selection_changes(&visgroup::objects(vmf), group, options)?;
        let count = changes.len();
        self.apply_object_changes(Vec::new(), &changes)?;
        self.message = format!("Moved {count} objects to VisGroup \"{name}\"");
        Ok(count)
    }

    /// Makes a new VisGroup of the selected objects, as one undo step. Returns its id.
    pub fn selection_to_new_visgroup(&mut self, name: &str, options: MoveOptions) -> Result<u32, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let mut groups = visgroup::groups(vmf);
        let group = new_group(&groups, name, None);
        let (id, name) = (group.id, group.name.clone());
        let changes = self.selection_changes(&visgroup::objects(vmf), &group, options)?;
        let count = changes.len();
        groups.push(group);
        self.apply_object_changes(vec![visgroup::set_groups(&groups)], &changes)?;
        self.message = format!("Moved {count} objects to the new VisGroup \"{name}\"");
        Ok(id)
    }

    /// Puts the selected objects in a VisGroup or takes them out of it, keeping their
    /// other groups, as the VisGroup tab of Object Properties does.
    pub fn set_selection_in_visgroup(&mut self, id: u32, member: bool) -> Result<usize, String> {
        if member {
            let options = MoveOptions {
                leave_others: false,
                hide: false,
            };
            return self.selection_to_visgroup(id, options);
        }
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let changes: Vec<Change> = visgroup::objects(vmf)
            .iter()
            .filter(|object| self.selection.contains(&owner_of(object)) && object.groups.contains(&id))
            .map(|object| {
                let groups: Vec<u32> = object.groups.iter().copied().filter(|group| *group != id).collect();
                Change {
                    shown: groups.is_empty().then_some(true),
                    groups: Some(groups),
                    ..Change::of(object)
                }
            })
            .collect();
        let count = self.apply_object_changes(Vec::new(), &changes)?;
        self.message = format!("Took {count} objects out of the VisGroup");
        Ok(count)
    }

    /// Renames and recolours a VisGroup. Its members take the new colour.
    pub fn edit_visgroup(&mut self, id: u32, name: &str, color: [u8; 3]) -> Result<(), String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let name = name.trim();
        if name.is_empty() {
            return Err("a VisGroup needs a name".to_owned());
        }
        let mut groups = visgroup::groups(vmf);
        let group = find_mut(&mut groups, id).ok_or_else(|| format!("no VisGroup has id {id}"))?;
        if group.name == name && group.color == color {
            return Ok(());
        }
        let recolored = group.color != color;
        group.name = name.to_owned();
        group.color = color;
        let changes: Vec<Change> = visgroup::objects(vmf)
            .iter()
            .filter(|object| recolored && object.groups.first() == Some(&id))
            .map(|object| Change {
                color: Some(color),
                ..Change::of(object)
            })
            .collect();
        self.apply_object_changes(vec![visgroup::set_groups(&groups)], &changes)?;
        self.message = format!("Changed VisGroup \"{name}\"");
        Ok(())
    }

    /// Removes a VisGroup and the groups nested inside it. Their members stay in the map
    /// and show again when no other group holds them.
    pub fn delete_visgroup(&mut self, id: u32) -> Result<(), String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let mut groups = visgroup::groups(vmf);
        let group = visgroup::find(&groups, id).ok_or_else(|| format!("no VisGroup has id {id}"))?;
        let removed = group.ids();
        let name = group.name.clone();
        let changes = removal_changes(&visgroup::objects(vmf), &removed);
        remove_groups(&mut groups, &removed);
        self.apply_object_changes(vec![visgroup::set_groups(&groups)], &changes)?;
        self.message = format!("Removed VisGroup \"{name}\"");
        Ok(())
    }

    /// Removes every VisGroup that has no members, itself or nested. Returns how many.
    pub fn purge_empty_visgroups(&mut self) -> Result<usize, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let used: BTreeSet<u32> = visgroup::objects(vmf)
            .iter()
            .flat_map(|object| object.groups.iter().copied())
            .collect();
        let mut groups = visgroup::groups(vmf);
        let before: usize = groups.iter().map(|group| group.ids().len()).sum();
        purge(&mut groups, &used);
        let count = before - groups.iter().map(|group| group.ids().len()).sum::<usize>();
        if count > 0 {
            self.apply_edit(&visgroup::set_groups(&groups))?;
        }
        self.message = match count {
            0 => "No VisGroup is empty".to_owned(),
            1 => "Removed 1 empty VisGroup".to_owned(),
            count => format!("Removed {count} empty VisGroups"),
        };
        Ok(count)
    }

    /// Hammer's Mark: selects the members of a VisGroup that show.
    pub fn select_visgroup(&mut self, id: u32) -> Result<usize, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let groups = visgroup::groups(vmf);
        let ids = visgroup::find(&groups, id)
            .ok_or_else(|| format!("no VisGroup has id {id}"))?
            .ids();
        let members = |object: &Object| object.groups.iter().any(|group| ids.contains(group));
        Ok(self.select_objects(members))
    }

    /// Selects the members of an automatic group that show.
    pub fn select_auto_visgroup(&mut self, group: AutoGroup) -> Result<usize, String> {
        if self.document.is_none() {
            return Err("no map is open".to_owned());
        }
        Ok(self.select_objects(|object| object.is_in(group)))
    }

    /// Hammer's Convert QuickHide Objects to VisGroup: the objects QuickHide hides go in
    /// a new VisGroup, which stays hidden. Returns the group's id.
    pub fn quick_hidden_to_visgroup(&mut self) -> Result<u32, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        let mut groups = visgroup::groups(vmf);
        let group = new_group(&groups, "QuickHide", None);
        let (id, name) = (group.id, group.name.clone());
        let changes: Vec<Change> = visgroup::objects(vmf)
            .iter()
            .filter(|object| object.quick_hidden())
            .map(|object| Change {
                groups: Some(with_group(&object.groups, id)),
                shown: Some(false),
                color: Some(group.color),
                ..Change::of(object)
            })
            .collect();
        if changes.is_empty() {
            return Err("QuickHide hides no objects".to_owned());
        }
        let count = changes.len();
        groups.push(group);
        self.apply_object_changes(vec![visgroup::set_groups(&groups)], &changes)?;
        self.message = format!("Moved {count} hidden objects to the new VisGroup \"{name}\"");
        Ok(id)
    }

    /// How many of the selected objects are in each VisGroup, and how many are selected.
    pub fn selection_visgroups(&self) -> (HashMap<u32, usize>, usize) {
        let mut counts = HashMap::new();
        let Some(document) = &self.document else {
            return (counts, 0);
        };
        let mut selected = 0;
        for object in visgroup::objects(&document.vmf) {
            if self.selection.contains(&owner_of(&object)) {
                selected += 1;
                for group in object.groups {
                    *counts.entry(group).or_insert(0) += 1;
                }
            }
        }
        (counts, selected)
    }

    /// The ids of the brushes and entities QuickHide hides, brushes first.
    pub(super) fn quick_hidden_ids(&self) -> (Vec<u32>, Vec<u32>) {
        let mut solids = Vec::new();
        let mut entities = Vec::new();
        if let Some(document) = &self.document {
            for object in visgroup::objects(&document.vmf) {
                if object.quick_hidden() {
                    if object.solid { &mut solids } else { &mut entities }.push(object.id);
                }
            }
        }
        (solids, entities)
    }

    /// The changes that put the selected objects in `group`.
    fn selection_changes(
        &self,
        objects: &[Object],
        group: &VisGroup,
        options: MoveOptions,
    ) -> Result<Vec<Change>, String> {
        let changes: Vec<Change> = objects
            .iter()
            .filter(|object| self.selection.contains(&owner_of(object)))
            .map(|object| {
                let groups = if options.leave_others {
                    vec![group.id]
                } else {
                    with_group(&object.groups, group.id)
                };
                Change {
                    shown: Some(!options.hide),
                    color: Some(group.color),
                    groups: Some(groups),
                    ..Change::of(object)
                }
            })
            .collect();
        if changes.is_empty() {
            return Err("select the objects to move first".to_owned());
        }
        Ok(changes)
    }

    /// Applies edits and then object changes as one undo step. Returns how many objects
    /// the changes named.
    fn apply_object_changes(&mut self, mut edits: Vec<Edit>, changes: &[Change]) -> Result<usize, String> {
        let vmf = &self.document.as_ref().ok_or("no map is open")?.vmf;
        edits.extend(visgroup::change_objects(vmf, changes));
        if !edits.is_empty() {
            self.apply_edit(&Edit::Batch(edits))?;
        }
        Ok(changes.len())
    }

    /// Selects the objects that show and that `wanted` picks. Returns how many.
    fn select_objects(&mut self, wanted: impl Fn(&Object) -> bool) -> usize {
        let owners: Vec<Owner> = self
            .document
            .as_ref()
            .map(|document| visgroup::objects(&document.vmf))
            .unwrap_or_default()
            .iter()
            .filter(|object| !object.hidden && wanted(object))
            .map(owner_of)
            .collect();
        let count = owners.len();
        self.select(owners, SelectMode::Replace);
        self.message = format!("Selected {count} objects");
        count
    }
}

fn owner_of(object: &Object) -> Owner {
    if object.solid {
        Owner::Solid(object.id)
    } else {
        Owner::Entity(object.id)
    }
}

/// `groups` with `group` added, when it is not in the list yet.
fn with_group(groups: &[u32], group: u32) -> Vec<u32> {
    let mut groups = groups.to_vec();
    if !groups.contains(&group) {
        groups.push(group);
    }
    groups
}

/// A group with a free id, ready to be added to `groups`.
fn new_group(groups: &[VisGroup], name: &str, color: Option<[u8; 3]>) -> VisGroup {
    let id = visgroup::free_id(groups);
    let name = match name.trim() {
        "" => format!("VisGroup {id}"),
        "QuickHide" => format!("QuickHide {id}"),
        name => name.to_owned(),
    };
    let hue = (id as f32 * 0.618_034).fract();
    VisGroup {
        id,
        name,
        color: color.unwrap_or_else(|| Hsva::new(hue, 0.55, 0.9, 1.0).to_srgb()),
        children: Vec::new(),
    }
}

fn find_mut(groups: &mut [VisGroup], id: u32) -> Option<&mut VisGroup> {
    for group in groups {
        if group.id == id {
            return Some(group);
        }
        if let Some(found) = find_mut(&mut group.children, id) {
            return Some(found);
        }
    }
    None
}

/// Takes the groups with these ids out of the tree.
fn remove_groups(groups: &mut Vec<VisGroup>, removed: &[u32]) {
    groups.retain(|group| !removed.contains(&group.id));
    for group in groups {
        remove_groups(&mut group.children, removed);
    }
}

/// Takes out every group that neither has members nor holds a group that has.
fn purge(groups: &mut Vec<VisGroup>, used: &BTreeSet<u32>) {
    for group in groups.iter_mut() {
        purge(&mut group.children, used);
    }
    groups.retain(|group| used.contains(&group.id) || !group.children.is_empty());
}

/// The changes that take every object out of the `removed` groups, showing the ones no
/// group is left to hide.
fn removal_changes(objects: &[Object], removed: &[u32]) -> Vec<Change> {
    objects
        .iter()
        .filter(|object| object.groups.iter().any(|group| removed.contains(group)))
        .map(|object| {
            let groups: Vec<u32> = object
                .groups
                .iter()
                .copied()
                .filter(|group| !removed.contains(group))
                .collect();
            Change {
                shown: groups.is_empty().then_some(true),
                groups: Some(groups),
                ..Change::of(object)
            }
        })
        .collect()
}

/// Adds the rows of `groups` and of the groups nested inside them, parents first.
fn user_rows(groups: &[VisGroup], depth: usize, objects: &[Object], rows: &mut Vec<UserGroupRow>) {
    for group in groups {
        let ids = group.ids();
        let members = objects
            .iter()
            .filter(|object| object.groups.iter().any(|group| ids.contains(group)));
        let (count, hidden) = members.fold((0, 0), |(count, hidden), object| {
            (count + 1, hidden + usize::from(!object.shown))
        });
        rows.push(UserGroupRow {
            id: group.id,
            name: group.name.clone(),
            color: group.color,
            depth,
            members: count,
            state: GroupState::of(count, hidden),
        });
        user_rows(&group.children, depth + 1, objects, rows);
    }
}

#[cfg(test)]
mod tests {
    use glam::DVec3;

    use super::*;

    fn editor_with_objects() -> (Editor, u32, u32, u32) {
        let mut editor = Editor::default();
        editor.new_map();
        let a = editor.create_block(DVec3::ZERO, DVec3::splat(64.0)).unwrap();
        let b = editor.create_block(DVec3::splat(128.0), DVec3::splat(192.0)).unwrap();
        let light = editor.create_entity("light", DVec3::splat(32.0), &[]).unwrap();
        (editor, a, b, light)
    }

    #[test]
    fn a_group_hides_shows_and_deletes() {
        let (mut editor, a, b, light) = editor_with_objects();
        editor.select([Owner::Solid(a), Owner::Entity(light)], SelectMode::Replace);
        let group = editor
            .selection_to_new_visgroup("walls", MoveOptions::default())
            .unwrap();
        assert_eq!(editor.visgroups.user.len(), 1);
        assert_eq!(editor.visgroups.user[0].members, 2);
        assert_eq!(editor.visgroups.user[0].state, GroupState::Shown);

        assert_eq!(editor.set_visgroup_shown(group, false).unwrap(), 2);
        assert_eq!(editor.visgroups.user[0].state, GroupState::Hidden);
        assert!(!editor.scene.contains(Owner::Solid(a)) && !editor.scene.contains(Owner::Entity(light)));
        assert!(editor.scene.contains(Owner::Solid(b)));
        assert_eq!(
            editor.show_all_hidden().unwrap(),
            0,
            "QuickHide's show leaves VisGroups alone"
        );

        let lights = AutoGroup::Lights;
        let row = |editor: &Editor| *editor.visgroups.auto.iter().find(|row| row.group == lights).unwrap();
        assert_eq!((row(&editor).members, row(&editor).state), (1, GroupState::Shown));

        editor.set_visgroup_shown(group, true).unwrap();
        assert!(editor.scene.contains(Owner::Solid(a)) && editor.scene.contains(Owner::Entity(light)));
        assert_eq!(editor.select_visgroup(group).unwrap(), 2);

        editor.set_auto_visgroup_shown(lights, false).unwrap();
        assert_eq!(row(&editor).state, GroupState::Hidden);
        assert!(!editor.scene.contains(Owner::Entity(light)));
        editor.show_all_visgroups().unwrap();
        assert!(editor.scene.contains(Owner::Entity(light)));

        editor.set_visgroup_shown(group, false).unwrap();
        editor.delete_visgroup(group).unwrap();
        assert!(editor.visgroups.user.is_empty());
        assert!(
            editor.scene.contains(Owner::Solid(a)),
            "members of a removed group show again"
        );
        editor.step_history(false).unwrap();
        assert_eq!(editor.visgroups.user[0].state, GroupState::Hidden);
    }

    #[test]
    fn quickhide_converts_and_membership_toggles() {
        let (mut editor, a, b, _) = editor_with_objects();
        editor.select([Owner::Solid(a)], SelectMode::Replace);
        editor.hide_selection().unwrap();
        assert_eq!(editor.visgroups.quick_hidden, 1);
        let group = editor.quick_hidden_to_visgroup().unwrap();
        assert_eq!(editor.visgroups.quick_hidden, 0);
        assert_eq!(editor.visgroups.user[0].state, GroupState::Hidden);

        editor.select([Owner::Solid(b)], SelectMode::Replace);
        editor.set_selection_in_visgroup(group, true).unwrap();
        assert_eq!(editor.visgroups.user[0].members, 2);
        assert_eq!(editor.visgroups.user[0].state, GroupState::Partial);
        assert_eq!(editor.selection_visgroups().0.get(&group), Some(&1));
        editor.set_selection_in_visgroup(group, false).unwrap();
        assert_eq!(editor.visgroups.user[0].members, 1);

        let empty = editor.create_visgroup("", None).unwrap();
        assert_ne!(empty, group);
        assert_eq!(editor.purge_empty_visgroups().unwrap(), 1);
        assert_eq!(editor.visgroups.user.len(), 1);
    }
}
