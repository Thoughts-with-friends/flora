//! Boots a title without a window and reports what happened.
//!
//! ```text
//! cargo run --release -p shocora_tests --example headless -- <TITLE_DIR | FILE.rpx> [FRAMES]
//! ```
//!
//! Environment:
//! * `SHOCORA_INTERP`: use the interpreter (precise faults) instead of the JIT.
//! * `SHOCORA_DISASM="addr,count;addr,count"`: extra code listings at the end.
//! * `SHOCORA_SHOTS="dir,every"`: render on the GPU and save the TV/GamePad image
//!   every `every` flips. Files stay outside the repository unless you say otherwise.
//! * `SHOCORA_PRESS="frame,button,frames;..."`: scripted GamePad input.
//! * `SHOCORA_UNPACED`: do not wait for real time between frames.
//! * `SHOCORA_PROFILE`: flips per second and where the host time goes. `SHOCORA_PROFILE=flips`
//!   leaves out the timers around every HLE call, which cost time themselves.
//! * `SHOCORA_XBRZ=factor`: upscale the screens (with `SHOCORA_SHOTS`).
//! * `SHOCORA_RENDER_THREAD`: render on a thread of its own, as the GUI does (no screenshots).
//! * `SHOCORA_SWAP_INTERVAL=n`: vertical blanks per flip, whatever the title asks for.
//! * `SHOCORA_BACKEND=auto|vulkan|dx12|gl`: host API; `auto` chooses like the GUI (default `vulkan`).
//! * `SHOCORA_WAV=file`: saves what the TV plays. Stays outside the repository unless you say otherwise.
//! * `SHOCORA_CAPTURE="flip,dir,min_pixels"` (with `SHOCORA_SHOTS`): one frame draw by draw, then exit.
//! * `SHOCORA_TRACE_UBO="first_flip,last_flip,file"`: uniform blocks of every draw, then exit.
//! * `SHOCORA_FRAMEGEN`: generates frames; with `SHOCORA_SHOTS`, also saves them as `Gen*`.
//! * `SHOCORA_RESET_AT=frame`: reboots the title at that frame, like the GUI's Reset.
//! * `SHOCORA_TOUCH="frame,x,y,frames"`: touches the GamePad's screen (854 x 480 pixels).
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use shocora_render::{
    LatteRenderer, RenderBackend, RenderCommand, Screen, UpscaleConfig, UpscaleMethod,
    request_device, wgpu,
};
use wiiu_emu::{Config, Emulator, GamePadState, Status};

/// One scripted button press.
#[derive(Debug, Clone, Copy)]
struct Press {
    frame: u64,
    button: u32,
    frames: u64,
}

fn parse_presses(text: &str) -> Vec<Press> {
    text.split(';')
        .filter_map(|item| {
            let mut parts = item.split(',');
            let frame = parts.next()?.trim().parse().ok()?;
            let button =
                u32::from_str_radix(parts.next()?.trim().trim_start_matches("0x"), 16).ok()?;
            let frames = parts.next()?.trim().parse().ok()?;
            Some(Press { frame, button, frames })
        })
        .collect()
}

fn create_renderer() -> Result<LatteRenderer, Box<dyn std::error::Error>> {
    let backends = match std::env::var("SHOCORA_BACKEND").as_deref() {
        Ok("auto") => wgpu::Backends::PRIMARY | wgpu::Backends::GL,
        Ok("dx12") => wgpu::Backends::DX12,
        Ok("gl") => wgpu::Backends::GL,
        _ => wgpu::Backends::VULKAN,
    };
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
        ..Default::default()
    }))?;
    println!("adapter: {} ({:?})", adapter.get_info().name, adapter.get_info().backend);
    let (device, queue) = pollster::block_on(request_device(&adapter))?;
    let mut renderer = LatteRenderer::new(device, queue);
    renderer.use_pipeline_cache(Path::new("shader_cache"), &adapter.get_info());
    renderer.frame_gen_setting().set(std::env::var_os("SHOCORA_FRAMEGEN").is_some());
    Ok(renderer)
}

fn save_png(
    path: &Path,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba)?;
    Ok(())
}

