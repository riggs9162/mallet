//! Keeps the hidden cursor inside the 3D viewport during mouselook.
//!
//! winit can only confine the cursor to the whole window, so on Windows the cursor is also
//! clipped to the viewport's own rectangle with `ClipCursor`. Elsewhere the window's
//! confinement and the re-centring each frame keep it in place.

use eframe::egui::{Context, Rect};

/// Clips the cursor to `rect`, a viewport in egui points, or lets it go with `None`.
/// Windows drops a clip when another window takes the focus, so mouselook calls this
/// every frame.
pub fn clip(ctx: &Context, rect: Option<Rect>) {
    let screen = rect.and_then(|rect| {
        let window = ctx.input(|input| input.viewport().inner_rect)?;
        let scale = ctx.pixels_per_point();
        let min = (window.min + rect.min.to_vec2()) * scale;
        let max = (window.min + rect.max.to_vec2()) * scale;
        Some([min.x.round() as i32, min.y.round() as i32, max.x.round() as i32, max.y.round() as i32])
    });
    clip_screen(screen);
}

#[cfg(windows)]
fn clip_screen(screen: Option<[i32; 4]>) {
    #[repr(C)]
    struct WinRect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn ClipCursor(rect: *const WinRect) -> i32;
    }

    let area = screen.map(|[left, top, right, bottom]| WinRect {
        left,
        top,
        right,
        bottom,
    });
    let pointer = area.as_ref().map_or(std::ptr::null(), |area| area as *const WinRect);
    // SAFETY: the pointer is null or points at a rectangle that outlives the call, which
    // only reads it.
    unsafe {
        ClipCursor(pointer);
    }
}

#[cfg(not(windows))]
fn clip_screen(_screen: Option<[i32; 4]>) {}
