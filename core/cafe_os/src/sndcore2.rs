//! `sndcore2.rpl` / `snd_core.rpl` (AX) and `snduser2.rpl` (AXFX).
//!
//! The guest allocates voices and describes them; every 3 ms the "DSP" (here:
//! [`CafeOs::sound_tick`]) renders them and the title's frame callbacks run.
//! Effects (`AXFX`) are accepted and leave the signal dry.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]

use std::collections::{HashMap, VecDeque};

use wiiu_hle_macros::hle_export;
use wiiu_memory::Memory;
use wiiu_sound::{SampleFormat, SampleMemory, Voice};

use crate::{
    CafeOs,
    hle::{HleContext, HleError, HleExport, callback::GuestCall},
};

/// Voices the library can hand out.
const MAX_VOICES: u32 = 96;
const VPB_SIZE: u32 = 0x60;
/// Output rate of the mixer, in samples per second.
pub const OUTPUT_RATE: u32 = 48_000;
/// Samples rendered per AX frame at 48 kHz.
const SAMPLES_PER_FRAME: u32 = 144;
/// Timebase ticks per AX frame (248.625 MHz / 333.33).
const TICKS_PER_FRAME: u64 = 745_875;
/// Frames made up for at most when the guest fell behind real time.
const MAX_CATCH_UP: u64 = 8;
/// Interleaved stereo samples kept for the host (about 200 ms).
const MAX_QUEUED_OUTPUT: usize = 2 * 9600;

mod vpb {
    pub const INDEX: u32 = 0x00;
    pub const STATE: u32 = 0x04;
    pub const PRIORITY: u32 = 0x1C;
    pub const CALLBACK: u32 = 0x20;
    pub const USER: u32 = 0x24;
    pub const OFFSETS: u32 = 0x34;
    pub const CALLBACK_EX: u32 = 0x48;
}

/// `AX_VOICE_STATE`.
const STATE_RUNNING: u32 = 1;

/// `AX_DEV_TV`.
const DEVICE_TV: u32 = 0;

/// `AXFINALMIXCBPARAM` and the planar 32-bit samples it points to: titles that
/// mix in software (every voice of theirs ends up here) overwrite the samples.
mod final_mix {
    pub const CHANNEL_POINTERS: u32 = 0x10;
    pub const SAMPLES: u32 = 0x40;
    /// Bytes of one device's block: pointers and samples for the most channels.
    pub const SIZE: u32 = SAMPLES + 8 * super::SAMPLES_PER_FRAME * 4;

    /// `(channels, devices)` of the callback's view of `device`.
    pub const fn layout(device: u32) -> (u32, u32) {
        if device == super::DEVICE_TV { (6, 1) } else { (4, 2) }
    }
}

/// One allocated voice.
#[derive(Debug, Clone)]
struct AxVoice {
    voice: Voice,
    running: bool,
    volume: f32,
    tv_left: f32,
    tv_right: f32,
    loop_count: u32,
}

impl AxVoice {
    fn new() -> Self {
        Self {
            voice: Voice::new(SampleFormat::Pcm16, 0, 0),
            running: false,
            volume: 1.0,
            tv_left: 1.0,
            tv_right: 1.0,
            loop_count: 0,
        }
    }
}

/// AX library state.
#[derive(Debug, Default)]
pub struct AxState {
    initialized: bool,
    voice_base: u32,
    free: Vec<u32>,
    voices: HashMap<u32, AxVoice>,
    app_frame_callbacks: Vec<u32>,
    frame_callback: u32,
    aux_callbacks: HashMap<u32, (u32, u32)>,
    final_mix_callbacks: HashMap<u32, u32>,
    /// Guest block of each device with a final mix callback (see [`final_mix`]).
    final_mix_blocks: HashMap<u32, u32>,
    fx_hooks: (u32, u32),
    last_tick: u64,
    user_protection: u32,
    /// Interleaved stereo for the host.
    pub output: VecDeque<i16>,
}

struct GuestSamples<'a>(&'a Memory);

impl SampleMemory for GuestSamples<'_> {
    fn read_u8(&self, address: u32) -> u8 {
        self.0.read_u8(address).unwrap_or(0)
    }
}

