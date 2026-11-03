//! The side running the title: lets one guest in at a time.
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use wiiu_peripherals::GamePadState;

use crate::{
    LinkState, PEER_TIMEOUT, PING_INTERVAL,
    protocol::{self, Downsampler, Frame, Message, Refusal},
};

/// Size of the network when only an address is given: `192.168.1.x`, as most homes have.
const DEFAULT_PREFIX: u8 = 24;
/// How long a wrong password keeps the door shut: guessing takes too long this way.
const REFUSAL_DELAY: Duration = Duration::from_secs(1);

/// How often the host looks for new guests, sound and pictures.
const TICK: Duration = Duration::from_millis(5);
/// Sound sent to the guest in one message: 20 ms.
const AUDIO_CHUNK: usize = protocol::AUDIO_RATE as usize / 50 * 2;
/// Frames of sound the emulator may be ahead before it is dropped: a second.
const AUDIO_QUEUE: usize = 60;

/// What the host needs from the emulator.
pub trait HostEvents: Send + Sync + 'static {
    /// The guest's controller; `None` when it left.
    fn input(&self, pad: Option<GamePadState>);
    /// Whether anyone looks at the pictures, which cost time to make.
    fn streaming(&self, wanted: bool);
    /// The newest picture since the last call.
    fn take_frame(&self) -> Option<Frame>;
}

/// Where a host listens and whom it lets in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostConfig {
    /// This machine's address on the network: only that interface listens.
    pub address: Ipv4Addr,
    /// Bits of the address that make up the network.
    pub prefix: u8,
    pub port: u16,
    /// Empty means anyone on the network may join.
    pub password: String,
}

impl HostConfig {
    /// `text` is `192.168.1.10`, or `192.168.1.10/16` for a network other than the usual /24.
    ///
    /// # Errors
    /// Says what is wrong with `text`.
    pub fn new(text: &str, port: u16, password: &str) -> Result<Self, String> {
        let text = text.trim();
        let (address, prefix) = text.split_once('/').unwrap_or((text, ""));
        let address: Ipv4Addr =
            address.parse().map_err(|_| format!("IPv4 アドレスではありません: {address}"))?;
        let prefix = match prefix {
            "" => DEFAULT_PREFIX,
            bits => bits
                .parse()
                .ok()
                .filter(|bits| *bits <= 32)
                .ok_or_else(|| format!("ネットワークの指定が不正です: /{bits}"))?,
        };
        Ok(Self { address, prefix, port, password: password.to_owned() })
    }

    const fn mask(&self) -> u32 {
        // Shifting by 32 is not defined for a u32.
        if self.prefix == 0 { 0 } else { u32::MAX << (32 - self.prefix as u32) }
    }

    /// The network guests may come from, like `192.168.1.0/24`.
    pub fn network(&self) -> String {
        let network = Ipv4Addr::from_bits(self.address.to_bits() & self.mask());
        format!("{network}/{}", self.prefix)
    }

    /// Whether `peer` is on this host's network.
    pub fn admits(&self, peer: IpAddr) -> bool {
        match peer.to_canonical() {
            IpAddr::V4(peer) => (peer.to_bits() ^ self.address.to_bits()) & self.mask() == 0,
            IpAddr::V6(_) => false,
        }
    }
}

/// A guest being served.
struct Session {
    /// Replies to its pings come from the reading thread: one writer at a time.
    writer: Arc<Mutex<TcpStream>>,
    reader: JoinHandle<()>,
    /// Cleared when the reading side stops.
    alive: Arc<AtomicBool>,
    started: Instant,
    downsampler: Downsampler,
    audio: Vec<i16>,
    last_ping: Option<Instant>,
}

impl Session {
    fn send(&self, message: &Message) -> io::Result<()> {
        let mut stream = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        protocol::write_message(&mut *stream, message)
    }

    fn close(self) {
        let stream = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = stream.shutdown(Shutdown::Both);
        drop(stream);
        let _ = self.reader.join();
    }
}

