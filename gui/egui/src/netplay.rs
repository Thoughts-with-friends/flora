//! The second player, as this window sees it: hosting one, or being one.
use std::sync::{Arc, Mutex, PoisonError, mpsc::Sender};

use eframe::egui;
use shocora_gui_common::{audio::AudioOutput, console::ConsoleCommand};
use shocora_netplay::{Client, ClientEvents, Frame, Host, HostConfig, HostEvents, protocol};
use shocora_render::StreamTap;
use wiiu_emu::GamePadState;

/// The `WPAD` channel the guest's controller sits on: the first Pro Controller.
const GUEST_CHANNEL: usize = 0;

/// The window's role.
pub enum Netplay {
    Off,
    Host(Host),
    Guest(Guest),
}

struct HostLink {
    /// In a mutex only because hosts are shared between threads.
    commands: Mutex<Sender<ConsoleCommand>>,
    tap: StreamTap,
}

impl HostEvents for HostLink {
    fn input(&self, pad: Option<GamePadState>) {
        let commands = self.commands.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = commands.send(ConsoleCommand::ProController(GUEST_CHANNEL, pad));
    }

    fn streaming(&self, wanted: bool) {
        self.tap.set_enabled(wanted);
    }

    fn take_frame(&self) -> Option<Frame> {
        let frame = self.tap.take()?;
        Some(Frame { width: frame.width, height: frame.height, rgba: frame.rgba })
    }
}

/// Starts letting a second player in.
///
/// # Errors
/// Fails if the port is taken or the address is not this machine's.
pub fn host(
    config: HostConfig,
    commands: Sender<ConsoleCommand>,
    tap: StreamTap,
) -> std::io::Result<Host> {
    Host::start(config, HostLink { commands: Mutex::new(commands), tap })
}

/// Being a second player.
pub struct Guest {
    pub client: Client,
    frame: Arc<Mutex<Option<Frame>>>,
    texture: Option<egui::TextureHandle>,
}

impl Guest {
    /// The newest picture from the host, as a texture.
    pub fn texture(&mut self, ctx: &egui::Context) -> Option<&egui::TextureHandle> {
        let frame = self.frame.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(frame) = frame {
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [frame.width as usize, frame.height as usize],
                &frame.rgba,
            );
            let options = egui::TextureOptions::LINEAR;
            match &mut self.texture {
                Some(texture) => texture.set(image, options),
                None => self.texture = Some(ctx.load_texture("netplay", image, options)),
            }
        }
        self.texture.as_ref()
    }
}

struct GuestLink {
    frame: Arc<Mutex<Option<Frame>>>,
    audio: AudioOutput,
    repaint: egui::Context,
}

impl ClientEvents for GuestLink {
    fn frame(&mut self, frame: Frame) {
        *self.frame.lock().unwrap_or_else(PoisonError::into_inner) = Some(frame);
        self.repaint.request_repaint();
    }

    fn audio(&mut self, samples: &[i16]) {
        self.audio.push(samples);
    }

    fn changed(&mut self) {
        self.repaint.request_repaint();
    }
}

/// Starts joining the host at `address`.
///
/// # Errors
/// Fails if the link's thread cannot start.
pub fn join(address: &str, password: &str, repaint: egui::Context) -> std::io::Result<Guest> {
    let frame = Arc::new(Mutex::new(None));
    let link_frame = Arc::clone(&frame);
    let client = Client::start(address, password, move || GuestLink {
        frame: link_frame,
        // The output upsamples to what the device runs at.
        audio: AudioOutput::open(protocol::AUDIO_RATE),
        repaint,
    })?;
    Ok(Guest { client, frame, texture: None })
}
