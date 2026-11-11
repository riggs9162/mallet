//! Editor actions and their key bindings, starting from Hammer++'s bindings.
//!
//! Bindings live in one table so they can be rebound later without touching the code
//! that reacts to them.

use std::collections::HashSet;

use eframe::egui::{Event, InputState, Key, Modifiers};

/// Something the user can trigger from the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    NewFile,
    CloseTab,
    NextTab,
    PreviousTab,
    OpenFile,
    SaveFile,
    Undo,
    Redo,
    /// Copy the selection to the clipboard, then delete it.
    Cut,
    /// Copy the selection to the clipboard.
    Copy,
    /// Paste the clipboard under the cursor.
    Paste,
    /// Open the Paste Special dialog.
    PasteSpecial,
    GridSmaller,
    GridLarger,
    /// Turn snapping to the grid on or off, as Hammer's Map > Snap to Grid does.
    ToggleSnapToGrid,
    /// Show or hide the grid in the 2D views, as Hammer's Map > Show Grid does.
    ToggleShowGrid,
    SelectionTool,
    /// Switch to the Camera tool.
    CameraTool,
    BlockTool,
    EntityTool,
    TextureTool,
    /// Switch to the Clipping tool, or with it active, cycle which halves it keeps.
    ClipTool,
    /// Toggle the Clipping tool between two-point and three-point clipping.
    ClipThreePoint,
    VertexTool,
    /// Switch to the Overlay tool.
    OverlayTool,
    /// Switch to the Decal tool.
    DecalTool,
    /// Split the face between the two selected vertices.
    SplitFace,
    Properties,
    /// Open the compile window (Hammer's Run Map).
    Compile,
    /// Compile again with the preset used last.
    CompileAgain,
    /// Open Mallet Options.
    Options,
    SelectAll,
    ClearSelection,
    DeleteSelection,
    CreateBlock,
    CenterViews2D,
    CenterView3D,
    /// Open the Transform dialog.
    Transform,
    /// Move the selection so its corner lands on the grid.
    SnapToGrid,
    /// Snap each selected object to the grid on its own.
    SnapToGridIndividually,
    /// Mirror the selection left to right in the 2D view used last.
    FlipHorizontal,
    /// Mirror the selection top to bottom in the 2D view used last.
    FlipVertical,
    /// Line the selected objects up with an edge of the selection in the 2D view used last.
    AlignLeft,
    AlignRight,
    AlignTop,
    AlignBottom,
    ToggleTextureLock,
    ToggleTextureScaleLock,
    /// Show or hide the 3D transform gizmo.
    ToggleGizmo,
    /// Switch the 3D transform gizmo to its next mode: move, rotate, then scale.
    CycleGizmoMode,
    /// Turn snapping rotations to the rotation step on or off.
    ToggleRotationSnap,
    ToggleMouseLook,
    MoveForward,
    MoveBack,
    StrafeLeft,
    StrafeRight,
    /// Fly the 3D camera along its own up direction.
    StrafeUp,
    /// Fly the 3D camera along its own down direction.
    StrafeDown,
    TurnLeft,
    TurnRight,
    LookUp,
    LookDown,
}

/// The actions that move or turn the 3D camera while their key is held.
const MOVEMENT_ACTIONS: [Action; 10] = [
    Action::MoveForward,
    Action::MoveBack,
    Action::StrafeLeft,
    Action::StrafeRight,
    Action::StrafeUp,
    Action::StrafeDown,
    Action::TurnLeft,
    Action::TurnRight,
    Action::LookUp,
    Action::LookDown,
];

impl Action {
    /// A readable name built from the variant's name: `CenterViews2D` reads as
    /// "Center views 2D".
    pub fn name(&self) -> String {
        let debug = format!("{self:?}");
        let mut name = String::new();
        let mut previous: Option<char> = None;
        for c in debug.chars() {
            match previous {
                // A letter right after a digit, as in "2D", stays with it.
                Some(before) if before.is_ascii_digit() => name.push(c),
                Some(_) if c.is_ascii_digit() => {
                    name.push(' ');
                    name.push(c);
                }
                Some(_) if c.is_ascii_uppercase() => {
                    name.push(' ');
                    name.push(c.to_ascii_lowercase());
                }
                _ => name.push(c),
            }
            previous = Some(c);
        }
        name
    }
}