impl CafeOs {
    /// Runs every AX frame that became due: guest callbacks, then the mixer.
    pub fn sound_tick(&mut self, memory: &mut Memory) {
        if !self.ax.initialized {
            return;
        }
        let now = self.timebase.ticks_since_boot();
        let due = now.saturating_sub(self.ax.last_tick) / TICKS_PER_FRAME;
        if due == 0 {
            return;
        }
        self.ax.last_tick += due * TICKS_PER_FRAME;
        for _ in 0..due.min(MAX_CATCH_UP) {
            if self.ax.frame_callback != 0 {
                self.callbacks
                    .push(GuestCall { function: self.ax.frame_callback, args: Vec::new() });
            }
            for index in 0..self.ax.app_frame_callbacks.len() {
                let function = self.ax.app_frame_callbacks[index];
                self.callbacks.push(GuestCall { function, args: Vec::new() });
            }
            self.mix_frame(memory);
        }
    }

    fn mix_frame(&mut self, memory: &mut Memory) {
        let source = GuestSamples(memory);
        let frame_size = SAMPLES_PER_FRAME as usize * 2;
        let mut mix = vec![0.0_f32; frame_size];
        let mut finished = Vec::new();
        for (address, voice) in &mut self.ax.voices {
            if !voice.running {
                continue;
            }
            let before = voice.voice.current_offset;
            for frame in mix.as_chunks_mut::<2>().0 {
                let sample = voice.voice.render(&source);
                frame[0] = f32::mul_add(sample * voice.volume, voice.tv_left, frame[0]);
                frame[1] = f32::mul_add(sample * voice.volume, voice.tv_right, frame[1]);
            }
            if voice.voice.looping && voice.voice.current_offset < before {
                voice.loop_count += 1;
            }
            if !voice.voice.playing {
                voice.running = false;
                finished.push(*address);
            }
        }
        for address in finished {
            let _ = memory.write_u32(address + vpb::STATE, 0);
        }
        let samples = mix.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16).collect::<Vec<_>>();
        self.play_frame(memory, &samples);
        self.run_final_mix(memory, &samples);
        let output = &mut self.ax.output;
        let excess = output.len().saturating_sub(MAX_QUEUED_OUTPUT);
        output.drain(..excess);
    }
}

impl CafeOs {
    /// Queues what the TV hears: with a final mix callback, its previous result.
    fn play_frame(&mut self, memory: &Memory, mixed: &[i16]) {
        let Some(block) = self.ax.final_mix_blocks.get(&DEVICE_TV).copied() else {
            self.ax.output.extend(mixed);
            return;
        };
        let channel = |channel: u32, sample: u32| {
            let at = block + final_mix::SAMPLES + (channel * SAMPLES_PER_FRAME + sample) * 4;
            memory.read_u32(at).unwrap_or(0).cast_signed().clamp(-32768, 32767) as i16
        };
        for sample in 0..SAMPLES_PER_FRAME {
            self.ax.output.push_back(channel(0, sample));
            self.ax.output.push_back(channel(1, sample));
        }
    }

    /// Hands this frame's samples to the final mix callbacks.
    fn run_final_mix(&mut self, memory: &mut Memory, mixed: &[i16]) {
        let callbacks: Vec<_> = self.ax.final_mix_callbacks.iter().map(|(d, f)| (*d, *f)).collect();
        for (device, function) in callbacks {
            let Some(block) = self.ax.final_mix_blocks.get(&device).copied() else { continue };
            let (channels, devices) = final_mix::layout(device);
            let total = channels * devices;
            let samples = block + final_mix::SAMPLES;
            let _ = memory.fill(samples, total * SAMPLES_PER_FRAME * 4, 0);
            for (i, sample) in mixed.iter().enumerate() {
                let (frame, channel) = ((i / 2) as u32, (i % 2) as u32);
                let at = samples + (channel * SAMPLES_PER_FRAME + frame) * 4;
                let _ = memory.write_u32(at, i32::from(*sample).cast_unsigned());
            }
            for channel in 0..total {
                let pointer = samples + channel * SAMPLES_PER_FRAME * 4;
                let _ =
                    memory.write_u32(block + final_mix::CHANNEL_POINTERS + channel * 4, pointer);
            }
            let _ = memory.write_u32(block, block + final_mix::CHANNEL_POINTERS);
            let _ = memory.write_u16(block + 4, channels as u16);
            let _ = memory.write_u16(block + 6, SAMPLES_PER_FRAME as u16);
            let _ = memory.write_u16(block + 8, devices as u16);
            let _ = memory.write_u16(block + 10, channels as u16);
            self.callbacks.push(GuestCall { function, args: vec![block] });
        }
    }
}

