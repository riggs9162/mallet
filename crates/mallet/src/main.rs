//! Mallet: a Hammer-style level editor for Source engine VMF maps.
//!
//! Pass a `.vmf` or `.mallet` path as the first argument to open it on start, or drop one onto
//! the window. When Mallet is already running, a map passed this way opens in a tab of that
//! Mallet instead, which is what double-clicking a map in Explorer does.
//! The window always opens centred on the main monitor, keeping the size and maximized state
//! it had when Mallet last closed.
//! AI agents can drive the editor through its MCP server; see `mcp`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![recursion_limit = "256"]

mod app;
mod branding;
mod canvas;
mod clipper;
mod compile;
mod config;
mod decals;
mod document;
mod editor;
mod face_pick;
mod faces;
mod gizmo;
mod handoff;
mod input;
mod keymap;
mod lighting;
mod mcp;
mod model_render;
mod netgraph;
mod position_pick;
mod presence;
mod problems;
mod prop_brushes;
mod properties;
mod render3d;
mod scene;
mod shell;
mod sound;
mod style;
mod tools;
mod transform;
mod vertex_tool;
mod view2d;
mod view3d;

use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result {
    report_crashes();
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    let settings = config::Config::load();
    // A map opened from Explorer while Mallet runs goes to that Mallet, as in Hammer.
    if let (Some(path), Ok(Some(port))) = (&path, mcp::configured_port(&settings.agents)) {
        if handoff::hand_off(path, port) {
            return Ok(());
        }
    }
    let present_mode = match std::env::var("MALLET_PRESENT").as_deref() {
        Ok("fifo") => eframe::wgpu::PresentMode::Fifo,
        Ok("mailbox") => eframe::wgpu::PresentMode::Mailbox,
        Ok("immediate") => eframe::wgpu::PresentMode::Immediate,
        Ok("novsync") => eframe::wgpu::PresentMode::AutoNoVsync,
        _ if settings.interface.vsync => eframe::wgpu::PresentMode::AutoVsync,
        _ => eframe::wgpu::PresentMode::AutoNoVsync,
    };
    let latency = std::env::var("MALLET_FRAME_LATENCY")
        .ok()
        .and_then(|value| value.parse().ok())
        .or(Some(1));
    // eframe keeps the window's size and place and every panel's position and size in
    // %APPDATA%\<app id>\data. MALLET_PROFILE picks a separate store, so test copies do
    // not move your windows around.
    let app_id = config::app_folder();
    let options = eframe::NativeOptions {
        persist_window: true,
        centered: true,
        viewport: egui::ViewportBuilder::default()
            .with_title("Mallet")
            .with_app_id(app_id.clone())
            .with_icon(std::sync::Arc::new(branding::window_icon()))
            .with_inner_size([1600.0, 1000.0])
            .with_drag_and_drop(true),
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            present_mode,
            desired_maximum_frame_latency: latency,
            wgpu_setup: render3d::wgpu_setup(),
            ..Default::default()
        },
        ..Default::default()
    };
    eframe::run_native(
        &app_id,
        options,
        Box::new(move |cc| Ok(Box::new(app::MalletApp::new(cc, path)))),
    )
}

/// Has Mallet write what went wrong to `crash.log` in its `%APPDATA%` folder when an
/// error stops it, as its window otherwise just closes without a word. Only an error on
/// the main thread stops Mallet; the worker threads' are recovered from and left out.
fn report_crashes() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let folder = config::data_dir().filter(|_| std::thread::current().name() == Some("main"));
        if let Some(folder) = folder {
            let version = env!("CARGO_PKG_VERSION");
            let trace = std::backtrace::Backtrace::force_capture();
            let _ = std::fs::create_dir_all(&folder);
            let report = format!("Mallet {version} stopped: {info}\n\n{trace}\n");
            let _ = std::fs::write(folder.join("crash.log"), report);
        }
        previous(info);
    }));
}
