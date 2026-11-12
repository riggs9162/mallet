//! The view an MCP agent works in, apart from the window.
//!
//! While Mallet Options > AI agents > "Agents work in their own view" is on, MCP tool
//! calls run against an [`AgentView`]: a tab, camera, 2D views, viewport layout, grid,
//! selection, tool and clipboard that belong to the agent. The maps themselves are
//! shared, so the agent's edits show in the window straight away, but the window's
//! camera, selection, tab and tool stay where the user left them, and the user can keep
//! looking around while the agent works.
//!
//! [`Editor::with_agent_view`] swaps the agent's state into the editor for the length of
//! one tool call and swaps the user's back afterwards, so every editor method works for
//! the agent unchanged.

use std::collections::BTreeSet;
use std::mem::swap;
use std::path::Path;

use glam::DVec3;
use mallet_vmf::geometry::Bounds;

use super::clipboard::{Clipboard, PasteCursor};
use super::{Editor, Tool, ViewMode, DEFAULT_BLOCK};
use crate::clipper::Clipper;
use crate::faces::TextureMode;
use crate::scene::{camera_marker_edges, Overlay, Owner};
use crate::vertex_tool::VertexState;
use crate::view2d::View2D;
use crate::view3d::Camera3D;

/// What the agent sees and has selected in its tab. It starts again whenever the agent
/// moves to another tab.
struct AgentMap {
    views: [View2D; 3],
    camera: Camera3D,
    selection: BTreeSet<Owner>,
    face_selection: BTreeSet<u32>,
    overlay: Overlay,
    last_block: Bounds,
    vertex: VertexState,
}

/// The agent's tool settings, which it keeps from tab to tab.
struct AgentTools {
    layout: [ViewMode; 4],
    grid: f64,
    tool: Tool,
    clipper: Clipper,
    texture_mode: TextureMode,
    lifted: Option<mallet_vmf::Side>,
    clipboard: Clipboard,
    paste_cursor: Option<PasteCursor>,
}

/// Everything an agent working in its own view has apart from the window.
pub struct AgentView {
    /// The tab the agent works in, by its id in [`Editor::tab_ids`].
    tab: u64,
    map: AgentMap,
    tools: AgentTools,
}

impl Editor {
    /// Whether a tool call is running in the agent's own view right now.
    pub fn in_agent_view(&self) -> bool {
        self.window_tab.is_some()
    }

    /// Index of the tab the window shows, also while an agent works in another one.
    pub fn window_tab(&self) -> usize {
        self.window_tab.unwrap_or(self.active_tab)
    }

    /// Index of the tab an agent working in its own view uses. Until its first call, or
    /// once its tab is closed, that is the tab the window shows.
    pub fn agent_tab(&self) -> usize {
        if self.in_agent_view() {
            return self.active_tab;
        }
        self.agent
            .as_ref()
            .and_then(|agent| self.tab_index(agent.tab))
            .unwrap_or(self.active_tab)
    }

    /// Edges that show where the agent's camera is, for the window's views: only while
    /// agents work in their own view and the agent's tab is the one the window shows.
    pub fn agent_camera_marker(&self) -> Vec<[DVec3; 2]> {
        if self.in_agent_view() || !self.config.agents.own_view {
            return Vec::new();
        }
        let Some(agent) = &self.agent else {
            return Vec::new();
        };
        if self.tab_index(agent.tab) != Some(self.active_tab) {
            return Vec::new();
        }
        let camera = &agent.map.camera;
        camera_marker_edges(camera.position, camera.forward(), camera.fov)
    }

    fn tab_index(&self, id: u64) -> Option<usize> {
        self.tab_ids.iter().position(|tab| *tab == id)
    }

    /// Runs `run` with the agent's tab, camera, views, selection and tools in place of
    /// the user's, then puts the user's back. Selections that lost objects to the
    /// agent's edits are trimmed, and the agent's status line and clipboard text are
    /// kept out of the window.
    pub fn with_agent_view<R>(&mut self, run: impl FnOnce(&mut Self) -> R) -> R {
        let mut agent = self.take_agent();
        let window_tab = self.tab_ids[self.active_tab];
        let message = self.message.clone();
        let system_clipboard = self.system_clipboard.take();
        self.swap_agent_tools(&mut agent.tools);
        self.window_tab = Some(self.active_tab);
        let index = self.tab_index(agent.tab).unwrap_or(self.active_tab);
        self.bring_to_front(index);
        self.swap_agent_map(&mut agent.map);
        self.sync_selection();

        let result = run(self);

        agent.tab = self.tab_ids[self.active_tab];
        self.swap_agent_map(&mut agent.map);
        self.sync_selection();
        let index = self.tab_index(window_tab).unwrap_or(self.active_tab);
        self.bring_to_front(index);
        self.window_tab = None;
        self.swap_agent_tools(&mut agent.tools);
        self.refresh_clip_preview();
        self.message = message;
        self.system_clipboard = system_clipboard;
        self.agent = Some(agent);
        result
    }