fn initialize(ctx: &mut HleContext<'_>) {
    if ctx.os.ax.initialized {
        return;
    }
    ctx.os.ax.initialized = true;
    ctx.os.ax.last_tick = ctx.os.timebase.ticks_since_boot();
    let base = ctx.os.registry.allocate_data(MAX_VOICES * VPB_SIZE, 64);
    ctx.os.ax.voice_base = base;
    ctx.os.ax.free = (0..MAX_VOICES).rev().map(|i| base + i * VPB_SIZE).collect();
}

#[hle_export(lib = "sndcore2", name = "AXInit")]
fn ax_init(ctx: &mut HleContext<'_>) {
    initialize(ctx);
}

#[hle_export(lib = "sndcore2", name = "AXInitEx")]
fn ax_init_ex(ctx: &mut HleContext<'_>, _pipeline: u32) {
    initialize(ctx);
}

#[hle_export(lib = "sndcore2", name = "AXInitWithParams")]
fn ax_init_with_params(ctx: &mut HleContext<'_>, _params: u32) {
    initialize(ctx);
}

#[hle_export(lib = "sndcore2", name = "AXIsInit")]
fn ax_is_init(ctx: &mut HleContext<'_>) -> bool {
    ctx.os.ax.initialized
}

#[hle_export(lib = "sndcore2", name = "AXQuit")]
fn ax_quit(ctx: &mut HleContext<'_>) {
    ctx.os.ax = AxState::default();
}

#[hle_export(lib = "sndcore2", name = "AXGetMaxVoices")]
fn ax_get_max_voices(_ctx: &mut HleContext<'_>) -> u32 {
    MAX_VOICES
}

#[hle_export(lib = "sndcore2", name = "AXGetInputSamplesPerFrame")]
fn ax_get_input_samples_per_frame(_ctx: &mut HleContext<'_>) -> u32 {
    SAMPLES_PER_FRAME
}

#[hle_export(lib = "sndcore2", name = "AXGetInputSamplesPerSec")]
fn ax_get_input_samples_per_sec(_ctx: &mut HleContext<'_>) -> u32 {
    OUTPUT_RATE
}

#[hle_export(lib = "sndcore2", name = "AXRegisterAppFrameCallback")]
fn ax_register_app_frame_callback(ctx: &mut HleContext<'_>, function: u32) -> i32 {
    if function == 0 {
        return -17;
    }
    ctx.os.ax.app_frame_callbacks.push(function);
    0
}

#[hle_export(lib = "sndcore2", name = "AXDeregisterAppFrameCallback")]
fn ax_deregister_app_frame_callback(ctx: &mut HleContext<'_>, function: u32) -> i32 {
    ctx.os.ax.app_frame_callbacks.retain(|f| *f != function);
    0
}

#[hle_export(lib = "sndcore2", name = "AXRegisterFrameCallback")]
fn ax_register_frame_callback(ctx: &mut HleContext<'_>, function: u32) -> u32 {
    std::mem::replace(&mut ctx.os.ax.frame_callback, function)
}

/// Aux effects are never called: the signal stays dry.
#[hle_export(lib = "sndcore2", name = "AXRegisterAuxCallback")]
fn ax_register_aux_callback(
    ctx: &mut HleContext<'_>,
    device: u32,
    id: u32,
    bus: u32,
    function: u32,
    context: u32,
) -> i32 {
    ctx.os.ax.aux_callbacks.insert((device << 16) | (id << 8) | bus, (function, context));
    0
}

