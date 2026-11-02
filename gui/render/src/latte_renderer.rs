//! Executes Latte's [`RenderCommand`]s on wgpu.
//!
//! Guest surfaces live in host textures identified by [`SurfaceKey`]; vertex,
//! index and uniform bytes all go through one storage buffer (the *arena*)
//! that generated shaders read directly. Commands are recorded into one
//! encoder per flip; anything `queue.write_*` would reorder flushes it first.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex, PoisonError},
};

use egui_wgpu::wgpu;
use wgpu::util::DeviceExt as _;
use wiiu_gpu::{
    RenderBackend, RenderCommand, Screen,
    latte::ARENA_SIZE,
    render::{
        AddressMode, BindingKind, BoundTexture, DrawCommand, HostFormat, SamplerDesc, ShaderModule,
        SurfaceKey, TextureUpload, Topology,
    },
    shader::{TextureDim, TextureKind},
};

use crate::upscale::{UpscaleSetting, Upscaler};

/// Size the shader prelude reads through the dynamic binding.
const PARAMS_BINDING_SIZE: u64 = 288;

/// Address of the surfaces standing in for attachments of the wrong size.
const STAND_IN_ADDRESS: u32 = 0xFFFF_F000;

/// Features the renderer uses when the adapter offers them.
pub fn wanted_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    adapter.features()
        & (wgpu::Features::TEXTURE_COMPRESSION_BC
            | wgpu::Features::DEPTH32FLOAT_STENCIL8
            | wgpu::Features::FLOAT32_FILTERABLE
            | wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
            | wgpu::Features::RG11B10UFLOAT_RENDERABLE
            | wgpu::Features::DEPTH_CLIP_CONTROL
            // Latte renders to formats WebGPU keeps read-only, such as RGBA8 SNORM.
            | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES)
}

/// A screen image ready for presentation.
#[derive(Debug, Clone)]
pub struct ScreenImage {
    pub view: wgpu::TextureView,
    pub texture: wgpu::Texture,
    pub width: u32,
    pub height: u32,
    /// Changes whenever the texture object is replaced.
    pub generation: u64,
}

/// The latest image of each screen, readable from the UI thread.
#[derive(Debug, Default, Clone)]
pub struct ScreenImages(Arc<Mutex<HashMap<Screen, ScreenImage>>>);

impl ScreenImages {
    pub fn get(&self, screen: Screen) -> Option<ScreenImage> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).get(&screen).cloned()
    }

    pub(crate) fn set(&self, screen: Screen, image: ScreenImage) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).insert(screen, image);
    }
}

pub fn wgpu_format(format: HostFormat) -> wgpu::TextureFormat {
    use HostFormat as H;
    use wgpu::{AstcBlock as _Unused, TextureFormat as T};
    let _ = std::marker::PhantomData::<_Unused>;
    match format {
        H::R8Unorm => T::R8Unorm,
        H::R8Snorm => T::R8Snorm,
        H::R8Uint => T::R8Uint,
        H::R8Sint => T::R8Sint,
        H::Rg8Unorm => T::Rg8Unorm,
        H::Rg8Snorm => T::Rg8Snorm,
        H::Rg8Uint => T::Rg8Uint,
        H::Rg8Sint => T::Rg8Sint,
        H::Rgba8Unorm => T::Rgba8Unorm,
        H::Rgba8Srgb => T::Rgba8UnormSrgb,
        H::Rgba8Snorm => T::Rgba8Snorm,
        H::Rgba8Uint => T::Rgba8Uint,
        H::Rgba8Sint => T::Rgba8Sint,
        H::R16Uint => T::R16Uint,
        H::R16Sint => T::R16Sint,
        H::R16Float => T::R16Float,
        H::R16Unorm => T::R16Unorm,
        H::Rg16Uint => T::Rg16Uint,
        H::Rg16Sint => T::Rg16Sint,
        H::Rg16Float => T::Rg16Float,
        H::Rg16Unorm => T::Rg16Unorm,
        H::Rgba16Uint => T::Rgba16Uint,
        H::Rgba16Sint => T::Rgba16Sint,
        H::Rgba16Float => T::Rgba16Float,
        H::Rgba16Unorm => T::Rgba16Unorm,
        H::Rgba16Snorm => T::Rgba16Snorm,
        H::R32Uint => T::R32Uint,
        H::R32Sint => T::R32Sint,
        H::R32Float => T::R32Float,
        H::Rg32Uint => T::Rg32Uint,
        H::Rg32Sint => T::Rg32Sint,
        H::Rg32Float => T::Rg32Float,
        H::Rgba32Uint => T::Rgba32Uint,
        H::Rgba32Sint => T::Rgba32Sint,
        H::Rgba32Float => T::Rgba32Float,
        H::Rgb10a2Unorm => T::Rgb10a2Unorm,
        H::Rg11b10Float => T::Rg11b10Ufloat,
        H::Bc1Unorm => T::Bc1RgbaUnorm,
        H::Bc1Srgb => T::Bc1RgbaUnormSrgb,
        H::Bc2Unorm => T::Bc2RgbaUnorm,
        H::Bc2Srgb => T::Bc2RgbaUnormSrgb,
        H::Bc3Unorm => T::Bc3RgbaUnorm,
        H::Bc3Srgb => T::Bc3RgbaUnormSrgb,
        H::Bc4Unorm => T::Bc4RUnorm,
        H::Bc4Snorm => T::Bc4RSnorm,
        H::Bc5Unorm => T::Bc5RgUnorm,
        H::Bc5Snorm => T::Bc5RgSnorm,
        H::Depth16Unorm => T::Depth16Unorm,
        H::Depth24PlusStencil8 => T::Depth24PlusStencil8,
        H::Depth32Float => T::Depth32Float,
        H::Depth32FloatStencil8 => T::Depth32FloatStencil8,
    }
}

const fn compare_function(value: u8) -> wgpu::CompareFunction {
    use wgpu::CompareFunction as C;
    match value & 7 {
        0 => C::Never,
        1 => C::Less,
        2 => C::Equal,
        3 => C::LessEqual,
        4 => C::Greater,
        5 => C::NotEqual,
        6 => C::GreaterEqual,
        _ => C::Always,
    }
}