/// Listens for a guest until dropped.
pub struct Host {
    state: Arc<Mutex<LinkState>>,
    audio: SyncSender<Vec<i16>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    port: u16,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host").field("port", &self.port).finish_non_exhaustive()
    }
}

impl Host {
    /// Starts listening; port zero lets the system choose.
    ///
    /// # Errors
    /// Fails if the port is taken or the address is not this machine's.
    pub fn start(config: HostConfig, events: impl HostEvents) -> io::Result<Self> {
        let listener = TcpListener::bind((config.address, config.port))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let state = Arc::new(Mutex::new(LinkState::Waiting));
        let stop = Arc::new(AtomicBool::new(false));
        let (audio, audio_rx) = mpsc::sync_channel(AUDIO_QUEUE);
        let thread = std::thread::Builder::new().name("shocora-net-host".into()).spawn({
            let (state, stop) = (Arc::clone(&state), Arc::clone(&stop));
            move || serve(&listener, &config, &audio_rx, &Arc::new(events), &state, &stop)
        })?;
        Ok(Self { state, audio, stop, thread: Some(thread), port })
    }

    pub const fn port(&self) -> u16 {
        self.port
    }

    pub fn state(&self) -> LinkState {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Where the emulator sends a copy of its sound (48 kHz stereo). Full means dropped.
    pub fn audio_sink(&self) -> SyncSender<Vec<i16>> {
        self.audio.clone()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn set_state(state: &Mutex<LinkState>, new: LinkState) {
    *state.lock().unwrap_or_else(PoisonError::into_inner) = new;
}

fn serve<E: HostEvents>(
    listener: &TcpListener,
    config: &HostConfig,
    audio: &Receiver<Vec<i16>>,
    events: &Arc<E>,
    state: &Arc<Mutex<LinkState>>,
    stop: &AtomicBool,
) {
    let mut session: Option<Session> = None;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(TICK);
        match listener.accept() {
            Ok((stream, peer)) if !config.admits(peer.ip()) => {
                log::warn!("netplay: {peer} is not on {}", config.network());
                let _ = refuse(stream, Refusal::Network);
            }
            Ok((stream, peer)) if session.is_none() => {
                match open(stream, peer, config, events, state) {
                    Ok(new) => {
                        log::info!("netplay: {peer} joined");
                        events.streaming(true);
                        session = Some(new);
                    }
                    Err(error) => log::warn!("netplay: {peer} could not join: {error}"),
                }
            }
            // One guest at a time: the second player's seat is taken.
            Ok((_, peer)) => log::info!("netplay: turned {peer} away"),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => log::warn!("netplay: accept: {error}"),
        }
        let sound: Vec<Vec<i16>> = audio.try_iter().collect();
        let Some(current) = &mut session else { continue };
        let healthy =
            current.alive.load(Ordering::Relaxed) && pump(current, &sound, events.as_ref()).is_ok();
        if !healthy {
            leave(&mut session, events.as_ref());
            set_state(state, LinkState::Waiting);
        }
    }
    leave(&mut session, events.as_ref());
}

/// Ends the session, if any: the second player's controller is unplugged.
fn leave(session: &mut Option<Session>, events: &impl HostEvents) {
    if let Some(gone) = session.take() {
        gone.close();
        log::info!("netplay: the guest left");
        events.streaming(false);
        events.input(None);
    }
}

/// Says no to a guest. Accepted sockets may inherit the listener's non-blocking mode.
fn refuse(mut stream: TcpStream, reason: Refusal) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_write_timeout(Some(PEER_TIMEOUT))?;
    protocol::write_message(&mut stream, &Message::Refused(reason))
}

/// Asks for the password, or an empty one when there is none.
fn check_password(stream: &mut TcpStream, password: &str) -> io::Result<()> {
    let challenge = protocol::new_challenge()?;
    protocol::write_message(stream, &Message::Challenge(challenge))?;
    let Message::Answer(answer) = protocol::read_message(stream)? else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "no answer"));
    };
    if !protocol::answers_match(&answer, &protocol::answer(&challenge, password)) {
        protocol::write_message(stream, &Message::Refused(Refusal::WrongPassword))?;
        std::thread::sleep(REFUSAL_DELAY);
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "wrong password"));
    }
    protocol::write_message(stream, &Message::Welcome)
}