#[hle_export(lib = "sndcore2", name = "AXGetAuxCallback")]
fn ax_get_aux_callback(
    ctx: &mut HleContext<'_>,
    device: u32,
    id: u32,
    bus: u32,
    function: u32,
    context: u32,
) -> Result<i32, HleError> {
    let (f, c) =
        ctx.os.ax.aux_callbacks.get(&((device << 16) | (id << 8) | bus)).copied().unwrap_or((0, 0));
    ctx.memory.write_u32(function, f)?;
    ctx.memory.write_u32(context, c)?;
    Ok(0)
}

#[hle_export(lib = "sndcore2", name = "AXRegisterDeviceFinalMixCallback")]
fn ax_register_device_final_mix_callback(
    ctx: &mut HleContext<'_>,
    device: u32,
    function: u32,
) -> i32 {
    if function == 0 {
        ctx.os.ax.final_mix_callbacks.remove(&device);
        ctx.os.ax.final_mix_blocks.remove(&device);
        return 0;
    }
    ctx.os.ax.final_mix_callbacks.insert(device, function);
    if !ctx.os.ax.final_mix_blocks.contains_key(&device) {
        let block = ctx.os.registry.allocate_data(final_mix::SIZE, 64);
        ctx.os.ax.final_mix_blocks.insert(device, block);
    }
    0
}

#[hle_export(lib = "sndcore2", name = "AXGetDeviceFinalMixCallback")]
fn ax_get_device_final_mix_callback(
    ctx: &mut HleContext<'_>,
    device: u32,
    out: u32,
) -> Result<i32, HleError> {
    let function = ctx.os.ax.final_mix_callbacks.get(&device).copied().unwrap_or(0);
    ctx.memory.write_u32(out, function)?;
    Ok(0)
}

/// Stereo on both outputs.
#[hle_export(lib = "sndcore2", name = "AXGetDeviceMode")]
fn ax_get_device_mode(ctx: &mut HleContext<'_>, _device: u32, out: u32) -> Result<i32, HleError> {
    ctx.memory.write_u32(out, 0)?;
    Ok(0)
}

#[hle_export(lib = "sndcore2", name = "AXUserBegin")]
fn ax_user_begin(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.ax.user_protection += 1;
    ctx.os.ax.user_protection
}

#[hle_export(lib = "sndcore2", name = "AXUserEnd")]
fn ax_user_end(ctx: &mut HleContext<'_>) -> u32 {
    ctx.os.ax.user_protection = ctx.os.ax.user_protection.saturating_sub(1);
    ctx.os.ax.user_protection
}

#[hle_export(lib = "sndcore2", name = "AXUserIsProtected")]
fn ax_user_is_protected(ctx: &mut HleContext<'_>) -> bool {
    ctx.os.ax.user_protection != 0
}

#[hle_export(lib = "sndcore2", name = "AXVoiceBegin")]
fn ax_voice_begin(_ctx: &mut HleContext<'_>, _voice: u32) -> u32 {
    1
}

#[hle_export(lib = "sndcore2", name = "AXVoiceEnd")]
fn ax_voice_end(_ctx: &mut HleContext<'_>, _voice: u32) -> u32 {
    0
}

#[hle_export(lib = "sndcore2", name = "AXVoiceIsProtected")]
fn ax_voice_is_protected(_ctx: &mut HleContext<'_>, _voice: u32) -> bool {
    false
}

fn acquire(
    ctx: &mut HleContext<'_>,
    priority: u32,
    callback_ex: u32,
    callback: u32,
    user: u32,
) -> Result<u32, HleError> {
    initialize(ctx);
    ctx.os.registry.commit(ctx.memory)?;
    let Some(address) = ctx.os.ax.free.pop() else { return Ok(0) };
    ctx.memory.fill(address, VPB_SIZE, 0)?;
    ctx.memory.write_u32(address + vpb::INDEX, (address - ctx.os.ax.voice_base) / VPB_SIZE)?;
    ctx.memory.write_u32(address + vpb::PRIORITY, priority)?;
    ctx.memory.write_u32(address + vpb::CALLBACK, callback)?;
    ctx.memory.write_u32(address + vpb::CALLBACK_EX, callback_ex)?;
    ctx.memory.write_u32(address + vpb::USER, user)?;
    ctx.os.ax.voices.insert(address, AxVoice::new());
    Ok(address)
}