const fn stencil_op(value: u8) -> wgpu::StencilOperation {
    use wgpu::StencilOperation as S;
    match value & 7 {
        0 => S::Keep,
        1 => S::Zero,
        2 => S::Replace,
        3 => S::IncrementClamp,
        4 => S::DecrementClamp,
        5 => S::Invert,
        6 => S::IncrementWrap,
        _ => S::DecrementWrap,
    }
}

const fn blend_factor(value: u8) -> wgpu::BlendFactor {
    use wgpu::BlendFactor as B;
    match value {
        0x00 => B::Zero,
        0x01 => B::One,
        0x02 => B::Src,
        0x03 => B::OneMinusSrc,
        0x04 | 0x0B => B::SrcAlpha,
        0x05 | 0x0C => B::OneMinusSrcAlpha,
        0x06 => B::DstAlpha,
        0x07 => B::OneMinusDstAlpha,
        0x08 => B::Dst,
        0x09 => B::OneMinusDst,
        0x0A => B::SrcAlphaSaturated,
        0x0D | 0x13 => B::Constant,
        0x0E | 0x14 => B::OneMinusConstant,
        _ => B::One,
    }
}

const fn blend_operation(value: u8) -> wgpu::BlendOperation {
    use wgpu::BlendOperation as B;
    match value & 7 {
        0 => B::Add,
        1 => B::Subtract,
        2 => B::Min,
        3 => B::Max,
        _ => B::ReverseSubtract,
    }
}

const fn address_mode(mode: AddressMode) -> wgpu::AddressMode {
    match mode {
        AddressMode::Repeat => wgpu::AddressMode::Repeat,
        AddressMode::Mirror => wgpu::AddressMode::MirrorRepeat,
        AddressMode::Clamp => wgpu::AddressMode::ClampToEdge,
        AddressMode::Border => wgpu::AddressMode::ClampToEdge,
    }
}

fn view_dimension(dim: TextureDim) -> wgpu::TextureViewDimension {
    match dim {
        TextureDim::D2 => wgpu::TextureViewDimension::D2,
        TextureDim::D2Array => wgpu::TextureViewDimension::D2Array,
        TextureDim::D3 => wgpu::TextureViewDimension::D3,
        TextureDim::Cube => wgpu::TextureViewDimension::Cube,
    }
}

const BLIT_SHADER: &str = "
struct VsOut { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@vertex fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VsOut;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;
@fragment fn fs_main(input: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSample(source, source_sampler, input.uv).rgb, 1.0);
}
";

struct HostSurface {
    texture: wgpu::Texture,
    views: HashMap<(TextureDim, u32, u32, u32, u32), wgpu::TextureView>,
    target_views: HashMap<(u32, u32), wgpu::TextureView>,
    mip_count: u32,
    /// Bumped when the texture object changes, to invalidate bind groups.
    id: u64,
}

impl HostSurface {
    fn sample_view(&mut self, key: &SurfaceKey, bound: &BoundTexture) -> wgpu::TextureView {
        let cache =
            (bound.dim, bound.base_level, bound.level_count, bound.base_layer, bound.layer_count);
        let mip_count = self.mip_count;
        let texture = &self.texture;
        self.views
            .entry(cache)
            .or_insert_with(|| {
                let aspect = if key.format.is_depth() && key.format.has_stencil() {
                    wgpu::TextureAspect::DepthOnly
                } else {
                    wgpu::TextureAspect::All
                };
                let base_mip_level = bound.base_level.min(mip_count - 1);
                let is_array = matches!(bound.dim, TextureDim::D2Array | TextureDim::Cube);
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: None,
                    format: None,
                    dimension: Some(view_dimension(bound.dim)),
                    usage: None,
                    aspect,
                    base_mip_level,
                    mip_level_count: Some(bound.level_count.min(mip_count - base_mip_level).max(1)),
                    base_array_layer: if is_array {
                        bound.base_layer.min(key.layers.saturating_sub(1))
                    } else {
                        0
                    },
                    array_layer_count: match bound.dim {
                        TextureDim::Cube => Some(6),
                        TextureDim::D2Array => Some(bound.layer_count.min(key.layers).max(1)),
                        _ => None,
                    },
                })
            })
            .clone()
    }

    fn target_view(&mut self, level: u32, slice: u32) -> wgpu::TextureView {
        let texture = &self.texture;
        self.target_views
            .entry((level, slice))
            .or_insert_with(|| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_mip_level: level,
                    mip_level_count: Some(1),
                    base_array_layer: slice,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .clone()
    }
}

/// `(color attachments, depth attachment)` of the open render pass.
type PassKey = ([Option<(SurfaceKey, u32)>; 8], Option<(SurfaceKey, u32)>);

/// The wgpu renderer.
pub struct LatteRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    arena: wgpu::Buffer,
    arena_layout: wgpu::BindGroupLayout,
    arena_group: wgpu::BindGroup,
    surfaces: HashMap<SurfaceKey, HostSurface>,
    next_surface_id: u64,
    modules: HashMap<u64, wgpu::ShaderModule>,
    pipelines: HashMap<(u64, u64, u64), Option<Arc<wgpu::RenderPipeline>>>,
    resource_layouts: HashMap<Vec<(u32, BindingKind)>, wgpu::BindGroupLayout>,
    pipeline_layouts: HashMap<Vec<(u32, BindingKind)>, wgpu::PipelineLayout>,
    samplers: HashMap<SamplerDesc, wgpu::Sampler>,
    bind_groups: HashMap<u64, wgpu::BindGroup>,
    dummy_textures: HashMap<(TextureDim, TextureKind), wgpu::TextureView>,
    feedback_copies: HashMap<SurfaceKey, (wgpu::Texture, wgpu::TextureView)>,
    encoder: Option<wgpu::CommandEncoder>,
    pass: Option<wgpu::RenderPass<'static>>,
    pass_key: Option<PassKey>,
    blit_pipeline: wgpu::RenderPipeline,
    blit_layout: wgpu::BindGroupLayout,
    blit_sampler: wgpu::Sampler,
    pending_scan: HashMap<Screen, (SurfaceKey, u32)>,
    screen_targets: HashMap<Screen, ScreenImage>,
    screens: ScreenImages,
    generation: u64,
    upscaler: Upscaler,
    /// Screens with a new image since the last flip.
    presented: Vec<Screen>,
    logged: std::collections::HashSet<String>,
    /// Debugging aid: only the first draws of each frame execute (FLORA_MAX_DRAWS).
    max_draws: Option<u32>,
    frame_draws: u32,
    target_log: HashMap<SurfaceKey, u32>,
    pub draws_executed: u64,
    pub flips: u64,
}

