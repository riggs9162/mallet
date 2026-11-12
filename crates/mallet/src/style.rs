//! How the viewports are drawn: the colours and grid spacing chosen in Mallet Options.
//!
//! Drawing code reads the style with [`current`] rather than taking it through every call,
//! as the CPU and GPU views and the MCP screenshots all draw with it. The window copies the
//! settings in with [`set`] every frame, so changes show at once. Cached viewport images
//! hash the style into their signature, so a change redraws them.

use std::sync::RwLock;

use eframe::egui::Color32;

use crate::config::{ColorOptions, Rgb, ViewOptions};

/// Colours and grid spacing of every view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ViewStyle {
    pub background: Color32,
    pub grid: Color32,
    pub grid_highlight: Color32,
    pub grid_major: Color32,
    pub axis: Color32,
    pub selection: Color32,
    pub tool: Color32,
    pub handles: Color32,
    pub selected_faces: Color32,
    /// The 3D view's camera drawn in the 2D views.
    pub camera: Color32,
    /// Grid lines closer than this many pixels are thinned out by doubling their spacing.
    pub grid_min_pixels: u32,
    /// Units between highlighted grid lines; 0 for none.
    pub grid_highlight_every: u32,
    /// Units between major grid lines; 0 for none.
    pub grid_major_every: u32,
}

impl ViewStyle {
    /// Hammer's look, which the default settings give.
    pub const HAMMER: ViewStyle = ViewStyle {
        background: Color32::BLACK,
        grid: Color32::from_gray(38),
        grid_highlight: Color32::from_gray(65),
        grid_major: Color32::from_rgb(100, 46, 0),
        axis: Color32::from_rgb(0, 100, 100),
        selection: Color32::from_rgb(255, 0, 0),
        tool: Color32::WHITE,
        handles: Color32::WHITE,
        selected_faces: Color32::from_rgb(255, 210, 60),
        camera: Color32::from_rgb(255, 40, 40),
        grid_min_pixels: 4,
        grid_highlight_every: 64,
        grid_major_every: 1024,
    };

    /// The style the settings describe, with spacings kept in a usable range.
    pub fn from_options(colors: &ColorOptions, view: &ViewOptions) -> Self {
        let color = |[r, g, b]: Rgb| Color32::from_rgb(r, g, b);
        let spacing = |units: f64| {
            if units.is_finite() {
                units.clamp(0.0, 65_536.0) as u32
            } else {
                0
            }
        };
        let min_pixels = if view.grid_min_pixels.is_finite() {
            view.grid_min_pixels.clamp(2.0, 64.0) as u32
        } else {
            4
        };
        Self {
            background: color(colors.background),
            grid: color(colors.grid),
            grid_highlight: color(colors.grid_highlight),
            grid_major: color(colors.grid_major),
            axis: color(colors.axis),
            selection: color(colors.selection),
            tool: color(colors.tool),
            handles: color(colors.handles),
            selected_faces: color(colors.selected_faces),
            camera: color(colors.camera),
            grid_min_pixels: min_pixels,
            grid_highlight_every: spacing(view.grid_highlight),
            grid_major_every: spacing(view.grid_major),
        }
    }

    /// The colour of the grid line at `value` units along an axis.
    pub fn grid_color(&self, value: f64) -> Color32 {
        let on = |every: u32| every > 0 && value % f64::from(every) == 0.0;
        if value == 0.0 {
            self.axis
        } else if on(self.grid_major_every) {
            self.grid_major
        } else if on(self.grid_highlight_every) {
            self.grid_highlight
        } else {
            self.grid
        }
    }
}

impl Default for ViewStyle {
    fn default() -> Self {
        Self::HAMMER
    }
}

static CURRENT: RwLock<ViewStyle> = RwLock::new(ViewStyle::HAMMER);

/// The style views draw with now.
pub fn current() -> ViewStyle {
    CURRENT.read().map_or(ViewStyle::HAMMER, |style| *style)
}

/// Makes `style` the one views draw with.
pub fn set(style: ViewStyle) {
    if current() == style {
        return;
    }
    if let Ok(mut current) = CURRENT.write() {
        *current = style;
    }
}