#[hle_export(lib = "sndcore2", name = "AXAcquireVoiceEx")]
fn ax_acquire_voice_ex(
    ctx: &mut HleContext<'_>,
    priority: u32,
    callback: u32,
    user: u32,
) -> Result<u32, HleError> {
    acquire(ctx, priority, callback, 0, user)
}

#[hle_export(lib = "sndcore2", name = "AXAcquireVoice")]
fn ax_acquire_voice(
    ctx: &mut HleContext<'_>,
    priority: u32,
    callback: u32,
    user: u32,
) -> Result<u32, HleError> {
    acquire(ctx, priority, 0, callback, user)
}

#[hle_export(lib = "sndcore2", name = "AXFreeVoice")]
fn ax_free_voice(ctx: &mut HleContext<'_>, voice: u32) {
    if ctx.os.ax.voices.remove(&voice).is_some() {
        ctx.os.ax.free.push(voice);
    }
}

#[hle_export(lib = "sndcore2", name = "AXSetVoicePriority")]
fn ax_set_voice_priority(
    ctx: &mut HleContext<'_>,
    voice: u32,
    priority: u32,
) -> Result<(), HleError> {
    Ok(ctx.memory.write_u32(voice + vpb::PRIORITY, priority)?)
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceState")]
fn ax_set_voice_state(ctx: &mut HleContext<'_>, voice: u32, state: u32) -> Result<(), HleError> {
    ctx.memory.write_u32(voice + vpb::STATE, state)?;
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.running = state == STATE_RUNNING;
        if entry.running {
            entry.voice.playing = true;
        }
    }
    Ok(())
}

#[hle_export(lib = "sndcore2", name = "AXIsVoiceRunning")]
fn ax_is_voice_running(ctx: &mut HleContext<'_>, voice: u32) -> bool {
    ctx.os.ax.voices.get(&voice).is_some_and(|v| v.running)
}

/// `AXPBOFFSET`: format, loop flag, then offsets relative to the sample pointer.
fn set_offsets(
    ctx: &mut HleContext<'_>,
    voice: u32,
    offsets: u32,
    samples: Option<u32>,
) -> Result<(), HleError> {
    let mut raw = [0; 0x14];
    ctx.memory.read_bytes(offsets, &mut raw)?;
    ctx.memory.write_bytes(voice + vpb::OFFSETS, &raw)?;
    let word = |o: usize| u32::from_be_bytes([raw[o], raw[o + 1], raw[o + 2], raw[o + 3]]);
    let format = u16::from_be_bytes([raw[0], raw[1]]);
    let looping = u16::from_be_bytes([raw[2], raw[3]]) != 0;
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        let target = &mut entry.voice;
        target.format = match format {
            0 => SampleFormat::Adpcm,
            0x19 => SampleFormat::Pcm8,
            _ => SampleFormat::Pcm16,
        };
        target.looping = looping;
        target.loop_offset = word(4);
        target.end_offset = word(8);
        target.current_offset = word(12);
        target.base_address = samples.unwrap_or_else(|| word(16));
    }
    Ok(())
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceOffsets")]
fn ax_set_voice_offsets(
    ctx: &mut HleContext<'_>,
    voice: u32,
    offsets: u32,
) -> Result<(), HleError> {
    set_offsets(ctx, voice, offsets, None)
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceOffsetsEx")]
fn ax_set_voice_offsets_ex(
    ctx: &mut HleContext<'_>,
    voice: u32,
    offsets: u32,
    samples: u32,
) -> Result<(), HleError> {
    set_offsets(ctx, voice, offsets, Some(samples))
}

#[hle_export(lib = "sndcore2", name = "AXGetVoiceOffsets")]
fn ax_get_voice_offsets(ctx: &mut HleContext<'_>, voice: u32, out: u32) -> Result<(), HleError> {
    let mut raw = [0; 0x14];
    ctx.memory.read_bytes(voice + vpb::OFFSETS, &mut raw)?;
    if let Some(entry) = ctx.os.ax.voices.get(&voice) {
        raw[12..16].copy_from_slice(&entry.voice.current_offset.to_be_bytes());
    }
    ctx.memory.write_bytes(out, &raw)?;
    Ok(())
}

#[hle_export(lib = "sndcore2", name = "AXGetVoiceCurrentOffsetEx")]
fn ax_get_voice_current_offset_ex(ctx: &mut HleContext<'_>, voice: u32, _samples: u32) -> u32 {
    ctx.os.ax.voices.get(&voice).map_or(0, |v| v.voice.current_offset)
}

#[hle_export(lib = "sndcore2", name = "AXGetVoiceLoopCount")]
fn ax_get_voice_loop_count(ctx: &mut HleContext<'_>, voice: u32) -> u32 {
    ctx.os.ax.voices.get(&voice).map_or(0, |v| v.loop_count)
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceLoop")]
fn ax_set_voice_loop(ctx: &mut HleContext<'_>, voice: u32, looping: u32) {
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.voice.looping = looping != 0;
    }
}

macro_rules! offset_setters {
    ($($fn_name:ident, $name:literal, $ex_fn:ident, $ex_name:literal, $field:ident;)*) => {$(
        #[hle_export(lib = "sndcore2", name = $name)]
        fn $fn_name(ctx: &mut HleContext<'_>, voice: u32, offset: u32) {
            if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
                entry.voice.$field = offset;
            }
        }

        #[hle_export(lib = "sndcore2", name = $ex_name)]
        fn $ex_fn(ctx: &mut HleContext<'_>, voice: u32, offset: u32, samples: u32) {
            if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
                entry.voice.$field = offset;
                entry.voice.base_address = samples;
            }
        }
    )*};
}

