//! xBRZ post-processing of the finished screen images.
//!
//! ```text
//!  screen target ──copy──► readback buffer ──map──► worker thread (xBRZ bands)
//!                                                        │
//!  screen shown by the UI ◄── upscaled texture ◄─────────┘
//! ```
//!
//! xBRZ runs on the CPU, so nothing ever waits for it: a screen keeps one frame
//! in the pipeline and flips arriving meanwhile are not upscaled. The guest and
//! the renderer run at full speed; a slow host only sees the upscaled image
//! update less often.
#![allow(clippy::cast_possible_truncation)]

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
};

use egui_wgpu::wgpu;
use wiiu_gpu::Screen;

use crate::{ScreenImage, ScreenImages};

/// Largest factor one xBRZ pass supports.
pub const MAX_FACTOR: u8 = 6;
/// Rows of real neighbours each band sees above and below: xBRZ looks two rows
/// away, more keeps the seams identical to a whole-image pass.
#[cfg(feature = "xbrz")]
const BAND_OVERLAP: usize = 4;

/// Which algorithm upscales the screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpscaleMethod {
    #[default]
    None,
    Xbrz,
}

/// Persisted upscaling settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct UpscaleConfig {
    pub method: UpscaleMethod,
    /// Output size relative to the guest's (2..=6).
    pub factor: u8,
}

impl Default for UpscaleConfig {
    fn default() -> Self {
        Self { method: UpscaleMethod::None, factor: 2 }
    }
}

impl UpscaleConfig {
    /// `1` when the screens stay as they are.
    pub const fn effective_factor(self) -> u8 {
        if !available() {
            return 1;
        }
        match self.method {
            UpscaleMethod::None => 1,
            UpscaleMethod::Xbrz => {
                if self.factor < 2 {
                    1
                } else if self.factor > MAX_FACTOR {
                    MAX_FACTOR
                } else {
                    self.factor
                }
            }
        }
    }
}

/// Whether this build contains xBRZ (GPL-3.0-only, behind the `xbrz` feature).
pub const fn available() -> bool {
    cfg!(feature = "xbrz")
}

/// The factor in effect, changeable from any thread while the renderer runs.
#[derive(Debug, Clone)]
pub struct UpscaleSetting(Arc<AtomicU8>);

impl Default for UpscaleSetting {
    fn default() -> Self {
        Self(Arc::new(AtomicU8::new(1)))
    }
}

impl UpscaleSetting {
    pub fn set(&self, config: UpscaleConfig) {
        self.0.store(config.effective_factor(), Ordering::Relaxed);
    }

    fn factor(&self) -> u8 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Upscales tightly packed RGBA8 in parallel bands; the result is opaque.
#[cfg(feature = "xbrz")]
fn scale_image(rgba: &[u8], width: usize, height: usize, factor: usize) -> Vec<u8> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).max(1);
    let rows = height.div_ceil(threads).max(1);
    let row_bytes = width * factor * 4;
    let band = |y0: usize| {
        let y1 = (y0 + rows).min(height);
        let (p0, p1) = (y0.saturating_sub(BAND_OVERLAP), (y1 + BAND_OVERLAP).min(height));
        let scaled =
            xbrz::scale_rgba(&rgba[p0 * width * 4..p1 * width * 4], width, p1 - p0, factor);
        scaled[(y0 - p0) * factor * row_bytes..(y1 - p0) * factor * row_bytes].to_vec()
    };
    let mut out = std::thread::scope(|scope| {
        let bands: Vec<_> = (0..height)
            .step_by(rows)
            .map(|y0| {
                let band = &band;
                scope.spawn(move || band(y0))
            })
            .collect();
        bands.into_iter().filter_map(|b| b.join().ok()).collect::<Vec<_>>().concat()
    });
    // Blending at the image border leaves partly transparent pixels behind.
    for pixel in out.as_chunks_mut::<4>().0 {
        pixel[3] = 0xFF;
    }
    out
}