/// A key plus the exact modifiers that must be held with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chord {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Chord {
    /// The key on its own.
    pub const fn key(key: Key) -> Self {
        Self {
            key,
            ctrl: false,
            shift: false,
            alt: false,
        }
    }

    /// Ctrl plus the key.
    pub const fn ctrl(key: Key) -> Self {
        Self {
            ctrl: true,
            ..Self::key(key)
        }
    }

    /// Shift plus the key.
    pub const fn shift(key: Key) -> Self {
        Self {
            shift: true,
            ..Self::key(key)
        }
    }

    /// Alt plus the key.
    pub const fn alt(key: Key) -> Self {
        Self {
            alt: true,
            ..Self::key(key)
        }
    }

    /// Ctrl+Shift plus the key.
    pub const fn ctrl_shift(key: Key) -> Self {
        Self {
            ctrl: true,
            shift: true,
            ..Self::key(key)
        }
    }

    fn modifiers_match(&self, modifiers: &Modifiers) -> bool {
        modifiers.ctrl == self.ctrl && modifiers.shift == self.shift && modifiers.alt == self.alt
    }

    /// Whether the held modifiers trigger this chord. Brackets also count when typed
    /// with AltGr, which Windows reports as Ctrl+Alt, because keyboards such as the German
    /// one have no other way to type them.
    fn triggered_by(&self, modifiers: &Modifiers) -> bool {
        let alt_gr = modifiers.ctrl && modifiers.alt && !self.ctrl && !self.alt;
        let typed_with_alt_gr = matches!(self.key, Key::OpenBracket | Key::CloseBracket) && alt_gr;
        self.modifiers_match(modifiers) || (typed_with_alt_gr && modifiers.shift == self.shift)
    }

    /// Text shown next to menu entries, such as `Ctrl+O`.
    pub fn label(&self) -> String {
        let mut label = String::new();
        if self.ctrl {
            label.push_str("Ctrl+");
        }
        if self.shift {
            label.push_str("Shift+");
        }
        if self.alt {
            label.push_str("Alt+");
        }
        label.push_str(self.key.symbol_or_name());
        label
    }
}

/// The table of bindings.
pub struct KeyMap {
    bindings: Vec<(Action, Chord)>,
}