offset_setters! {
    ax_set_voice_loop_offset, "AXSetVoiceLoopOffset", ax_set_voice_loop_offset_ex, "AXSetVoiceLoopOffsetEx", loop_offset;
    ax_set_voice_end_offset, "AXSetVoiceEndOffset", ax_set_voice_end_offset_ex, "AXSetVoiceEndOffsetEx", end_offset;
    ax_set_voice_current_offset, "AXSetVoiceCurrentOffset", ax_set_voice_current_offset_ex, "AXSetVoiceCurrentOffsetEx", current_offset;
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceSamplesAddr")]
fn ax_set_voice_samples_addr(ctx: &mut HleContext<'_>, voice: u32, samples: u32) {
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.voice.base_address = samples;
    }
}

/// `AXPBADPCM`: sixteen coefficients, gain, predictor/scale and history.
#[hle_export(lib = "sndcore2", name = "AXSetVoiceAdpcm")]
fn ax_set_voice_adpcm(ctx: &mut HleContext<'_>, voice: u32, adpcm: u32) -> Result<(), HleError> {
    let mut coefficients = [0_i16; 16];
    for (i, coefficient) in coefficients.iter_mut().enumerate() {
        *coefficient = ctx.memory.read_u16(adpcm + 2 * i as u32)?.cast_signed();
    }
    let history = [
        ctx.memory.read_u16(adpcm + 0x24)?.cast_signed(),
        ctx.memory.read_u16(adpcm + 0x26)?.cast_signed(),
    ];
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.voice.adpcm_coefficients = coefficients;
        entry.voice.adpcm_state =
            wiiu_sound::adpcm::AdpcmState { hist1: history[0], hist2: history[1] };
    }
    Ok(())
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceAdpcmLoop")]
fn ax_set_voice_adpcm_loop(_ctx: &mut HleContext<'_>, _voice: u32, _adpcm_loop: u32) {}

/// `AXPBSRC`: 16.16 fixed-point ratio.
#[hle_export(lib = "sndcore2", name = "AXSetVoiceSrc")]
fn ax_set_voice_src(ctx: &mut HleContext<'_>, voice: u32, src: u32) -> Result<(), HleError> {
    let ratio = ctx.memory.read_u32(src)?;
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.voice.ratio = ratio as f32 / 65536.0;
    }
    Ok(())
}