/// Writes interleaved stereo samples as a 16-bit WAV file.
fn save_wav(path: &Path, samples: &[i16]) -> Result<(), Box<dyn std::error::Error>> {
    let data = u32::try_from(samples.len() * 2)?;
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&48_000_u32.to_le_bytes());
    bytes.extend_from_slice(&192_000_u32.to_le_bytes());
    bytes.extend_from_slice(&4_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data.to_le_bytes());
    bytes.extend(samples.iter().flat_map(|sample| sample.to_le_bytes()));
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Runs one frame draw by draw: logs each draw and how many pixels of its first color target it
/// changed, saves the target after draws changing at least `min_pixels`, and writes the shaders.
fn capture_frame(
    renderer: &mut LatteRenderer,
    commands: &[RenderCommand],
    dir: &Path,
    min_pixels: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::{collections::HashMap, fmt::Write as _};
    std::fs::create_dir_all(dir)?;
    let mut last: HashMap<_, Vec<u8>> = HashMap::new();
    let mut log = String::new();
    for (index, command) in commands.iter().enumerate() {
        // A surface as the frame's first readers see it.
        if let RenderCommand::Draw(draw) = command {
            for texture in &draw.textures {
                if let Some((width, height, rgba)) = renderer.read_surface(texture.key) {
                    let name = format!(
                        "tex{index:05}_{:08x}_{width}x{height}_{:?}.png",
                        texture.key.address, texture.key.format
                    );
                    save_png(&dir.join(name), width, height, &rgba)?;
                }
            }
        }
        // What a target holds before the frame's first draw to it.
        if let RenderCommand::Draw(draw) = command {
            if let Some((key, _)) = draw.color_targets.iter().flatten().next() {
                if !last.contains_key(key) {
                    if let Some((_, _, rgba)) = renderer.read_surface(*key) {
                        last.insert(*key, rgba);
                    }
                }
            }
        }
        renderer.execute(std::slice::from_ref(command));
        let RenderCommand::Draw(draw) = command else {
            if !matches!(command, RenderCommand::ArenaWrite { .. } | RenderCommand::Upload(_)) {
                writeln!(log, "{index}: {command:?}")?;
            }
            continue;
        };
        for module in [&draw.vertex, &draw.pixel] {
            std::fs::write(dir.join(format!("shader_{}.wgsl", module.id)), &module.wgsl)?;
        }
        let target = draw.color_targets.iter().flatten().next().map(|(key, _)| *key);
        let mut changed = None;
        if let Some((key, (width, height, rgba))) =
            target.and_then(|key| renderer.read_surface(key).map(|image| (key, image)))
        {
            let before = last.get(&key);
            let count = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .filter(|(pixel, value)| {
                    before.is_none_or(|b| b[pixel * 4..pixel * 4 + 4] != **value)
                })
                .count() as u32;
            changed = Some(count);
            if count >= min_pixels {
                let name = format!("d{index:05}_{:08x}.png", key.address);
                save_png(&dir.join(name), width, height, &rgba)?;
            }
            last.insert(key, rgba);
        }
        writeln!(
            log,
            "{index}: draw vs{} ps{} n{} i{} {:?} changed {changed:?} target {target:?} depth {:?} \
             masks {:?} blend {:?} test {} write {} cmp {} cull {}/{} cw {} vp {:?} sc {:?}\n  tex {:?}",
            draw.vertex.id,
            draw.pixel.id,
            draw.count,
            draw.instances,
            draw.state.topology,
            draw.depth_target,
            &draw.state.color_masks[..2],
            draw.state.blend[0],
            draw.state.depth_test,
            draw.state.depth_write,
            draw.state.depth_compare,
            draw.state.cull_front,
            draw.state.cull_back,
            draw.state.front_face_cw,
            draw.viewport,
            draw.scissor,
            draw.textures
                .iter()
                .map(|t| (t.binding, t.key, t.base_level, t.level_count, t.sampler))
                .collect::<Vec<_>>(),
        )?;
    }
    std::fs::write(dir.join("draws.txt"), log)?;
    renderer.submit();
    for screen in Screen::ALL {
        if let Some((width, height, rgba)) = renderer.read_screen(screen) {
            save_png(&dir.join(format!("{screen:?}.png")), width, height, &rgba)?;
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(args.next().ok_or("usage: headless <TITLE_DIR | FILE.rpx> [FRAMES]")?);
    let frames: u64 = args.next().and_then(|f| f.to_str()?.parse().ok()).unwrap_or(60);

    let config = Config {
        jit: std::env::var_os("SHOCORA_INTERP").is_none(),
        paced: std::env::var_os("SHOCORA_UNPACED").is_none(),
        swap_interval: std::env::var("SHOCORA_SWAP_INTERVAL").ok().and_then(|n| n.parse().ok()),
        profile: std::env::var("SHOCORA_PROFILE").is_ok_and(|value| value != "flips"),
        ..Config::default()
    };
    let mut emu = Emulator::new(config)?;
    emu.load_title(&path)?;
    if let Some(title) = emu.title() {
        println!("title: {} ({:016x?})", title.display_name(), title.meta.title_id);
        println!("rpx:   {}", title.rpx_path.display());
    }

    let shots = std::env::var("SHOCORA_SHOTS").ok();
    let (shot_dir, shot_every) = shots
        .as_deref()
        .and_then(|s| s.split_once(','))
        .map_or((None, 60), |(dir, every)| (Some(PathBuf::from(dir)), every.parse().unwrap_or(60)));
    let mut renderer = shot_dir.as_ref().map(|_| create_renderer()).transpose()?;
    if let (Some(renderer), Some(factor)) = (&renderer, std::env::var("SHOCORA_XBRZ").ok()) {
        let factor = factor.parse().unwrap_or(2);
        renderer.upscale_setting().set(UpscaleConfig { method: UpscaleMethod::Xbrz, factor });
    }
    if let Some(dir) = &shot_dir {
        std::fs::create_dir_all(dir)?;
    }
    let (render_thread, upscale) = match std::env::var_os("SHOCORA_RENDER_THREAD") {
        Some(_) => {
            let renderer = create_renderer()?;
            let upscale = renderer.upscale_setting();
            if let Some(factor) = std::env::var("SHOCORA_XBRZ").ok().and_then(|f| f.parse().ok()) {
                upscale.set(UpscaleConfig { method: UpscaleMethod::Xbrz, factor });
            }
            (Some(shocora_render::RenderThread::spawn(renderer)?), Some(upscale))
        }
        None => (None, None),
    };
    let render_sender = render_thread.as_ref().and_then(|thread| thread.sender());
    let presses = parse_presses(&std::env::var("SHOCORA_PRESS").unwrap_or_default());
    // SHOCORA_CAPTURE="flip,dir,min_pixels": see `capture_frame`.
    let capture = std::env::var("SHOCORA_CAPTURE").ok().and_then(|text| {
        let mut parts = text.split(',');
        let flip = parts.next()?.parse::<u64>().ok()?;
        let dir = PathBuf::from(parts.next()?);
        Some((flip, dir, parts.next().and_then(|n| n.parse::<u32>().ok()).unwrap_or(64)))
    });

    let ubo_trace = std::env::var("SHOCORA_TRACE_UBO").ok().and_then(|text| {
        let mut parts = text.split(',');
        Some((
            parts.next()?.parse::<u64>().ok()?,
            parts.next()?.parse::<u64>().ok()?,
            PathBuf::from(parts.next()?),
        ))
    });
    let mut ubo_arena = std::collections::HashMap::new();
    let mut ubo_log = String::new();
    let start = Instant::now();
    let mut flips = 0_u64;
    let mut last_report = Instant::now();
    let mut arena = std::collections::HashMap::new();
    let profile = std::env::var_os("SHOCORA_PROFILE").is_some();
    let (mut run_time, mut render_time) = (Duration::ZERO, Duration::ZERO);
    let (mut second_start, mut second_flips) = (Instant::now(), 0_u64);
    let mut flip_rates = Vec::new();
    // Host time between flips, with what the frame before each one was doing.
    let mut flip_gaps: Vec<(Duration, u64, Duration, Duration)> = Vec::new();
    let mut last_flip = Instant::now();
    let mut upscaled_rates = Vec::new();
    let mut upscaled_before = 0;
    let wav = std::env::var_os("SHOCORA_WAV").map(PathBuf::from);
    let mut sound = Vec::new();
    for frame in 0..frames {
        let held = presses
            .iter()
            .filter(|p| frame >= p.frame && frame < p.frame + p.frames)
            .fold(0, |bits, p| bits | p.button);
        let mut pad = GamePadState::from_button_bits(held);
        // SHOCORA_TOUCH="frame,x,y,frames": a finger on the GamePad's screen.
        pad.touch = std::env::var("SHOCORA_TOUCH").ok().and_then(|text| {
            let parts: Vec<u64> = text.split(',').filter_map(|n| n.parse().ok()).collect();
            let [at, x, y, length] = parts[..] else { return None };
            (frame >= at && frame < at + length).then_some((x as u16, y as u16))
        });
        emu.set_gamepad(pad);
        // SHOCORA_RESET_AT=frame: the reset the GUI offers.
        if std::env::var("SHOCORA_RESET_AT").ok().and_then(|at| at.parse().ok()) == Some(frame) {
            emu.reset()?;
            println!("reset at frame {frame}");
        }
        let frame_start = Instant::now();
        emu.run_frame();
        run_time += frame_start.elapsed();
        let run_elapsed = frame_start.elapsed();
        if wav.is_some() {
            sound.extend(emu.take_audio());
        }
        let commands = emu.take_render_commands();
        let new_flips = commands.iter().filter(|c| matches!(c, RenderCommand::Flip)).count() as u64;
        let mut send_elapsed = Duration::ZERO;
        let commands = match &render_sender {
            Some(sender) => {
                let wait = Instant::now();
                sender.send(commands)?;
                send_elapsed = wait.elapsed();
                render_time += send_elapsed;
                Vec::new()
            }
            None => commands,
        };
        if std::env::var_os("SHOCORA_TRACE_DRAWS").is_some() {
            for command in &commands {
                if let RenderCommand::ArenaWrite { offset, bytes } = command {
                    arena.insert(*offset, std::sync::Arc::clone(bytes));
                }
            }
        }
        // SHOCORA_TRACE_DRAWS="first_frame,count": prints draws and their shaders.
        if let Some((first, count)) = std::env::var("SHOCORA_TRACE_DRAWS").ok().and_then(|s| {
            let (a, b) = s.split_once(',')?;
            Some((a.parse::<u64>().ok()?, b.parse::<usize>().ok()?))
        }) {
            if frame >= first {
                let interesting: Vec<_> = commands
                    .iter()
                    .filter(|c| {
                        matches!(
                            c,
                            RenderCommand::Draw(_)
                                | RenderCommand::Clear { .. }
                                | RenderCommand::CopyToScanBuffer { .. }
                                | RenderCommand::CopySurface { .. }
                        )
                    })
                    .collect();
                for command in interesting.iter().skip(interesting.len().saturating_sub(count)) {
                    match command {
                        RenderCommand::Draw(draw) => {
                            if let Some(params) = arena.get(&draw.params_offset) {
                                let word = |i: usize| {
                                    u32::from_le_bytes([
                                        params[i * 4],
                                        params[i * 4 + 1],
                                        params[i * 4 + 2],
                                        params[i * 4 + 3],
                                    ])
                                };
                                println!(
                                    "  params {:08x?} attr {:08x?} stride {:?}",
                                    (0..8).map(word).collect::<Vec<_>>(),
                                    (40..44).map(word).collect::<Vec<_>>(),
                                    (56..60).map(word).collect::<Vec<_>>()
                                );
                                for (name, base) in [("vs", 8), ("ps", 24)] {
                                    for block in 0..16 {
                                        let offset = word(base + block);
                                        if offset != 0 {
                                            if let Some(data) = arena.get(&offset) {
                                                let floats: Vec<f32> = data
                                                    .as_chunks::<4>()
                                                    .0
                                                    .iter()
                                                    .take(16)
                                                    .map(|b| {
                                                        f32::from_le_bytes([b[0], b[1], b[2], b[3]])
                                                    })
                                                    .collect();
                                                println!(
                                                    "  {name} ubo{block} ({} bytes): {floats:?}",
                                                    data.len()
                                                );
                                            }
                                        }
                                    }
                                }
                                for (name, offset) in [("vs", word(0)), ("ps", word(1))] {
                                    if let Some(data) = arena.get(&offset) {
                                        let floats: Vec<f32> = data
                                            .as_chunks::<4>()
                                            .0
                                            .iter()
                                            .take(32)
                                            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                                            .collect();
                                        println!("  {name} alu: {floats:?}");
                                    }
                                }
                                let attribute = word(40);
                                if let Some(data) = arena.get(&attribute) {
                                    println!(
                                        "  vertex bytes: {:02x?}",
                                        &data[..data.len().min(48)]
                                    );
                                }
                            }
                            println!(
                                "draw vs{} ps{} count {} inst {} idx {:?} base {} topo {:?} blend {:?}\n  targets {:?}\n  depth {:?}\n  masks {:?} formats {:?}\n  vp {:?} sc {:?} depth_test {} write {} cmp {} cull {}/{} cw {}\n  textures {:?}",
                                draw.vertex.id,
                                draw.pixel.id,
                                draw.count,
                                draw.instances,
                                draw.index_offset,
                                draw.base_vertex,
                                draw.state.topology,
                                draw.state.blend[0],
                                draw.color_targets.iter().flatten().collect::<Vec<_>>(),
                                draw.depth_target,
                                draw.state.color_masks,
                                draw.state.color_formats,
                                draw.viewport,
                                draw.scissor,
                                draw.state.depth_test,
                                draw.state.depth_write,
                                draw.state.depth_compare,
                                draw.state.cull_front,
                                draw.state.cull_back,
                                draw.state.front_face_cw,
                                draw.textures
                                    .iter()
                                    .map(|t| (
                                        t.binding,
                                        t.key.address,
                                        t.key.format,
                                        t.key.width,
                                        t.key.height
                                    ))
                                    .collect::<Vec<_>>(),
                            );
                            if std::env::var_os("SHOCORA_TRACE_WGSL").is_some() {
                                println!("{}\n{}", draw.vertex.wgsl, draw.pixel.wgsl);
                            }
                        }
                        other => println!("{other:?}"),
                    }
                }
                // SHOCORA_TRACE_SPAN: frames to keep tracing after the first one.
                let span = std::env::var("SHOCORA_TRACE_SPAN")
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                if !commands.is_empty() && frame >= first + span {
                    let Some(renderer) = &mut renderer else { return Ok(()) };
                    renderer.execute(&commands);
                    // SHOCORA_TRACE_TEX=dir saves the 8-bit textures the traced draws read.
                    if let Some(dir) = std::env::var_os("SHOCORA_TRACE_TEX") {
                        let keys: std::collections::HashSet<_> = commands
                            .iter()
                            .filter_map(|c| {
                                if let RenderCommand::Draw(draw) = c {
                                    Some(draw.textures.iter().map(|t| t.key))
                                } else {
                                    None
                                }
                            })
                            .flatten()
                            .collect();
                        for key in keys {
                            if let Some((width, height, rgba)) = renderer.read_surface(key) {
                                let mean = |channel: usize| {
                                    rgba.iter()
                                        .skip(channel)
                                        .step_by(4)
                                        .map(|v| u64::from(*v))
                                        .sum::<u64>()
                                        / u64::from(width * height).max(1)
                                };
                                println!(
                                    "  texture {:#010x} {width}x{height}: mean rgba {} {} {} {}",
                                    key.address,
                                    mean(0),
                                    mean(1),
                                    mean(2),
                                    mean(3)
                                );
                                save_png(
                                    &std::path::Path::new(&dir)
                                        .join(format!("tex_{:08x}.png", key.address)),
                                    width,
                                    height,
                                    &rgba,
                                )?;
                            }
                        }
                    }
                    return Ok(());
                }
            }
        }
        // SHOCORA_TRACE_UBO="first_flip,last_flip,file": uniform blocks of every draw, per flip.
        if let Some((first, last, file)) = &ubo_trace {
            use std::fmt::Write as _;
            for command in &commands {
                match command {
                    RenderCommand::ArenaWrite { offset, bytes } => {
                        ubo_arena.insert(*offset, std::sync::Arc::clone(bytes));
                    }
                    RenderCommand::Draw(draw) if flips >= *first && flips <= *last => {
                        let Some(params) = ubo_arena.get(&draw.params_offset) else { continue };
                        let word = |i: usize| {
                            u32::from_le_bytes([
                                params[i * 4],
                                params[i * 4 + 1],
                                params[i * 4 + 2],
                                params[i * 4 + 3],
                            ])
                        };
                        let _ = write!(
                            ubo_log,
                            "{flips} vs{} ps{} n{}",
                            draw.vertex.id, draw.pixel.id, draw.count
                        );
                        for block in 0..16 {
                            let Some(data) = ubo_arena.get(&word(8 + block)) else { continue };
                            let floats: Vec<String> = data
                                .as_chunks::<4>()
                                .0
                                .iter()
                                .take(16)
                                .map(|b| {
                                    format!("{:.3}", f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                                })
                                .collect();
                            let _ =
                                write!(ubo_log, " | b{block}:{} {}", data.len(), floats.join(","));
                        }
                        ubo_log.push('\n');
                    }
                    _ => {}
                }
            }
            if flips > *last {
                std::fs::write(file, &ubo_log)?;
                return Ok(());
            }
        }
        if let (Some(renderer), Some((flip, dir, min_pixels))) = (&mut renderer, &capture) {
            if flips >= *flip && new_flips > 0 {
                capture_frame(renderer, &commands, dir, *min_pixels)?;
                return Ok(());
            }
        }
        if let Some(renderer) = &mut renderer {
            let render_start = Instant::now();
            renderer.execute(&commands);
            renderer.submit();
            render_time += render_start.elapsed();
            // SHOCORA_SHOTS_FROM: first flip worth a screenshot.
            let shots_from = std::env::var("SHOCORA_SHOTS_FROM")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            if new_flips > 0
                && flips >= shots_from
                && flips / shot_every != (flips + new_flips) / shot_every
            {
                if let Some(dir) = &shot_dir {
                    for screen in Screen::ALL {
                        if let Some((width, height, rgba)) = renderer.read_screen(screen) {
                            let name = format!("{:?}_{:06}.png", screen, flips + new_flips);
                            save_png(&dir.join(name), width, height, &rgba)?;
                        }
                        if let Some((width, height, rgba)) = renderer.read_generated(screen) {
                            let name = format!("Gen{:?}_{:06}.png", screen, flips + new_flips);
                            save_png(&dir.join(name), width, height, &rgba)?;
                        }
                    }
                    if std::env::var_os("SHOCORA_DUMP_TARGETS").is_some() {
                        for (key, draws) in renderer.take_target_log() {
                            println!(
                                "  target {:#010x} {}x{} {:?}: {draws} draws",
                                key.address, key.width, key.height, key.format
                            );
                            if let Some((width, height, rgba)) = renderer.read_surface(key) {
                                let name = format!(
                                    "rt{:06}_{:08x}_{}x{}.png",
                                    flips + new_flips,
                                    key.address,
                                    width,
                                    height
                                );
                                save_png(&dir.join(name), width, height, &rgba)?;
                            }
                        }
                    }
                }
            }
        }
        flips += new_flips;
        second_flips += new_flips;
        if new_flips > 0 {
            flip_gaps.push((last_flip.elapsed(), frame, run_elapsed, send_elapsed));
            last_flip = Instant::now();
        }
        if profile && second_start.elapsed() >= Duration::from_secs(1) {
            flip_rates.push(second_flips);
            if let Some(upscale) = &upscale {
                upscaled_rates.push(upscale.images_shown() - upscaled_before);
                upscaled_before = upscale.images_shown();
            }
            (second_start, second_flips) = (Instant::now(), 0);
        }
        for line in emu.take_console() {
            println!("[guest] {}", line.trim_end());
        }
        if *emu.status() != Status::Running {
            break;
        }
        if last_report.elapsed() > Duration::from_secs(10) {
            last_report = Instant::now();
            let stats = emu.stats();
            println!(
                "... frame {frame} · {:.0} MIPS · {} flips · {} draws ({} skipped)",
                stats.instructions as f64 / 1e6 / start.elapsed().as_secs_f64(),
                stats.gpu.flips,
                stats.gpu.draws,
                stats.gpu.skipped_draws
            );
        }
    }
    if profile {
        println!("flips per second: {flip_rates:?}");
        if upscale.is_some() {
            println!("upscaled images per second: {upscaled_rates:?}");
        }
        // The first flips come while everything is loaded for the first time.
        let settled = flip_gaps.len() / 4;
        let mut gaps: Vec<_> = flip_gaps[settled..].to_vec();
        gaps.sort_by_key(|gap| gap.0);
        if let (Some(median), Some(p99), Some(worst)) =
            (gaps.get(gaps.len() / 2), gaps.get(gaps.len() * 99 / 100), gaps.last())
        {
            println!(
                "time between flips (last 3/4 of the run): median {:.2?}, 99% {:.2?}, worst {:.2?}",
                median.0, p99.0, worst.0
            );
        }
        for (gap, frame, run, send) in gaps.iter().rev().take(6) {
            println!(
                "  {gap:>10.2?} before frame {frame}: run_frame {run:.2?}, waiting for the renderer {send:.2?}"
            );
        }
        let stats = emu.stats();
        println!(
            "run_frame {run_time:.2?} (guest code {:.2?}, of it translating {} blocks {:.2?}; HLE {:.2?}) · render {render_time:.2?}",
            Duration::from_nanos(stats.guest_nanos),
            stats.compiled_blocks,
            Duration::from_nanos(stats.compile_nanos),
            Duration::from_nanos(stats.hle_nanos),
        );
        for (name, calls, nanos) in emu.hle_slowest(16) {
            println!("  {:>10.2?} {calls:>10}  {name}", Duration::from_nanos(nanos));
        }
        println!("time slices ran out at:");
        for (address, slices) in emu.slice_end_profile(8) {
            println!("  {slices:>8}  {}", emu.disassemble_around(address, 0, 1).join(""));
        }
    }
    for (name, calls) in emu.hle_profile(8) {
        println!("  {calls:>12}  {name}");
    }
    if !matches!(emu.status(), Status::Exited(_)) {
        if let Some(cpu) = emu.core(1) {
            println!(
                "pc {:#010x} lr {:#010x} ctr {:#010x} cr {:08x}",
                cpu.pc, cpu.lr, cpu.ctr, cpu.cr
            );
            for row in 0..8 {
                let regs: Vec<String> = (0..4)
                    .map(|c| format!("r{:<2}={:08x}", row * 4 + c, cpu.gpr[row * 4 + c]))
                    .collect();
                println!("  {}", regs.join(" "));
            }
            for line in emu.disassemble_around(cpu.pc, 12, 16) {
                println!("  {line}");
            }
        }
        println!("recent HLE calls:");
        for line in emu.hle_trace() {
            println!("  {line}");
        }
    }
    for spec in
        std::env::var("SHOCORA_DISASM").unwrap_or_default().split(';').filter(|s| !s.is_empty())
    {
        let (at, count) = spec.split_once(',').ok_or("SHOCORA_DISASM: expected addr,count")?;
        let (at, count) = (u32::from_str_radix(at.trim_start_matches("0x"), 16)?, count.parse()?);
        println!("listing {at:#010x}:");
        for line in emu.disassemble_around(at, 0, count) {
            println!("  {line}");
        }
    }
    if let Some(path) = &wav {
        save_wav(path, &sound)?;
    }
    let stats = emu.stats();
    println!("threads:");
    for line in emu.thread_report() {
        println!("  {line}");
    }
    if let Some(renderer) = &renderer {
        println!("generated frames: {}", renderer.frame_gen_setting().images_generated());
    }
    println!("status: {:?}", emu.status());
    println!(
        "frames {} · {:.1} M instructions · {} HLE calls · {} PM4 packets · {} draws ({} skipped) · {} flips · {:.2?}",
        stats.frames,
        stats.instructions as f64 / 1e6,
        stats.hle_calls,
        stats.gpu.packets,
        stats.gpu.draws,
        stats.gpu.skipped_draws,
        stats.gpu.flips,
        start.elapsed()
    );
    Ok(())
}