    /// Points the agent at another tab, leaving the window on the one it shows.
    pub fn agent_switch_tab(&mut self, index: usize) -> Result<(), String> {
        if index >= self.tabs.len() {
            return Err(format!("there is no tab {index}; {} are open", self.tabs.len()));
        }
        let mut agent = self.take_agent();
        if self.tab_index(agent.tab) != Some(index) {
            agent.tab = self.tab_ids[index];
            agent.map = self.agent_map(index);
        }
        self.agent = Some(agent);
        Ok(())
    }

    /// Opens a new tab with an empty map for the agent, behind the tab the window shows.
    pub fn agent_new_tab(&mut self) {
        let _ = self.agent_add_tab(|_| Ok(()));
    }

    /// Opens a map in a new tab for the agent, behind the tab the window shows. A map
    /// that is already open becomes the agent's tab instead.
    pub fn agent_open(&mut self, path: &Path) -> Result<(), String> {
        match self.tab_of_path(path) {
            Some(index) => self.agent_switch_tab(index),
            None => self.agent_add_tab(|editor| editor.open(path)),
        }
    }

    /// Closes a tab for the agent. The tab the window shows is the user's to close.
    pub fn agent_close_tab(&mut self, index: usize) -> Result<(), String> {
        if index >= self.tabs.len() {
            return Err(format!("there is no tab {index}; {} are open", self.tabs.len()));
        }
        if index == self.active_tab {
            return Err(format!(
                "tab {index} is the one the window shows, so only the user can close it"
            ));
        }
        self.close_tab(index)
    }

    /// Adds a tab after the window's, fills it with `fill` and gives it to the agent,
    /// then brings the window's tab back to the front. The tab is closed again if
    /// `fill` fails.
    fn agent_add_tab(&mut self, fill: impl FnOnce(&mut Self) -> Result<(), String>) -> Result<(), String> {
        let mut agent = self.take_agent();
        let message = self.message.clone();
        let window_tab = self.active_tab;
        self.swap_agent_tools(&mut agent.tools);
        self.new_tab();
        let result = fill(self);
        let added = self.active_tab;
        if result.is_ok() {
            agent.tab = self.tab_ids[added];
            agent.map = self.agent_map(added);
        }
        self.bring_to_front(window_tab);
        if result.is_err() {
            let _ = self.close_tab(added);
        }
        self.swap_agent_tools(&mut agent.tools);
        self.message = message;
        self.agent = Some(agent);
        result
    }

    /// The agent's state, starting it on the window's tab when it has none or its tab
    /// was closed.
    fn take_agent(&mut self) -> AgentView {
        match self.agent.take() {
            Some(agent) if self.tab_index(agent.tab).is_some() => agent,
            previous => AgentView {
                tab: self.tab_ids[self.active_tab],
                map: self.agent_map(self.active_tab),
                tools: previous.map_or_else(|| self.agent_tools(), |agent| agent.tools),
            },
        }
    }

    /// A fresh agent view of a tab: looking from where that tab's own camera and 2D views
    /// look, with nothing selected.
    fn agent_map(&self, index: usize) -> AgentMap {
        let (camera, views) = match &self.tabs[index] {
            Some(parked) if index != self.active_tab => (&parked.camera, &parked.views),
            _ => (&self.camera, &self.views),
        };
        AgentMap {
            views: views.clone(),
            camera: Camera3D::looking_like(camera),
            selection: BTreeSet::new(),
            face_selection: BTreeSet::new(),
            overlay: Overlay::default(),
            last_block: DEFAULT_BLOCK,
            vertex: VertexState::default(),
        }
    }

    /// The agent's first tool settings: the window's layout, grid and texture mode, the
    /// selection tool, and an empty clipboard.
    fn agent_tools(&self) -> AgentTools {
        AgentTools {
            layout: self.layout,
            grid: self.grid,
            tool: Tool::Select,
            clipper: Clipper::default(),
            texture_mode: self.texture_mode,
            lifted: None,
            clipboard: Clipboard::default(),
            paste_cursor: None,
        }
    }

    fn swap_agent_map(&mut self, map: &mut AgentMap) {
        swap(&mut self.views, &mut map.views);
        swap(&mut self.camera, &mut map.camera);
        swap(&mut self.selection, &mut map.selection);
        swap(&mut self.face_selection, &mut map.face_selection);
        swap(&mut self.overlay, &mut map.overlay);
        swap(&mut self.last_block, &mut map.last_block);
        swap(&mut self.vertex, &mut map.vertex);
    }