#[cfg(not(feature = "xbrz"))]
fn scale_image(rgba: &[u8], _width: usize, _height: usize, _factor: usize) -> Vec<u8> {
    rgba.to_vec()
}

struct Job {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    factor: u8,
}

struct Scaled {
    screen: Screen,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    factor: u8,
}

/// Where a screen's frame is in the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Idle,
    /// The readback buffer is being mapped.
    Mapping {
        width: u32,
        height: u32,
        row: u32,
        factor: u8,
    },
    /// A worker has the frame.
    Scaling,
}

struct ScreenState {
    stage: Stage,
    readback: Option<(wgpu::Buffer, u64)>,
    mapped: Option<Receiver<bool>>,
    jobs: Sender<Job>,
    /// The upscaled image being shown.
    image: Option<ScreenImage>,
}

/// xBRZ for the screens of one renderer.
pub struct Upscaler {
    setting: UpscaleSetting,
    screens: HashMap<Screen, ScreenState>,
    results: Receiver<Scaled>,
    results_sender: Sender<Scaled>,
}

impl std::fmt::Debug for Upscaler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upscaler").field("setting", &self.setting).finish_non_exhaustive()
    }
}

impl Default for Upscaler {
    fn default() -> Self {
        let (results_sender, results) = mpsc::channel();
        Self {
            setting: UpscaleSetting::default(),
            screens: HashMap::new(),
            results,
            results_sender,
        }
    }
}

fn spawn_worker(screen: Screen, results: Sender<Scaled>) -> Sender<Job> {
    let (jobs, receiver) = mpsc::channel::<Job>();
    let spawned = std::thread::Builder::new().name("flora-xbrz".into()).spawn(move || {
        while let Ok(job) = receiver.recv() {
            let Job { rgba, width, height, factor } = job;
            let rgba = scale_image(&rgba, width as usize, height as usize, usize::from(factor));
            let (width, height) = (width * u32::from(factor), height * u32::from(factor));
            if results.send(Scaled { screen, rgba, width, height, factor }).is_err() {
                break;
            }
        }
    });
    if let Err(error) = spawned {
        log::error!("cannot start the xBRZ thread: {error}");
    }
    jobs
}

impl Upscaler {
    pub fn setting(&self) -> UpscaleSetting {
        self.setting.clone()
    }

    /// `true` while the UI should see the upscaled image of `screen`, not the native one.
    pub fn shows(&self, screen: Screen) -> bool {
        self.setting.factor() > 1 && self.screens.get(&screen).is_some_and(|s| s.image.is_some())
    }

    /// Moves frames along the pipeline. Call after a flip's work was submitted;
    /// `presented` lists the screens that have a new image.
    pub fn update(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        presented: &[Screen],
        targets: &HashMap<Screen, ScreenImage>,
        shown: &ScreenImages,
        generation: &mut u64,
    ) {
        let factor = self.setting.factor();
        if factor <= 1 {
            self.switch_off(targets, shown);
            return;
        }
        let _ = device.poll(wgpu::PollType::Poll);
        for state in self.screens.values_mut() {
            Self::hand_over(state);
        }
        while let Ok(scaled) = self.results.try_recv() {
            if let Some(state) = self.screens.get_mut(&scaled.screen) {
                state.stage = Stage::Idle;
                if scaled.factor == factor {
                    Self::show(device, queue, state, &scaled, shown, generation);
                }
            }
        }
        for screen in presented {
            if let Some(target) = targets.get(screen) {
                self.capture(device, queue, *screen, target, factor);
            }
        }
    }

    /// Back to the native images as soon as the setting says so.
    fn switch_off(&mut self, targets: &HashMap<Screen, ScreenImage>, shown: &ScreenImages) {
        for (screen, state) in &mut self.screens {
            if state.image.take().is_some() {
                if let Some(target) = targets.get(screen) {
                    shown.set(*screen, target.clone());
                }
            }
        }
        while self.results.try_recv().is_ok() {}
        for state in self.screens.values_mut() {
            if state.stage == Stage::Scaling {
                state.stage = Stage::Idle;
            }
        }
    }