#[hle_export(lib = "sndcore2", name = "AXSetVoiceSrcRatio")]
fn ax_set_voice_src_ratio(ctx: &mut HleContext<'_>, voice: u32, ratio: f32) -> i32 {
    if !(0.0..=64.0).contains(&ratio) {
        return -1;
    }
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.voice.ratio = ratio;
    }
    0
}

/// `AXPBVE`: current volume in 1.15 fixed point.
#[hle_export(lib = "sndcore2", name = "AXSetVoiceVe")]
fn ax_set_voice_ve(ctx: &mut HleContext<'_>, voice: u32, ve: u32) -> Result<(), HleError> {
    let volume = f32::from(ctx.memory.read_u16(ve)?) / 32768.0;
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.volume = volume;
    }
    Ok(())
}

/// `AXCHMIX` array, `{volume, delta}` per channel: only the main bus of the TV is mixed.
#[hle_export(lib = "sndcore2", name = "AXSetVoiceDeviceMix")]
fn ax_set_voice_device_mix(
    ctx: &mut HleContext<'_>,
    voice: u32,
    device: u32,
    _index: u32,
    mix: u32,
) -> Result<i32, HleError> {
    if device != 0 {
        return Ok(0);
    }
    let left = f32::from(ctx.memory.read_u16(mix)?) / 32768.0;
    let right = f32::from(ctx.memory.read_u16(mix + 4)?) / 32768.0;
    if let Some(entry) = ctx.os.ax.voices.get_mut(&voice) {
        entry.tv_left = left;
        entry.tv_right = right;
    }
    Ok(0)
}

// ---- snduser2 --------------------------------------------------------------

#[hle_export(lib = "snduser2", name = "AXFXSetHooks")]
fn axfx_set_hooks(ctx: &mut HleContext<'_>, alloc: u32, free: u32) {
    ctx.os.ax.fx_hooks = (alloc, free);
}

#[hle_export(lib = "snduser2", name = "AXFXGetHooks")]
fn axfx_get_hooks(ctx: &mut HleContext<'_>, alloc: u32, free: u32) -> Result<(), HleError> {
    ctx.memory.write_u32(alloc, ctx.os.ax.fx_hooks.0)?;
    ctx.memory.write_u32(free, ctx.os.ax.fx_hooks.1)?;
    Ok(())
}

/// Placeholder work area: the effects stay dry, like in Cemu.
const FX_MEM_SIZE: u32 = 0xC;

#[hle_export(lib = "snduser2", name = "AXFXReverbStdExpGetMemSize")]
fn axfx_reverb_std_exp_get_mem_size(_ctx: &mut HleContext<'_>, _params: u32) -> u32 {
    FX_MEM_SIZE
}

#[hle_export(lib = "snduser2", name = "AXFXReverbStdExpInit")]
fn axfx_reverb_std_exp_init(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}

#[hle_export(lib = "snduser2", name = "AXFXReverbStdExpSettings")]
fn axfx_reverb_std_exp_settings(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}

#[hle_export(lib = "snduser2", name = "AXFXReverbStdExpSettingsUpdate")]
fn axfx_reverb_std_exp_settings_update(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}

#[hle_export(lib = "snduser2", name = "AXFXDelayExpGetMemSize")]
fn axfx_delay_exp_get_mem_size(_ctx: &mut HleContext<'_>, _params: u32) -> u32 {
    FX_MEM_SIZE
}

#[hle_export(lib = "snduser2", name = "AXFXDelayExpInit")]
fn axfx_delay_exp_init(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}

#[hle_export(lib = "snduser2", name = "AXFXDelayExpSettings")]
fn axfx_delay_exp_settings(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}

#[hle_export(lib = "snduser2", name = "AXFXDelayExpSettingsUpdate")]
fn axfx_delay_exp_settings_update(_ctx: &mut HleContext<'_>, _params: u32) -> bool {
    true
}

