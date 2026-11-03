//! shocora — egui front end.
//!
//! ```text
//! shocora [--graphics wgpu|vulkan|opengl] [TITLE_DIR | FILE.rpx]
//! ```
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod fonts;
mod lang;
mod launcher;
mod netplay;
mod presenter;

use std::path::PathBuf;

use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
use shocora_gui_common::config::AppConfig;
use shocora_render::{GraphicsApi, wgpu};

/// Parsed command line.
#[derive(Debug, Default)]
struct Args {
    graphics: Option<GraphicsApi>,
    title: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut iter = std::env::args_os().skip(1);
    while let Some(arg) = iter.next() {
        match arg.to_str() {
            Some("--graphics" | "-g") => {
                let value = iter.next().and_then(|v| v.into_string().ok()).unwrap_or_default();
                args.graphics = Some(
                    GraphicsApi::parse(&value)
                        .ok_or_else(|| format!("unknown graphics API {value:?}"))?,
                );
            }
            Some("--help" | "-h") => {
                return Err(
                    "usage: shocora [--graphics wgpu|vulkan|opengl] [TITLE_DIR | FILE.rpx]".into(),
                );
            }
            _ => args.title = Some(PathBuf::from(arg)),
        }
    }
    Ok(args)
}

fn native_options(api: GraphicsApi) -> eframe::NativeOptions {
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("shocora")
        .with_inner_size([1400.0, 640.0])
        // The start screen's backdrop color may let the desktop show through.
        .with_transparent(true);

    match eframe::icon_data::from_png_bytes(include_bytes!("../assets/shocora.png")) {
        Ok(icon) => viewport = viewport.with_icon(icon),
        Err(error) => log::warn!("window icon: {error}"),
    }

    let mut options = eframe::NativeOptions { viewport, ..eframe::NativeOptions::default() };
    options.renderer = eframe::Renderer::Wgpu;

    let mut setup = WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = api.wgpu_backends();
    setup.power_preference = wgpu::PowerPreference::HighPerformance;

    // The renderer needs more than egui's defaults: compressed textures and big storage buffers.
    setup.device_descriptor = std::sync::Arc::new(|adapter| wgpu::DeviceDescriptor {
        label: Some("shocora"),
        required_features: shocora_render::wanted_features(adapter),
        required_limits: adapter.limits(),
        ..Default::default()
    });
    options.wgpu_options =
        WgpuConfiguration { wgpu_setup: WgpuSetup::CreateNew(setup), ..Default::default() };
    options
}

fn main() -> eframe::Result {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    // Read shocora.toml
    let config_path = AppConfig::default_path();
    let mut config = AppConfig::load(&config_path);

    // Override graphics API
    if let Some(api) = args.graphics {
        config.graphics_api = api;
    }
    let api = config.graphics_api;
    log::info!("graphics API: {}", api.label());

    eframe::run_native(
        "shocora",
        native_options(api),
        Box::new(move |cc| {
            Ok(Box::new(app::ShocoraApp::new(cc, config, config_path, api, args.title)?))
        }),
    )
}