    /// A finished readback goes to the worker.
    fn hand_over(state: &mut ScreenState) {
        let Stage::Mapping { width, height, row, factor } = state.stage else { return };
        let Some(mapped) = &state.mapped else { return };
        let success = match mapped.try_recv() {
            Ok(success) => success,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => false,
        };
        state.mapped = None;
        state.stage = Stage::Idle;
        let Some((buffer, _)) = &state.readback else { return };
        if !success {
            return;
        }
        let tight = (width * 4) as usize;
        let rgba = {
            let Ok(data) = buffer.slice(..).get_mapped_range() else { return };
            let mut rgba = Vec::with_capacity(tight * height as usize);
            for line in data.chunks_exact(row as usize) {
                rgba.extend_from_slice(&line[..tight]);
            }
            rgba
        };
        buffer.unmap();
        if state.jobs.send(Job { rgba, width, height, factor }).is_ok() {
            state.stage = Stage::Scaling;
        }
    }

    fn show(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        state: &mut ScreenState,
        scaled: &Scaled,
        shown: &ScreenImages,
        generation: &mut u64,
    ) {
        let (width, height) = (scaled.width, scaled.height);
        let stale = state.image.as_ref().is_none_or(|i| i.width != width || i.height != height);
        if stale {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("flora upscaled screen"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            *generation += 1;
            state.image =
                Some(ScreenImage { view, texture, width, height, generation: *generation });
        }
        let Some(image) = &state.image else { return };
        queue.write_texture(
            image.texture.as_image_copy(),
            &scaled.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        if stale {
            shown.set(scaled.screen, image.clone());
        }
    }

    /// Starts reading `target` back unless the screen still has a frame in the pipeline.
    fn capture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: Screen,
        target: &ScreenImage,
        factor: u8,
    ) {
        let results = &self.results_sender;
        let state = self.screens.entry(screen).or_insert_with(|| ScreenState {
            stage: Stage::Idle,
            readback: None,
            mapped: None,
            jobs: spawn_worker(screen, results.clone()),
            image: None,
        });
        if state.stage != Stage::Idle {
            return;
        }
        let (width, height) = (target.width, target.height);
        let row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let size = u64::from(row) * u64::from(height);
        if state.readback.as_ref().is_none_or(|(_, existing)| *existing != size) {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("flora upscale readback"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            state.readback = Some((buffer, size));
        }
        let Some((buffer, _)) = &state.readback else { return };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("flora upscale readback"),
        });
        encoder.copy_texture_to_buffer(
            target.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        queue.submit([encoder.finish()]);
        let (sender, receiver) = mpsc::channel();
        buffer.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result.is_ok());
        });
        state.mapped = Some(receiver);
        state.stage = Stage::Mapping { width, height, row, factor };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factors_stay_in_the_range_of_one_pass() {
        let config = |method, factor| UpscaleConfig { method, factor }.effective_factor();
        assert_eq!(config(UpscaleMethod::None, 4), 1);
        if available() {
            assert_eq!(config(UpscaleMethod::Xbrz, 0), 1);
            assert_eq!(config(UpscaleMethod::Xbrz, 3), 3);
            assert_eq!(config(UpscaleMethod::Xbrz, 200), MAX_FACTOR);
        }
    }

    #[cfg(feature = "xbrz")]
    #[test]
    fn bands_match_a_whole_image_pass() {
        let (width, height) = (97, 61);
        let rgba: Vec<u8> = (0..width * height)
            .flat_map(|i| {
                let v = (i * i % 251) as u8;
                [v, v.wrapping_mul(3), 255 - v, 255]
            })
            .collect();
        let mut whole = xbrz::scale_rgba(&rgba, width, height, 2);
        for pixel in whole.as_chunks_mut::<4>().0 {
            pixel[3] = 0xFF;
        }
        assert!(scale_image(&rgba, width, height, 2) == whole);
    }
}