pub const EXPORTS: &[HleExport] = &[
    AX_INIT,
    AX_INIT_EX,
    AX_INIT_WITH_PARAMS,
    AX_IS_INIT,
    AX_QUIT,
    AX_GET_MAX_VOICES,
    AX_GET_INPUT_SAMPLES_PER_FRAME,
    AX_GET_INPUT_SAMPLES_PER_SEC,
    AX_REGISTER_APP_FRAME_CALLBACK,
    AX_DEREGISTER_APP_FRAME_CALLBACK,
    AX_REGISTER_FRAME_CALLBACK,
    AX_REGISTER_AUX_CALLBACK,
    AX_GET_AUX_CALLBACK,
    AX_REGISTER_DEVICE_FINAL_MIX_CALLBACK,
    AX_GET_DEVICE_FINAL_MIX_CALLBACK,
    AX_GET_DEVICE_MODE,
    AX_USER_BEGIN,
    AX_USER_END,
    AX_USER_IS_PROTECTED,
    AX_VOICE_BEGIN,
    AX_VOICE_END,
    AX_VOICE_IS_PROTECTED,
    AX_ACQUIRE_VOICE_EX,
    AX_ACQUIRE_VOICE,
    AX_FREE_VOICE,
    AX_SET_VOICE_PRIORITY,
    AX_SET_VOICE_STATE,
    AX_IS_VOICE_RUNNING,
    AX_SET_VOICE_OFFSETS,
    AX_SET_VOICE_OFFSETS_EX,
    AX_GET_VOICE_OFFSETS,
    AX_GET_VOICE_CURRENT_OFFSET_EX,
    AX_GET_VOICE_LOOP_COUNT,
    AX_SET_VOICE_LOOP,
    AX_SET_VOICE_LOOP_OFFSET,
    AX_SET_VOICE_LOOP_OFFSET_EX,
    AX_SET_VOICE_END_OFFSET,
    AX_SET_VOICE_END_OFFSET_EX,
    AX_SET_VOICE_CURRENT_OFFSET,
    AX_SET_VOICE_CURRENT_OFFSET_EX,
    AX_SET_VOICE_SAMPLES_ADDR,
    AX_SET_VOICE_ADPCM,
    AX_SET_VOICE_ADPCM_LOOP,
    AX_SET_VOICE_SRC,
    AX_SET_VOICE_SRC_RATIO,
    AX_SET_VOICE_VE,
    AX_SET_VOICE_DEVICE_MIX,
    AXFX_SET_HOOKS,
    AXFX_GET_HOOKS,
    AXFX_REVERB_STD_EXP_GET_MEM_SIZE,
    AXFX_REVERB_STD_EXP_INIT,
    AXFX_REVERB_STD_EXP_SETTINGS,
    AXFX_REVERB_STD_EXP_SETTINGS_UPDATE,
    AXFX_DELAY_EXP_GET_MEM_SIZE,
    AXFX_DELAY_EXP_INIT,
    AXFX_DELAY_EXP_SETTINGS,
    AXFX_DELAY_EXP_SETTINGS_UPDATE,
];

#[cfg(test)]
mod tests {
    use crate::hle::test_env::{SCRATCH, TestEnv};

    #[test]
    fn voices_play_and_stop_at_their_end() {
        let mut env = TestEnv::new();
        let voice = env.call("sndcore2", "AXAcquireVoiceEx", &[1, 0, 0]);
        assert_ne!(voice, 0);
        let (offsets, samples) = (SCRATCH, SCRATCH + 0x100);
        env.memory.write_u16(samples, 0x4000).unwrap();
        env.memory.write_u16(offsets, 0xA).unwrap(); // PCM16
        env.memory.write_u32(offsets + 8, 1).unwrap(); // end
        env.memory.write_u32(offsets + 16, samples).unwrap();
        env.call("sndcore2", "AXSetVoiceOffsets", &[voice, offsets]);
        env.call("sndcore2", "AXSetVoiceState", &[voice, 1]);
        assert_eq!(env.call("sndcore2", "AXIsVoiceRunning", &[voice]), 1);
        env.os.mix_frame(&mut env.memory);
        assert_eq!(env.call("sndcore2", "AXIsVoiceRunning", &[voice]), 0);
        assert!(env.os.ax.output.iter().any(|s| *s != 0));
    }
}