impl std::fmt::Debug for LatteRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LatteRenderer")
            .field("draws", &self.draws_executed)
            .field("flips", &self.flips)
            .finish_non_exhaustive()
    }
}

impl LatteRenderer {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        device.on_uncaptured_error(Arc::new(|error| log::error!("wgpu: {error}")));
        let limit = device.limits().max_storage_buffer_binding_size;
        let arena_size = u64::from(ARENA_SIZE).min(limit);
        let arena = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("flora arena"),
            size: arena_size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::INDEX
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let visibility = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
        let arena_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flora arena layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(PARAMS_BINDING_SIZE),
                    },
                    count: None,
                },
            ],
        });
        let arena_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("flora arena group"),
            layout: &arena_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: arena.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &arena,
                        offset: 0,
                        size: wgpu::BufferSize::new(PARAMS_BINDING_SIZE),
                    }),
                },
            ],
        });

        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flora blit layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let blit_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("flora blit"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
        });
        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("flora blit"),
            bind_group_layouts: &[Some(&blit_layout)],
            immediate_size: 0,
        });
        let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("flora blit"),
            layout: Some(&blit_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &blit_module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &blit_module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::TextureFormat::Rgba8Unorm.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let blit_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            device,
            queue,
            arena,
            arena_layout,
            arena_group,
            surfaces: HashMap::new(),
            next_surface_id: 1,
            modules: HashMap::new(),
            pipelines: HashMap::new(),
            resource_layouts: HashMap::new(),
            pipeline_layouts: HashMap::new(),
            samplers: HashMap::new(),
            bind_groups: HashMap::new(),
            dummy_textures: HashMap::new(),
            feedback_copies: HashMap::new(),
            encoder: None,
            pass: None,
            pass_key: None,
            blit_pipeline,
            blit_layout,
            blit_sampler,
            pending_scan: HashMap::new(),
            screen_targets: HashMap::new(),
            screens: ScreenImages::default(),
            generation: 0,
            upscaler: Upscaler::default(),
            presented: Vec::new(),
            logged: std::collections::HashSet::new(),
            max_draws: std::env::var("FLORA_MAX_DRAWS").ok().and_then(|v| v.parse().ok()),
            frame_draws: 0,
            target_log: HashMap::new(),
            draws_executed: 0,
            flips: 0,
        }
    }

    pub fn screens(&self) -> ScreenImages {
        self.screens.clone()
    }

    /// Handle to change the screens' upscaling while the renderer runs.
    pub fn upscale_setting(&self) -> UpscaleSetting {
        self.upscaler.setting()
    }

    pub const fn device(&self) -> &wgpu::Device {
        &self.device
    }

    fn log_once(&mut self, message: String) {
        if self.logged.insert(message.clone()) {
            log::warn!("{message}");
        }
    }

    fn encoder(&mut self) -> &mut wgpu::CommandEncoder {
        self.encoder.get_or_insert_with(|| {
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("flora frame"),
            })
        })
    }

    fn end_pass(&mut self) {
        self.pass = None;
        self.pass_key = None;
    }

    /// Submits everything recorded so far.
    pub fn submit(&mut self) {
        self.end_pass();
        if let Some(encoder) = self.encoder.take() {
            self.queue.submit([encoder.finish()]);
        }
    }

    fn surface(&mut self, key: SurfaceKey, dim: TextureDim, mip_count: u32) -> &mut HostSurface {
        let stale = self.surfaces.get(&key).is_some_and(|s| s.mip_count < mip_count);
        if stale {
            self.surfaces.remove(&key);
        }
        let device = &self.device;
        let id = self.next_surface_id;
        let surface = self.surfaces.entry(key).or_insert_with(|| {
            let format = wgpu_format(key.format);
            let renderable = format.is_depth_stencil_format() || !format.is_compressed();
            let mut usage = wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC;
            if renderable && dim != TextureDim::D3 {
                usage |= wgpu::TextureUsages::RENDER_ATTACHMENT;
            }
            let (dimension, layers) = match dim {
                TextureDim::D3 => (wgpu::TextureDimension::D3, key.layers.max(1)),
                TextureDim::Cube => (wgpu::TextureDimension::D2, 6),
                TextureDim::D2Array => (wgpu::TextureDimension::D2, key.layers.max(1)),
                TextureDim::D2 => (wgpu::TextureDimension::D2, 1),
            };
            let (mut width, mut height) = (key.width.max(1), key.height.max(1));
            if format.is_compressed() {
                width = width.next_multiple_of(4);
                height = height.next_multiple_of(4);
            }
            let max_levels = (32 - width.max(height).leading_zeros()).max(1);
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("flora surface"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: layers },
                mip_level_count: mip_count.clamp(1, max_levels),
                sample_count: 1,
                dimension,
                format,
                usage,
                view_formats: &[],
            });
            HostSurface {
                texture,
                views: HashMap::new(),
                target_views: HashMap::new(),
                mip_count: mip_count.clamp(1, max_levels),
                id,
            }
        });
        if surface.id == id {
            self.next_surface_id += 1;
        }
        surface
    }

    fn upload(&mut self, upload: &TextureUpload) {
        // `write_texture` happens before recorded commands that use the old contents.
        self.submit();
        let format = wgpu_format(upload.key.format);
        let mip_count = upload.mips.len() as u32;
        let queue = self.queue.clone();
        let surface = self.surface(upload.key, upload.dim, mip_count);
        let texture = surface.texture.clone();
        let surface_id = surface.id;
        let block = format.block_dimensions();
        let block_bytes = format.block_copy_size(None).unwrap_or(4);
        for (level, mip) in upload.mips.iter().enumerate() {
            let level = level as u32;
            if level >= texture.mip_level_count() || mip.data.is_empty() {
                continue;
            }
            let blocks_x = mip.width.div_ceil(block.0);
            let blocks_y = mip.height.div_ceil(block.1);
            let extent_width = mip.width.next_multiple_of(block.0);
            let extent_height = mip.height.next_multiple_of(block.1);
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &mip.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(blocks_x * block_bytes),
                    rows_per_image: Some(blocks_y),
                },
                wgpu::Extent3d {
                    width: extent_width,
                    height: extent_height,
                    depth_or_array_layers: mip.layers.max(1),
                },
            );
        }
        let _ = surface_id;
    }

    fn clear(
        &mut self,
        color: Option<(SurfaceKey, u32, [f32; 4])>,
        depth: Option<wiiu_gpu::render::DepthClear>,
    ) {
        self.end_pass();
        let color_view = color.map(|(key, slice, _)| {
            self.surface(key, TextureDim::D2, 1)
                .target_view(0, slice.min(key.layers.saturating_sub(1)))
        });
        let depth_view = depth.as_ref().map(|d| {
            self.surface(d.surface, TextureDim::D2, 1)
                .target_view(0, d.slice.min(d.surface.layers.saturating_sub(1)))
        });
        let color_attachment =
            color.as_ref().zip(color_view.as_ref()).map(|((key, _, rgba), view)| {
                wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear_color(*key, *rgba)),
                        store: wgpu::StoreOp::Store,
                    },
                }
            });
        let depth_attachment = depth.as_ref().zip(depth_view.as_ref()).map(|(d, view)| {
            wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: Some(wgpu::Operations {
                    load: d.depth.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: d.surface.format.has_stencil().then_some(wgpu::Operations {
                    load: d.stencil.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                    store: wgpu::StoreOp::Store,
                }),
            }
        });
        if color_attachment.is_none() && depth_attachment.is_none() {
            return;
        }
        let encoder = self.encoder();
        drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("flora clear"),
            color_attachments: &[color_attachment],
            depth_stencil_attachment: depth_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }));
    }

    fn module(&mut self, shader: &ShaderModule) -> wgpu::ShaderModule {
        let device = &self.device;
        self.modules
            .entry(shader.id)
            .or_insert_with(|| {
                device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("flora shader"),
                    source: wgpu::ShaderSource::Wgsl(if std::env::var_os("FLORA_DEBUG_RED").is_some() { shader.wgsl.replace("return out;", "if (true) { out.c0 = vec4<f32>(1.0, 0.0, 0.0, 1.0); }\nreturn out;").into() } else { shader.wgsl.as_str().into() }),
                })
            })
            .clone()
    }

    fn resource_layout(&mut self, bindings: &[(u32, BindingKind)]) -> wgpu::BindGroupLayout {
        let device = &self.device;
        self.resource_layouts
            .entry(bindings.to_vec())
            .or_insert_with(|| {
                let entries: Vec<wgpu::BindGroupLayoutEntry> = bindings
                    .iter()
                    .map(|(binding, kind)| {
                        let visibility = if (*binding as usize) < 36 {
                            wgpu::ShaderStages::VERTEX
                        } else {
                            wgpu::ShaderStages::FRAGMENT
                        };
                        let ty = match kind {
                            BindingKind::Texture { dim, kind } => wgpu::BindingType::Texture {
                                sample_type: match kind {
                                    TextureKind::Float => {
                                        wgpu::TextureSampleType::Float { filterable: true }
                                    }
                                    TextureKind::Depth => wgpu::TextureSampleType::Depth,
                                    TextureKind::Uint => wgpu::TextureSampleType::Uint,
                                    TextureKind::Sint => wgpu::TextureSampleType::Sint,
                                },
                                view_dimension: view_dimension(*dim),
                                multisampled: false,
                            },
                            BindingKind::Sampler { comparison, filtering } => {
                                wgpu::BindingType::Sampler(if *comparison {
                                    wgpu::SamplerBindingType::Comparison
                                } else if *filtering {
                                    wgpu::SamplerBindingType::Filtering
                                } else {
                                    wgpu::SamplerBindingType::NonFiltering
                                })
                            }
                        };
                        wgpu::BindGroupLayoutEntry {
                            binding: *binding,
                            visibility,
                            ty,
                            count: None,
                        }
                    })
                    .collect();
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("flora resources"),
                    entries: &entries,
                })
            })
            .clone()
    }

    fn pipeline(
        &mut self,
        draw: &DrawCommand,
        bindings: &[(u32, BindingKind)],
    ) -> Option<Arc<wgpu::RenderPipeline>> {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        draw.state.hash(&mut hasher);
        let key = (draw.vertex.id, draw.pixel.id, hasher.finish());
        if let Some(existing) = self.pipelines.get(&key) {
            return existing.clone();
        }
        let vs = self.module(&draw.vertex);
        let fs = self.module(&draw.pixel);
        let resources = self.resource_layout(bindings);
        let device = self.device.clone();
        let arena_layout = self.arena_layout.clone();
        let layout = self
            .pipeline_layouts
            .entry(bindings.to_vec())
            .or_insert_with(|| {
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("flora pipeline"),
                    bind_group_layouts: &[Some(&arena_layout), Some(&resources)],
                    immediate_size: 0,
                })
            })
            .clone();
        let state = &draw.state;
        let targets: Vec<Option<wgpu::ColorTargetState>> = (0..8)
            .map(|i| {
                state.color_formats[i].map(|format| {
                    let wgpu_format = wgpu_format(format);
                    let integer = matches!(format.kind(), TextureKind::Uint | TextureKind::Sint);
                    let blend = state.blend[i];
                    wgpu::ColorTargetState {
                        format: wgpu_format,
                        blend: (blend.enabled && !integer).then_some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: blend_factor(blend.color_src),
                                dst_factor: blend_factor(blend.color_dst),
                                operation: blend_operation(blend.color_op),
                            },
                            alpha: wgpu::BlendComponent {
                                src_factor: blend_factor(blend.alpha_src),
                                dst_factor: blend_factor(blend.alpha_dst),
                                operation: blend_operation(blend.alpha_op),
                            },
                        }),
                        write_mask: wgpu::ColorWrites::from_bits_truncate(u32::from(
                            state.color_masks[i] & 0xF,
                        )),
                    }
                })
            })
            .collect();
        let last = targets.iter().rposition(Option::is_some).map_or(0, |i| i + 1);
        let depth_stencil = state.depth_format.map(|format| {
            let face = |f: wiiu_gpu::render::StencilFace| wgpu::StencilFaceState {
                compare: compare_function(f.compare),
                fail_op: stencil_op(f.fail),
                depth_fail_op: stencil_op(f.depth_fail),
                pass_op: stencil_op(f.pass),
            };
            let has_stencil = format.has_stencil() && state.stencil_test;
            wgpu::DepthStencilState {
                format: wgpu_format(format),
                depth_write_enabled: Some(state.depth_write),
                depth_compare: Some(if state.depth_test {
                    compare_function(state.depth_compare)
                } else {
                    wgpu::CompareFunction::Always
                }),
                stencil: if has_stencil {
                    wgpu::StencilState {
                        front: face(state.stencil_front),
                        back: face(state.stencil_back),
                        read_mask: u32::from(state.stencil_read_mask),
                        write_mask: u32::from(state.stencil_write_mask),
                    }
                } else {
                    wgpu::StencilState::default()
                },
                bias: wgpu::DepthBiasState {
                    constant: f32::from_bits(state.depth_bias[1]) as i32,
                    slope_scale: f32::from_bits(state.depth_bias[0]) / 16.0,
                    clamp: f32::from_bits(state.depth_bias[2]),
                },
            }
        });
        let topology = match state.topology {
            Topology::Points => wgpu::PrimitiveTopology::PointList,
            Topology::Lines => wgpu::PrimitiveTopology::LineList,
            Topology::LineStrip => wgpu::PrimitiveTopology::LineStrip,
            Topology::Triangles | Topology::Rects => wgpu::PrimitiveTopology::TriangleList,
            Topology::TriangleStrip => wgpu::PrimitiveTopology::TriangleStrip,
        };
        let strip = matches!(state.topology, Topology::LineStrip | Topology::TriangleStrip);
        let cull_mode = match (state.cull_front, state.cull_back) {
            (true, false) => Some(wgpu::Face::Front),
            (false, true) => Some(wgpu::Face::Back),
            _ => None,
        };
        let pipeline = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("flora pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &vs,
                entry_point: Some(if state.topology == Topology::Rects {
                    wiiu_gpu::shader::RECT_ENTRY_POINT
                } else {
                    "main"
                }),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology,
                strip_index_format: strip.then_some(wgpu::IndexFormat::Uint32),
                front_face: if state.front_face_cw {
                    wgpu::FrontFace::Cw
                } else {
                    wgpu::FrontFace::Ccw
                },
                cull_mode,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &fs,
                entry_point: Some("main"),
                compilation_options: Default::default(),
                targets: &targets[..last],
            }),
            multiview_mask: None,
            cache: None,
        });
        let pipeline = Some(Arc::new(pipeline));
        self.pipelines.insert(key, pipeline.clone());
        pipeline
    }

    fn sampler(&mut self, desc: SamplerDesc) -> wgpu::Sampler {
        let device = &self.device;
        self.samplers
            .entry(desc)
            .or_insert_with(|| {
                let filter = |linear: bool| {
                    if linear { wgpu::FilterMode::Linear } else { wgpu::FilterMode::Nearest }
                };
                device.create_sampler(&wgpu::SamplerDescriptor {
                    label: Some("flora sampler"),
                    address_mode_u: address_mode(desc.address[0]),
                    address_mode_v: address_mode(desc.address[1]),
                    address_mode_w: address_mode(desc.address[2]),
                    mag_filter: filter(desc.mag_linear),
                    min_filter: filter(desc.min_linear),
                    mipmap_filter: if desc.mip_linear {
                        wgpu::MipmapFilterMode::Linear
                    } else {
                        wgpu::MipmapFilterMode::Nearest
                    },
                    lod_min_clamp: f32::from(desc.min_lod) / 64.0,
                    lod_max_clamp: (f32::from(desc.max_lod) / 64.0)
                        .max(f32::from(desc.min_lod) / 64.0),
                    compare: desc.compare.map(compare_function),
                    anisotropy_clamp: 1,
                    border_color: None,
                    ..Default::default()
                })
            })
            .clone()
    }

    fn dummy_texture(&mut self, dim: TextureDim, kind: TextureKind) -> wgpu::TextureView {
        let device = self.device.clone();
        let queue = self.queue.clone();
        self.dummy_textures
            .entry((dim, kind))
            .or_insert_with(|| {
                let format = match kind {
                    TextureKind::Float => wgpu::TextureFormat::Rgba8Unorm,
                    TextureKind::Depth => wgpu::TextureFormat::Depth32Float,
                    TextureKind::Uint => wgpu::TextureFormat::Rgba8Uint,
                    TextureKind::Sint => wgpu::TextureFormat::Rgba8Sint,
                };
                let layers = if dim == TextureDim::Cube { 6 } else { 1 };
                let texture = device.create_texture_with_data(
                    &queue,
                    &wgpu::TextureDescriptor {
                        label: Some("flora dummy"),
                        size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: layers },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: if dim == TextureDim::D3 {
                            wgpu::TextureDimension::D3
                        } else {
                            wgpu::TextureDimension::D2
                        },
                        format,
                        usage: wgpu::TextureUsages::TEXTURE_BINDING
                            | wgpu::TextureUsages::COPY_DST
                            | wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[],
                    },
                    wgpu::util::TextureDataOrder::LayerMajor,
                    &vec![
                        0;
                        (format.block_copy_size(Some(wgpu::TextureAspect::All)).unwrap_or(4)
                            * layers) as usize
                    ],
                );
                texture.create_view(&wgpu::TextureViewDescriptor {
                    dimension: Some(view_dimension(dim)),
                    array_layer_count: if dim == TextureDim::Cube {
                        Some(6)
                    } else if dim == TextureDim::D2Array {
                        Some(1)
                    } else {
                        None
                    },
                    ..Default::default()
                })
            })
            .clone()
    }

    /// A surface sampled while it is also a target: the draw reads a copy made
    /// just before it, which is what the hardware's texture cache would see.
    fn feedback_copy(
        &mut self,
        bound: &BoundTexture,
        dim: TextureDim,
        kind: TextureKind,
    ) -> Option<wgpu::TextureView> {
        let source = self.surfaces.get(&bound.key)?.texture.clone();
        if dim != TextureDim::D2 || bound.key.format.kind() != kind {
            return None;
        }
        self.end_pass();
        let device = self.device.clone();
        let copy = self.feedback_copies.entry(bound.key).or_insert_with(|| {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("flora feedback copy"),
                size: source.size(),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: source.format(),
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let aspect = if bound.key.format.has_stencil() {
                wgpu::TextureAspect::DepthOnly
            } else {
                wgpu::TextureAspect::All
            };
            let view =
                texture.create_view(&wgpu::TextureViewDescriptor { aspect, ..Default::default() });
            (texture, view)
        });
        let (target, view) = copy.clone();
        let size = source.size();
        self.encoder().copy_texture_to_texture(
            source.as_image_copy(),
            target.as_image_copy(),
            wgpu::Extent3d { depth_or_array_layers: 1, ..size },
        );
        Some(view)
    }

    /// `group(1)` of a draw. Textures and samplers rarely change between draws.
    fn resource_group(
        &mut self,
        draw: &DrawCommand,
        bindings: &[(u32, BindingKind)],
    ) -> wgpu::BindGroup {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bindings.hash(&mut hasher);
        for bound in &draw.textures {
            bound.hash(&mut hasher);
            self.surfaces.get(&bound.key).map(|s| s.id).hash(&mut hasher);
        }
        let key = hasher.finish();
        // A draw sampling its own target reads a fresh copy every time.
        let feedback = draw.textures.iter().any(|t| {
            draw.color_targets
                .iter()
                .flatten()
                .chain(draw.depth_target.iter())
                .any(|(target, _)| *target == t.key)
        });
        if !feedback {
            if let Some(group) = self.bind_groups.get(&key) {
                return group.clone();
            }
        }
        let mut views: Vec<(u32, wgpu::TextureView)> = Vec::new();
        let mut samplers: Vec<(u32, wgpu::Sampler)> = Vec::new();
        for (binding, kind) in bindings {
            match kind {
                BindingKind::Texture { dim, kind } => {
                    let bound = draw.textures.iter().find(|t| t.binding == *binding).copied();
                    let in_use = bound.as_ref().is_some_and(|b| {
                        draw.color_targets
                            .iter()
                            .flatten()
                            .chain(draw.depth_target.iter())
                            .any(|(key, _)| *key == b.key)
                    });
                    let view = match bound {
                        Some(bound) if in_use => self.feedback_copy(&bound, *dim, *kind),
                        Some(bound)
                            if self.surfaces.contains_key(&bound.key)
                                && bound.key.format.kind() == *kind
                                && bound.dim == *dim =>
                        {
                            let key = bound.key;
                            self.surfaces.get_mut(&key).map(|s| s.sample_view(&key, &bound))
                        }
                        _ => None,
                    };
                    let view = match view {
                        Some(view) => view,
                        None => self.dummy_texture(*dim, *kind),
                    };
                    views.push((*binding, view));
                }
                BindingKind::Sampler { comparison, filtering } => {
                    let bound = draw.textures.iter().find(|t| t.binding + 1 == *binding);
                    let mut desc = bound.and_then(|t| t.sampler).unwrap_or_default();
                    if !*filtering {
                        desc.mag_linear = false;
                        desc.min_linear = false;
                        desc.mip_linear = false;
                    }
                    if *comparison && desc.compare.is_none() {
                        desc.compare = Some(1);
                    }
                    if !*comparison {
                        desc.compare = None;
                    }
                    samplers.push((*binding, self.sampler(desc)));
                }
            }
        }
        let mut entries: Vec<wgpu::BindGroupEntry<'_>> =
            Vec::with_capacity(views.len() + samplers.len());
        for (binding, view) in &views {
            entries.push(wgpu::BindGroupEntry {
                binding: *binding,
                resource: wgpu::BindingResource::TextureView(view),
            });
        }
        for (binding, sampler) in &samplers {
            entries.push(wgpu::BindGroupEntry {
                binding: *binding,
                resource: wgpu::BindingResource::Sampler(sampler),
            });
        }
        let layout = self
            .resource_layouts
            .get(bindings)
            .cloned()
            .unwrap_or_else(|| self.resource_layout(bindings));
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("flora resources"),
            layout: &layout,
            entries: &entries,
        });
        if !feedback {
            self.bind_groups.insert(key, group.clone());
        }
        group
    }

    fn open_pass(&mut self, draw: &DrawCommand) {
        let key: PassKey = (draw.color_targets, draw.depth_target);
        if self.pass_key == Some(key) && self.pass.is_some() {
            return;
        }
        self.end_pass();
        // Latte draws to attachments of different sizes (the smaller one bounds the
        // draw); the host insists on one size, the first color target's. A misfit gets
        // a throwaway surface of the right size.
        let extent =
            draw.color_targets.iter().flatten().next().map(|(key, _)| (key.width, key.height));
        let fitted = |key: SurfaceKey, slice: u32| match extent {
            Some((width, height)) if (key.width, key.height) != (width, height) => (
                SurfaceKey {
                    address: STAND_IN_ADDRESS,
                    width,
                    height,
                    layers: 1,
                    format: key.format,
                },
                0,
                true,
            ),
            _ => (key, slice.min(key.layers.saturating_sub(1)), false),
        };
        let mut color_views: Vec<Option<(wgpu::TextureView, bool)>> = Vec::with_capacity(8);
        for target in &draw.color_targets {
            color_views.push(target.map(|(key, slice)| {
                let (key, slice, stand_in) = fitted(key, slice);
                (self.surface(key, TextureDim::D2, 1).target_view(0, slice), stand_in)
            }));
        }
        let depth_view = draw.depth_target.map(|(key, slice)| {
            let (key, slice, stand_in) = fitted(key, slice);
            (self.surface(key, TextureDim::D2, 1).target_view(0, slice), stand_in)
        });
        let last = color_views.iter().rposition(Option::is_some).map_or(0, |i| i + 1);
        let attachments: Vec<Option<wgpu::RenderPassColorAttachment<'_>>> = color_views[..last]
            .iter()
            .map(|view| {
                view.as_ref().map(|(view, stand_in)| wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: if *stand_in {
                            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                        } else {
                            wgpu::LoadOp::Load
                        },
                        store: wgpu::StoreOp::Store,
                    },
                })
            })
            .collect();
        let depth_format = draw.depth_target.map(|(key, _)| key.format);
        let depth_attachment =
            depth_view.as_ref().map(|(view, stand_in)| wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: Some(wgpu::Operations {
                    load: if *stand_in { wgpu::LoadOp::Clear(1.0) } else { wgpu::LoadOp::Load },
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: depth_format.is_some_and(HostFormat::has_stencil).then_some(
                    wgpu::Operations {
                        load: if *stand_in { wgpu::LoadOp::Clear(0) } else { wgpu::LoadOp::Load },
                        store: wgpu::StoreOp::Store,
                    },
                ),
            });
        let encoder = self.encoder();
        let pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("flora draw"),
                color_attachments: &attachments,
                depth_stencil_attachment: depth_attachment,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            })
            .forget_lifetime();
        self.pass = Some(pass);
        self.pass_key = Some(key);
    }

    fn draw(&mut self, draw: &DrawCommand) {
        let mut bindings: Vec<(u32, BindingKind)> =
            draw.vertex.bindings.iter().chain(&draw.pixel.bindings).copied().collect();
        bindings.sort_by_key(|(binding, _)| *binding);
        let Some(pipeline) = self.pipeline(draw, &bindings) else { return };
        let group = self.resource_group(draw, &bindings);
        self.open_pass(draw);
        let extent = draw
            .color_targets
            .iter()
            .flatten()
            .map(|(key, _)| (key.width, key.height))
            .chain(draw.depth_target.map(|(key, _)| (key.width, key.height)))
            .fold((u32::MAX, u32::MAX), |acc, size| (acc.0.min(size.0), acc.1.min(size.1)));
        let Some(pass) = self.pass.as_mut() else { return };
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &self.arena_group, &[draw.params_offset]);
        pass.set_bind_group(1, &group, &[]);
        let [x, y, width, height, near, far] = draw.viewport;
        let (limit_w, limit_h) = (extent.0 as f32, extent.1 as f32);
        let vx = x.clamp(0.0, limit_w - 1.0);
        let vy = y.clamp(0.0, limit_h - 1.0);
        let vw = (width + x - vx).clamp(1.0, limit_w - vx);
        let vh = (height + y - vy).clamp(1.0, limit_h - vy);
        pass.set_viewport(vx, vy, vw, vh, near.min(far), far.max(near));
        let [sx, sy, sw, sh] = draw.scissor;
        let (sx, sy) = (sx.min(extent.0), sy.min(extent.1));
        pass.set_scissor_rect(sx, sy, sw.min(extent.0 - sx), sh.min(extent.1 - sy));
        pass.set_stencil_reference(u32::from(draw.stencil_reference));
        let [r, g, b, a] = draw.blend_constant;
        pass.set_blend_constant(wgpu::Color {
            r: f64::from(r),
            g: f64::from(g),
            b: f64::from(b),
            a: f64::from(a),
        });
        if let Some(offset) = draw.index_offset {
            pass.set_index_buffer(
                self.arena.slice(u64::from(offset)..u64::from(offset) + u64::from(draw.count) * 4),
                wgpu::IndexFormat::Uint32,
            );
            pass.draw_indexed(0..draw.count, draw.base_vertex, 0..draw.instances);
        } else {
            let first = draw.base_vertex.cast_unsigned();
            pass.draw(first..first + draw.count, 0..draw.instances);
        }
        self.draws_executed += 1;
        for (key, _) in draw.color_targets.iter().flatten() {
            *self.target_log.entry(*key).or_insert(0) += 1;
        }
    }

    fn copy_surface(
        &mut self,
        source: SurfaceKey,
        source_slice: u32,
        target: SurfaceKey,
        target_level: u32,
        target_slice: u32,
    ) {
        self.end_pass();
        if !self.surfaces.contains_key(&source) {
            return;
        }
        if wgpu_format(source.format).remove_srgb_suffix()
            != wgpu_format(target.format).remove_srgb_suffix()
        {
            self.log_once(format!(
                "copy between unrelated formats {:?} -> {:?}",
                source.format, target.format
            ));
            return;
        }
        let source_texture = self.surfaces.get(&source).map(|s| s.texture.clone());
        let target_texture = self.surface(target, TextureDim::D2, target_level + 1).texture.clone();
        let Some(source_texture) = source_texture else { return };
        let (width, height) = (
            source.width.min(target.width >> target_level).max(1),
            source.height.min(target.height >> target_level).max(1),
        );
        let aspect = wgpu::TextureAspect::All;
        let encoder = self.encoder();
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &source_texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: source_slice },
                aspect,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &target_texture,
                mip_level: target_level,
                origin: wgpu::Origin3d { x: 0, y: 0, z: target_slice },
                aspect,
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
    }

    fn present(&mut self, screen: Screen, source: SurfaceKey, slice: u32) {
        let Some(surface) = self.surfaces.get_mut(&source) else {
            self.log_once(format!("scan buffer copy from unknown surface {source:?}"));
            return;
        };
        if source.format.is_depth() || !matches!(source.format.kind(), TextureKind::Float) {
            return;
        }
        let source_view = surface.target_view(0, slice.min(source.layers.saturating_sub(1)));
        let (width, height) = (source.width, source.height);
        let stale =
            self.screen_targets.get(&screen).is_none_or(|t| t.width != width || t.height != height);
        if stale {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(match screen {
                    Screen::Tv => "flora TV",
                    Screen::Drc => "flora GamePad",
                }),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.generation += 1;
            let image = ScreenImage { view, texture, width, height, generation: self.generation };
            self.screen_targets.insert(screen, image);
        }
        let Some(target) = self.screen_targets.get(&screen).cloned() else { return };
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("flora blit"),
            layout: &self.blit_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.blit_sampler),
                },
            ],
        });
        let pipeline = self.blit_pipeline.clone();
        let encoder = self.encoder();
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("flora present"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        self.presented.push(screen);
        if !self.upscaler.shows(screen) {
            self.screens.set(screen, target);
        }
    }

    fn flip(&mut self) {
        self.end_pass();
        for (screen, (key, slice)) in std::mem::take(&mut self.pending_scan) {
            self.present(screen, key, slice);
        }
        self.submit();
        self.upscaler.update(
            &self.device,
            &self.queue,
            &self.presented,
            &self.screen_targets,
            &self.screens,
            &mut self.generation,
        );
        self.presented.clear();
        self.flips += 1;
        self.frame_draws = 0;
        // Bind groups keep textures alive; the set is rebuilt cheaply.
        if self.bind_groups.len() > 8192 {
            self.bind_groups.clear();
        }
    }

    fn clear_screen(&mut self, screen: Screen, color: [f32; 4]) {
        let (width, height) = screen.default_size();
        let bytes: Vec<u8> = color
            .map(|c| (c.clamp(0.0, 1.0) * 255.0) as u8)
            .iter()
            .copied()
            .cycle()
            .take((width * height * 4) as usize)
            .collect();
        let texture = self.device.create_texture_with_data(
            &self.queue,
            &wgpu::TextureDescriptor {
                label: Some("flora blank screen"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_DST
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &bytes,
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.generation += 1;
        let image = ScreenImage { view, texture, width, height, generation: self.generation };
        self.screen_targets.insert(screen, image.clone());
        self.screens.set(screen, image);
    }

    /// Render targets drawn since the last call, with their draw counts (tools).
    pub fn take_target_log(&mut self) -> Vec<(SurfaceKey, u32)> {
        let mut list: Vec<_> = std::mem::take(&mut self.target_log).into_iter().collect();
        list.sort_by_key(|(key, _)| key.address);
        list
    }

    /// Reads a surface back as RGBA8 when its format has a simple 8-bit layout (tools).
    pub fn read_surface(&mut self, key: SurfaceKey) -> Option<(u32, u32, Vec<u8>)> {
        self.submit();
        let format = wgpu_format(key.format);
        if !matches!(format, wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb)
        {
            return None;
        }
        let texture = self.surfaces.get(&key)?.texture.clone();
        self.read_texture(&texture, key.width, key.height)
    }

    fn read_texture(
        &self,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Option<(u32, u32, Vec<u8>)> {
        let row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("flora readback"),
            size: u64::from(row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("flora readback"),
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let data = slice.get_mapped_range().ok()?;
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for line in data.chunks_exact(row as usize) {
            out.extend_from_slice(&line[..(width * 4) as usize]);
        }
        drop(data);
        buffer.unmap();
        Some((width, height, out))
    }

    /// Reads a screen back as RGBA8 (tools and tests).
    pub fn read_screen(&mut self, screen: Screen) -> Option<(u32, u32, Vec<u8>)> {
        self.submit();
        let image = self.screens.get(screen)?;
        self.read_texture(&image.texture, image.width, image.height)
    }
}

fn clear_color(key: SurfaceKey, rgba: [f32; 4]) -> wgpu::Color {
    let _ = key;
    wgpu::Color {
        r: f64::from(rgba[0]),
        g: f64::from(rgba[1]),
        b: f64::from(rgba[2]),
        a: f64::from(rgba[3]),
    }
}

impl RenderBackend for LatteRenderer {
    fn execute(&mut self, commands: &[RenderCommand]) {
        for command in commands {
            match command {
                RenderCommand::ClearScreen { screen, color } => self.clear_screen(*screen, *color),
                RenderCommand::ArenaWrite { offset, bytes } => {
                    if u64::from(*offset) + bytes.len() as u64 <= self.arena.size() {
                        self.queue.write_buffer(&self.arena, u64::from(*offset), bytes);
                    }
                }
                RenderCommand::ArenaWrap => self.submit(),
                RenderCommand::Upload(upload) => self.upload(upload),
                RenderCommand::Clear { color, depth } => self.clear(*color, *depth),
                RenderCommand::Draw(draw) => {
                    self.frame_draws += 1;
                    if self.max_draws.is_none_or(|limit| self.frame_draws <= limit) {
                        self.draw(draw);
                    }
                }
                RenderCommand::CopySurface {
                    source,
                    source_slice,
                    target,
                    target_level,
                    target_slice,
                    ..
                } => {
                    self.copy_surface(
                        *source,
                        *source_slice,
                        *target,
                        *target_level,
                        *target_slice,
                    );
                }
                RenderCommand::CopyToScanBuffer { surface, slice, screen } => {
                    self.pending_scan.insert(*screen, (*surface, *slice));
                }
                RenderCommand::Flip => self.flip(),
            }
        }
    }
}

/// Runs the renderer on its own thread, so the guest does not wait for recording.
#[derive(Debug)]
pub struct RenderThread {
    sender: Option<std::sync::mpsc::SyncSender<Vec<RenderCommand>>>,
    thread: Option<std::thread::JoinHandle<()>>,
    screens: ScreenImages,
}

/// Frames of commands the emulator may be ahead of the renderer.
const RENDER_QUEUE_LENGTH: usize = 3;

impl RenderThread {
    /// # Errors
    /// Fails if the thread cannot be spawned.
    pub fn spawn(mut renderer: LatteRenderer) -> std::io::Result<Self> {
        let screens = renderer.screens();
        let (sender, receiver) =
            std::sync::mpsc::sync_channel::<Vec<RenderCommand>>(RENDER_QUEUE_LENGTH);
        let thread = std::thread::Builder::new().name("flora-render".into()).spawn(move || {
            while let Ok(commands) = receiver.recv() {
                renderer.execute(&commands);
            }
        })?;
        Ok(Self { sender: Some(sender), thread: Some(thread), screens })
    }

    pub fn screens(&self) -> ScreenImages {
        self.screens.clone()
    }

    /// A clonable handle for the emulation thread.
    pub fn sender(&self) -> Option<std::sync::mpsc::SyncSender<Vec<RenderCommand>>> {
        self.sender.clone()
    }
}

impl Drop for RenderThread {
    fn drop(&mut self) {
        self.sender = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Requests the device the renderer wants from `adapter`.
///
/// # Errors
/// Fails if the adapter refuses.
pub async fn request_device(
    adapter: &wgpu::Adapter,
) -> Result<(wgpu::Device, wgpu::Queue), wgpu::RequestDeviceError> {
    adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("flora"),
            required_features: wanted_features(adapter),
            required_limits: adapter.limits(),
            ..Default::default()
        })
        .await
}