/// Greets a new guest and starts listening to it.
fn open<E: HostEvents>(
    mut stream: TcpStream,
    peer: SocketAddr,
    config: &HostConfig,
    events: &Arc<E>,
    state: &Arc<Mutex<LinkState>>,
) -> io::Result<Session> {
    // Accepted sockets may inherit the listener's non-blocking mode.
    stream.set_nonblocking(false)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(PEER_TIMEOUT))?;
    stream.set_write_timeout(Some(PEER_TIMEOUT))?;
    protocol::write_message(&mut stream, &Message::Hello)?;
    let mut reading = stream.try_clone()?;
    if protocol::read_message(&mut reading)? != Message::Hello {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "no greeting"));
    }
    check_password(&mut stream, &config.password)?;
    let writer = Arc::new(Mutex::new(stream));
    let alive = Arc::new(AtomicBool::new(true));
    let started = Instant::now();
    set_state(state, LinkState::Connected { peer: peer.to_string(), round_trip: None });
    let reader = std::thread::Builder::new().name("shocora-net-read".into()).spawn({
        let (writer, alive) = (Arc::clone(&writer), Arc::clone(&alive));
        let (events, state) = (Arc::clone(events), Arc::clone(state));
        move || {
            if let Err(error) =
                listen(&mut reading, &writer, events.as_ref(), &state, peer, started)
            {
                log::info!("netplay: lost the guest: {error}");
            }
            alive.store(false, Ordering::Relaxed);
        }
    })?;
    Ok(Session {
        writer,
        reader,
        alive,
        started,
        downsampler: Downsampler::default(),
        audio: Vec::new(),
        last_ping: None,
    })
}

/// Handles what the guest sends until the connection ends. The read timeout makes silence an
/// error, so a guest that vanished without a word is noticed too.
fn listen(
    reading: &mut TcpStream,
    writer: &Mutex<TcpStream>,
    events: &impl HostEvents,
    state: &Mutex<LinkState>,
    peer: SocketAddr,
    started: Instant,
) -> io::Result<()> {
    loop {
        match protocol::read_message(reading)? {
            Message::Input(pad) => events.input(Some(pad)),
            Message::Ping(value) => {
                let mut stream = writer.lock().unwrap_or_else(PoisonError::into_inner);
                protocol::write_message(&mut *stream, &Message::Pong(value))?;
            }
            Message::Pong(sent) => {
                let round_trip =
                    Duration::from_nanos(protocol::nanos(started).saturating_sub(sent));
                set_state(
                    state,
                    LinkState::Connected { peer: peer.to_string(), round_trip: Some(round_trip) },
                );
            }
            _ => {}
        }
    }
}

/// Sends what is due.
fn pump(session: &mut Session, sound: &[Vec<i16>], events: &impl HostEvents) -> io::Result<()> {
    if session.last_ping.is_none_or(|last| last.elapsed() >= PING_INTERVAL) {
        session.last_ping = Some(Instant::now());
        session.send(&Message::Ping(protocol::nanos(session.started)))?;
    }
    for samples in sound {
        session.downsampler.push(samples, &mut session.audio);
    }
    while session.audio.len() >= AUDIO_CHUNK {
        let rest = session.audio.split_off(AUDIO_CHUNK);
        let chunk = std::mem::replace(&mut session.audio, rest);
        session.send(&Message::Audio(chunk))?;
    }
    if let Some(frame) = events.take_frame() {
        session.send(&Message::Video(protocol::encode_frame(&frame)?))?;
    }
    Ok(())
}