impl KeyMap {
    /// Hammer++'s bindings for the actions Mallet supports so far, as its keybindings
    /// file sets them. Actions Hammer++ has no key for keep Mallet's own, unless that key
    /// does something else in Hammer++: closing a tab uses Ctrl+F4 because Hammer++ puts
    /// group ignore on Ctrl+W. The camera turns with the mouse only, since Hammer++'s
    /// arrow keys strafe. Stock Hammer's keys that the Valve Developer Community tutorials
    /// teach work as well where nothing else uses them: `[` and `]` (or Alt+A and Alt+S)
    /// for the grid size and Shift+R to show or hide the grid.
    pub fn hammer_plus_plus_defaults() -> Self {
        Self {
            bindings: vec![
                (Action::NewFile, Chord::ctrl(Key::N)),
                (Action::CloseTab, Chord::ctrl(Key::F4)),
                (Action::NextTab, Chord::ctrl(Key::Tab)),
                (Action::PreviousTab, Chord::ctrl_shift(Key::Tab)),
                (Action::OpenFile, Chord::ctrl(Key::O)),
                (Action::SaveFile, Chord::ctrl(Key::S)),
                (Action::Undo, Chord::ctrl(Key::Z)),
                (Action::Redo, Chord::ctrl(Key::Y)),
                (Action::Cut, Chord::ctrl(Key::X)),
                (Action::Cut, Chord::shift(Key::Delete)),
                (Action::Copy, Chord::ctrl(Key::C)),
                (Action::Copy, Chord::ctrl(Key::Insert)),
                (Action::Paste, Chord::ctrl(Key::V)),
                (Action::Paste, Chord::shift(Key::Insert)),
                (Action::PasteSpecial, Chord::ctrl_shift(Key::V)),
                (Action::GridSmaller, Chord::key(Key::Minus)),
                (Action::GridLarger, Chord::key(Key::Plus)),
                (Action::GridSmaller, Chord::key(Key::OpenBracket)),
                (Action::GridLarger, Chord::key(Key::CloseBracket)),
                (Action::GridSmaller, Chord::alt(Key::A)),
                (Action::GridLarger, Chord::alt(Key::S)),
                (Action::ToggleSnapToGrid, Chord::shift(Key::W)),
                (Action::ToggleShowGrid, Chord::shift(Key::R)),
                (Action::SelectionTool, Chord::shift(Key::S)),
                (Action::CameraTool, Chord::shift(Key::C)),
                (Action::BlockTool, Chord::shift(Key::B)),
                (Action::EntityTool, Chord::shift(Key::E)),
                (Action::TextureTool, Chord::shift(Key::A)),
                (Action::ClipTool, Chord::shift(Key::X)),
                (Action::ClipThreePoint, Chord::key(Key::C)),
                (Action::VertexTool, Chord::shift(Key::V)),
                (Action::OverlayTool, Chord::shift(Key::O)),
                (Action::DecalTool, Chord::shift(Key::D)),
                (Action::SplitFace, Chord::ctrl(Key::F)),
                (Action::Properties, Chord::alt(Key::Enter)),
                (Action::Compile, Chord::key(Key::F9)),
                (Action::CompileAgain, Chord::shift(Key::F9)),
                (Action::SelectAll, Chord::ctrl_shift(Key::A)),
                (Action::ClearSelection, Chord::shift(Key::Q)),
                (Action::ClearSelection, Chord::key(Key::Escape)),
                (Action::DeleteSelection, Chord::key(Key::Delete)),
                (Action::CreateBlock, Chord::key(Key::Enter)),
                (Action::CenterViews2D, Chord::ctrl(Key::E)),
                (Action::CenterView3D, Chord::ctrl(Key::R)),
                (Action::Transform, Chord::ctrl(Key::M)),
                (Action::SnapToGrid, Chord::ctrl(Key::B)),
                (Action::SnapToGridIndividually, Chord::ctrl_shift(Key::B)),
                (Action::FlipHorizontal, Chord::ctrl(Key::L)),
                (Action::FlipVertical, Chord::ctrl(Key::I)),
                (Action::ToggleTextureLock, Chord::shift(Key::L)),
                (Action::CycleGizmoMode, Chord::key(Key::Q)),
                (Action::ToggleMouseLook, Chord::key(Key::Z)),
                (Action::MoveForward, Chord::key(Key::W)),
                (Action::MoveBack, Chord::key(Key::S)),
                (Action::StrafeLeft, Chord::key(Key::A)),
                (Action::StrafeRight, Chord::key(Key::D)),
                (Action::StrafeLeft, Chord::key(Key::ArrowLeft)),
                (Action::StrafeRight, Chord::key(Key::ArrowRight)),
                (Action::StrafeUp, Chord::key(Key::ArrowUp)),
                (Action::StrafeDown, Chord::key(Key::ArrowDown)),
            ],
        }
    }

    /// Every binding, in table order.
    pub fn bindings(&self) -> &[(Action, Chord)] {
        &self.bindings
    }

    /// The first chord bound to an action, for display in menus.
    pub fn chord(&self, action: Action) -> Option<Chord> {
        self.bindings
            .iter()
            .find(|(bound, _)| *bound == action)
            .map(|(_, chord)| *chord)
    }

    /// Menu label for an action's shortcut, or an empty string when it has none.
    pub fn label(&self, action: Action) -> String {
        self.chord(action).map(|chord| chord.label()).unwrap_or_default()
    }

    /// The bindings that use exactly the held modifiers, one per action, for the key hints
    /// shown while a modifier is held. Empty when no modifier is held, and movement keys
    /// are left out because they act while held rather than as shortcuts.
    pub fn hints(&self, modifiers: &Modifiers) -> Vec<(Action, Chord)> {
        if !modifiers.ctrl && !modifiers.shift && !modifiers.alt {
            return Vec::new();
        }
        let mut hints: Vec<(Action, Chord)> = Vec::new();
        for (action, chord) in &self.bindings {
            if chord.modifiers_match(modifiers)
                && !MOVEMENT_ACTIONS.contains(action)
                && !hints.iter().any(|(shown, _)| shown == action)
            {
                hints.push((*action, *chord));
            }
        }
        hints
    }