    fn swap_agent_tools(&mut self, tools: &mut AgentTools) {
        swap(&mut self.layout, &mut tools.layout);
        swap(&mut self.grid, &mut tools.grid);
        swap(&mut self.tool, &mut tools.tool);
        swap(&mut self.clipper, &mut tools.clipper);
        swap(&mut self.texture_mode, &mut tools.texture_mode);
        swap(&mut self.lifted, &mut tools.lifted);
        swap(&mut self.clipboard, &mut tools.clipboard);
        swap(&mut self.paste_cursor, &mut tools.paste_cursor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::SelectMode;

    fn editor_with_block() -> (Editor, u32) {
        let mut editor = Editor::default();
        editor.new_map();
        let solid = editor.create_block(DVec3::splat(-64.0), DVec3::splat(64.0)).unwrap();
        editor.clear_selection();
        (editor, solid)
    }

    #[test]
    fn the_agent_selects_and_aims_without_touching_the_window() {
        let (mut editor, solid) = editor_with_block();
        let camera = editor.camera.position;
        let grid = editor.grid;
        editor.with_agent_view(|editor| {
            editor.select([Owner::Solid(solid)], SelectMode::Replace);
            editor.camera.position = DVec3::new(1000.0, 0.0, 0.0);
            editor.grid = 8.0;
            editor.set_tool(Tool::Block);
        });
        assert!(editor.selection.is_empty());
        assert_eq!(editor.camera.position, camera);
        assert_eq!(editor.grid, grid);
        assert_eq!(editor.tool, Tool::Select);
        let agent = editor.with_agent_view(|editor| {
            (
                editor.selection.clone(),
                editor.camera.position,
                editor.grid,
                editor.tool,
            )
        });
        assert_eq!(agent.0, BTreeSet::from([Owner::Solid(solid)]));
        assert_eq!(agent.1, DVec3::new(1000.0, 0.0, 0.0));
        assert_eq!(agent.2, 8.0);
        assert_eq!(agent.3, Tool::Block);
    }

    #[test]
    fn the_window_shows_the_agent_camera_only_in_the_agent_tab() {
        let (mut editor, _) = editor_with_block();
        assert!(editor.display_overlay().agent_camera.is_empty());
        editor.with_agent_view(|editor| {
            editor.camera.position = DVec3::new(500.0, 0.0, 0.0);
            assert!(editor.display_overlay().agent_camera.is_empty());
        });
        let marker = editor.display_overlay().agent_camera;
        assert!(marker.iter().flatten().any(|point| point.x > 500.0));
        editor.agent_new_tab();
        assert!(editor.display_overlay().agent_camera.is_empty());
        editor.config.agents.own_view = false;
        editor.agent_switch_tab(0).unwrap();
        assert!(editor.display_overlay().agent_camera.is_empty());
    }

    #[test]
    fn agent_edits_reach_the_shared_map() {
        let (mut editor, solid) = editor_with_block();
        editor.select([Owner::Solid(solid)], SelectMode::Replace);
        editor.with_agent_view(|editor| {
            editor.select([Owner::Solid(solid)], SelectMode::Replace);
            editor.delete_selection().unwrap();
        });
        assert!(!editor.scene.contains(Owner::Solid(solid)));
        assert!(
            editor.selection.is_empty(),
            "the window drops objects the agent deleted"
        );
    }

    #[test]
    fn the_agent_works_in_a_background_tab() {
        let (mut editor, _) = editor_with_block();
        editor.agent_new_tab();
        assert_eq!(editor.tabs().len(), 2);
        assert_eq!(editor.active_tab(), 0, "the window stays on its tab");
        assert_eq!(editor.agent_tab(), 1);
        editor.with_agent_view(|editor| {
            assert_eq!(editor.active_tab(), 1);
            editor.create_block(DVec3::ZERO, DVec3::splat(32.0)).unwrap();
        });
        assert_eq!(editor.active_tab(), 0);
        assert_eq!(editor.scene.solid_count, 1);
        editor.switch_tab(1).unwrap();
        assert_eq!(editor.scene.solid_count, 1);
        assert!(editor.tabs()[1].dirty);
    }

    #[test]
    fn the_agent_cannot_close_the_window_tab() {
        let (mut editor, _) = editor_with_block();
        editor.agent_new_tab();
        assert!(editor.agent_close_tab(0).is_err());
        editor.agent_close_tab(1).unwrap();
        assert_eq!(editor.tabs().len(), 1);
        assert_eq!(editor.agent_tab(), 0, "the agent falls back to the window's tab");
    }

    #[test]
    fn the_agent_follows_its_tab_when_tabs_move() {
        let (mut editor, _) = editor_with_block();
        editor.agent_new_tab();
        editor.switch_tab(1).unwrap();
        editor.new_tab();
        editor.switch_tab(0).unwrap();
        editor.close_tab(2).unwrap();
        assert_eq!(editor.agent_tab(), 1);
        editor.new_tab();
        editor.switch_tab(0).unwrap();
        assert_eq!(editor.agent_tab(), 2, "a tab opened in front of it moves it along");
    }
}
