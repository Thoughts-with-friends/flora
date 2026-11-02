//! Bridges the render thread's screen images to egui textures.
use std::{collections::HashMap, sync::mpsc::SyncSender};

use eframe::{egui, egui_wgpu};
use flora_render::{
    LatteRenderer, RenderCommand, RenderThread, Screen, ScreenImages, UpscaleConfig,
    UpscaleSetting, wgpu,
};

/// A screen ready to draw: egui texture plus its pixel size.
#[derive(Debug, Clone, Copy)]
pub struct ScreenView {
    pub texture: egui::TextureId,
    pub size: egui::Vec2,
}

/// The renderer and the textures showing its output.
pub struct Presenter {
    render_thread: RenderThread,
    screens: ScreenImages,
    upscale: UpscaleSetting,
    device: wgpu::Device,
    egui_renderer: std::sync::Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
    /// Registered texture, the generation it shows and the image size.
    textures: HashMap<Screen, (egui::TextureId, u64, egui::Vec2)>,
}

impl Presenter {
    /// Starts the render thread on eframe's device.
    ///
    /// # Errors
    /// Fails if eframe has no wgpu context or the thread cannot start.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self, String> {
        let Some(state) = &cc.wgpu_render_state else {
            return Err("no wgpu device available".into());
        };
        let info = state.adapter.get_info();
        log::info!("wgpu adapter: {} ({:?})", info.name, info.backend);
        let renderer = LatteRenderer::new(state.device.clone(), state.queue.clone());
        let upscale = renderer.upscale_setting();
        let render_thread = RenderThread::spawn(renderer).map_err(|e| e.to_string())?;
        Ok(Self {
            screens: render_thread.screens(),
            upscale,
            render_thread,
            device: state.device.clone(),
            egui_renderer: std::sync::Arc::clone(&state.renderer),
            textures: HashMap::new(),
        })
    }

    /// Applies from the next flip on.
    pub fn set_upscale(&self, config: UpscaleConfig) {
        self.upscale.set(config);
    }

    /// Where the emulator sends its work.
    pub fn sender(&self) -> Option<SyncSender<Vec<RenderCommand>>> {
        self.render_thread.sender()
    }

    /// Registers new screen images with egui.
    #[allow(clippy::cast_precision_loss)]
    pub fn sync(&mut self) {
        for screen in Screen::ALL {
            let Some(image) = self.screens.get(screen) else { continue };
            let size = egui::vec2(image.width as f32, image.height as f32);
            match self.textures.get(&screen) {
                Some((_, generation, _)) if *generation == image.generation => {}
                Some((id, _, _)) => {
                    let id = *id;
                    self.egui_renderer.write().update_egui_texture_from_wgpu_texture(
                        &self.device,
                        &image.view,
                        wgpu::FilterMode::Linear,
                        id,
                    );
                    self.textures.insert(screen, (id, image.generation, size));
                }
                None => {
                    let id = self.egui_renderer.write().register_native_texture(
                        &self.device,
                        &image.view,
                        wgpu::FilterMode::Linear,
                    );
                    self.textures.insert(screen, (id, image.generation, size));
                }
            }
        }
    }

    pub fn view(&self, screen: Screen) -> Option<ScreenView> {
        self.textures
            .get(&screen)
            .map(|(texture, _, size)| ScreenView { texture: *texture, size: *size })
    }

    pub const fn backend_name() -> &'static str {
        "wgpu"
    }
}