    /// Whether the action was triggered this frame, with exactly its modifiers held.
    ///
    /// The window turns the clipboard actions' keys into copy, cut and paste events, which
    /// [`clipboard_action`] reads, so this only sees those keys in synthetic input.
    pub fn pressed(&self, input: &InputState, action: Action) -> bool {
        self.bindings.iter().any(|(bound, chord)| {
            *bound == action && input.key_pressed(chord.key) && chord.triggered_by(&input.modifiers)
        })
    }

    /// Whether a movement action's key is held. Alt suppresses it, Ctrl does too unless
    /// `crouch` is set, and so does a key in `blocked`: one that went down as part of a
    /// shortcut such as Ctrl+S, which must not move the camera when Ctrl is let go first.
    pub fn held(&self, input: &InputState, action: Action, blocked: &HashSet<Key>, crouch: bool) -> bool {
        (crouch || !input.modifiers.ctrl)
            && !input.modifiers.alt
            && self
                .bindings
                .iter()
                .any(|(bound, chord)| *bound == action && input.key_down(chord.key) && !blocked.contains(&chord.key))
    }

    /// Whether an action is a Ctrl shortcut on a movement key, such as Ctrl+S. In
    /// mouselook Ctrl is crouch, so these chords slow the camera instead of firing.
    pub fn is_crouch_chord(&self, action: Action) -> bool {
        self.chord(action)
            .is_some_and(|chord| chord.ctrl && !chord.alt && self.is_movement_key(chord.key))
    }

    /// Whether a key is bound to moving or turning the 3D camera.
    pub fn is_movement_key(&self, key: Key) -> bool {
        self.bindings
            .iter()
            .any(|(action, chord)| chord.key == key && MOVEMENT_ACTIONS.contains(action))
    }
}

/// The clipboard action a copy, cut or paste event from the window stands for, given the
/// modifiers held with it. Ctrl+Shift+C is left alone, as Hammer uses it for Carve.
pub fn clipboard_action(event: &Event, modifiers: &Modifiers) -> Option<Action> {
    match event {
        Event::Cut => Some(Action::Cut),
        Event::Copy if !modifiers.shift => Some(Action::Copy),
        Event::Paste(_) if modifiers.ctrl && modifiers.shift => Some(Action::PasteSpecial),
        Event::Paste(_) => Some(Action::Paste),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(ctrl: bool, shift: bool, alt: bool) -> Modifiers {
        Modifiers {
            ctrl,
            shift,
            alt,
            ..Modifiers::NONE
        }
    }

    #[test]
    fn hints_list_only_chords_with_exactly_the_held_modifiers() {
        let keymap = KeyMap::hammer_plus_plus_defaults();
        let ctrl = keymap.hints(&held(true, false, false));
        assert!(ctrl.iter().any(|(action, _)| *action == Action::SaveFile));
        assert!(ctrl.iter().all(|(_, chord)| chord.ctrl && !chord.shift && !chord.alt));
        assert!(!ctrl.iter().any(|(action, _)| *action == Action::SelectAll));
        let ctrl_shift = keymap.hints(&held(true, true, false));
        assert!(ctrl_shift.iter().any(|(action, _)| *action == Action::SelectAll));
    }

    #[test]
    fn hints_are_empty_without_a_modifier_and_list_each_action_once() {
        let keymap = KeyMap::hammer_plus_plus_defaults();
        assert!(keymap.hints(&Modifiers::NONE).is_empty());
        let shift = keymap.hints(&held(false, true, false));
        let cuts = shift.iter().filter(|(action, _)| *action == Action::Cut).count();
        assert_eq!(cuts, 1);
    }

    #[test]
    fn brackets_typed_with_alt_gr_change_the_grid() {
        let bracket = Chord::key(Key::OpenBracket);
        assert!(bracket.triggered_by(&Modifiers::NONE));
        assert!(bracket.triggered_by(&held(true, false, true)));
        assert!(!bracket.triggered_by(&held(true, false, false)));
        assert!(!Chord::alt(Key::A).triggered_by(&held(true, false, true)));
    }

    #[test]
    fn q_cycles_the_gizmo_mode() {
        let keymap = KeyMap::hammer_plus_plus_defaults();
        assert_eq!(keymap.chord(Action::CycleGizmoMode), Some(Chord::key(Key::Q)));
        assert_eq!(Action::CycleGizmoMode.name(), "Cycle gizmo mode");
        assert_eq!(Action::CenterViews2D.name(), "Center views 2D");
        assert_eq!(Action::CenterView3D.name(), "Center view 3D");
    }
}
