//! The guest's side: keeps trying to reach the host, shows what it sends, sends back the
//! controller.
use std::{
    io,
    net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use crate::{
    DEFAULT_PORT, LinkState, PEER_TIMEOUT, PING_INTERVAL,
    protocol::{self, Frame, GuestInput, HostStatus, Message, Refusal},
};

/// What keeps a guest out.
#[derive(Debug)]
enum JoinError {
    /// The host said no.
    Refused(Refusal),
    Link(io::Error),
}

impl From<io::Error> for JoinError {
    fn from(error: io::Error) -> Self {
        Self::Link(error)
    }
}

/// How long one attempt to reach the host may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// How long the guest waits between attempts to reach the host.
const RETRY_DELAY: Duration = Duration::from_secs(2);
/// Stopping is noticed at least this often while waiting.
const STOP_POLL: Duration = Duration::from_millis(50);

/// What the guest does with what arrives. Lives on the link's thread.
pub trait ClientEvents {
    fn frame(&mut self, frame: Frame);
    /// Interleaved stereo at [`protocol::AUDIO_RATE`].
    fn audio(&mut self, samples: &[i16]);
    /// The host's state, about once a second.
    fn status(&mut self, status: HostStatus);
    /// The link changed; a good moment to draw again.
    fn changed(&mut self);
}

#[derive(Default)]
struct Shared {
    writer: Mutex<Option<TcpStream>>,
    /// Sent again after reconnecting.
    input: Mutex<GuestInput>,
    stop: AtomicBool,
}

impl Shared {
    fn send(&self, message: &Message) -> io::Result<()> {
        let mut writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        match writer.as_mut() {
            Some(stream) => protocol::write_message(stream, message),
            None => Ok(()),
        }
    }
}

/// A link to a host, kept up until dropped.
pub struct Client {
    shared: Arc<Shared>,
    state: Arc<Mutex<LinkState>>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

/// `host` or `host:port`, with the default port if none is given.
fn with_port(address: &str) -> String {
    let address = address.trim();
    if address.contains(':') { address.to_owned() } else { format!("{address}:{DEFAULT_PORT}") }
}

impl Client {
    /// Starts connecting to `address`; `password` may be empty. `events` is made on the
    /// link's thread, because audio outputs must stay on the thread that opened them.
    ///
    /// # Errors
    /// Fails if the thread cannot start.
    pub fn start<E: ClientEvents>(
        address: &str,
        password: &str,
        events: impl FnOnce() -> E + Send + 'static,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let state = Arc::new(Mutex::new(LinkState::Connecting));
        let (address, password) = (with_port(address), password.to_owned());
        let thread = std::thread::Builder::new().name("shocora-net-client".into()).spawn({
            let (shared, state) = (Arc::clone(&shared), Arc::clone(&state));
            move || run(&address, &password, &mut events(), &shared, &state)
        })?;
        Ok(Self { shared, state, thread: Some(thread) })
    }

    pub fn state(&self) -> LinkState {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Sends the controller's state to the host.
    pub fn set_input(&self, input: GuestInput) {
        *self.shared.input.lock().unwrap_or_else(PoisonError::into_inner) = input;
        // A failure shows on the reading side, which then reconnects.
        let _ = self.shared.send(&Message::Input(input));
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        drop_connection(&self.shared);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn drop_connection(shared: &Shared) {
    let writer = shared.writer.lock().unwrap_or_else(PoisonError::into_inner).take();
    if let Some(stream) = writer {
        let _ = stream.shutdown(Shutdown::Both);
    }
}

fn set_state(state: &Mutex<LinkState>, new: LinkState, events: &mut impl ClientEvents) {
    *state.lock().unwrap_or_else(PoisonError::into_inner) = new;
    events.changed();
}

/// Waits unless stopped; `true` if it is time to stop.
fn wait(shared: &Shared, time: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < time {
        if shared.stop.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(STOP_POLL);
    }
    shared.stop.load(Ordering::Relaxed)
}

/// The host's IPv4 address, the only kind hosts listen on: `localhost` may give IPv6 first.
fn resolve(address: &str) -> io::Result<SocketAddr> {
    address
        .to_socket_addrs()?
        .find(SocketAddr::is_ipv4)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such address"))
}

/// The next message of the greeting, unless the host refuses.
fn greeting_message(stream: &mut TcpStream) -> Result<Message, JoinError> {
    match protocol::read_message(stream)? {
        Message::Refused(reason) => Err(JoinError::Refused(reason)),
        message => Ok(message),
    }
}

fn no_greeting() -> JoinError {
    JoinError::Link(io::Error::new(io::ErrorKind::InvalidData, "no greeting"))
}

fn connect(address: &str, password: &str) -> Result<TcpStream, JoinError> {
    let mut stream = TcpStream::connect_timeout(&resolve(address)?, CONNECT_TIMEOUT)?;
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(PEER_TIMEOUT))?;
    stream.set_write_timeout(Some(PEER_TIMEOUT))?;
    protocol::write_message(&mut stream, &Message::Hello)?;
    if greeting_message(&mut stream)? != Message::Hello {
        return Err(no_greeting());
    }
    let Message::Challenge(challenge) = greeting_message(&mut stream)? else {
        return Err(no_greeting());
    };
    let answer = protocol::answer(&challenge, password);
    protocol::write_message(&mut stream, &Message::Answer(answer))?;
    match greeting_message(&mut stream)? {
        Message::Welcome => Ok(stream),
        _ => Err(no_greeting()),
    }
}

/// Connects, serves the connection until it ends, and tries again. Every attempt doubles as
/// the check whether the host can be reached at all.
fn run(
    address: &str,
    password: &str,
    events: &mut impl ClientEvents,
    shared: &Arc<Shared>,
    state: &Mutex<LinkState>,
) {
    while !shared.stop.load(Ordering::Relaxed) {
        let stream = match connect(address, password) {
            Ok(stream) => stream,
            Err(JoinError::Refused(reason)) => {
                log::warn!("netplay: the host refused: {reason:?}");
                set_state(state, LinkState::Refused(reason), events);
                return;
            }
            Err(JoinError::Link(error)) => {
                set_state(state, LinkState::Unreachable(error.to_string()), events);
                if wait(shared, RETRY_DELAY) {
                    return;
                }
                continue;
            }
        };
        let peer = address.to_owned();
        set_state(state, LinkState::Connected { peer: peer.clone(), round_trip: None }, events);
        if let Err(error) = serve(stream, &peer, events, shared, state) {
            if !shared.stop.load(Ordering::Relaxed) {
                log::info!("netplay: lost the host: {error}");
                set_state(state, LinkState::Unreachable(error.to_string()), events);
            }
        }
        drop_connection(shared);
        if wait(shared, RETRY_DELAY) {
            return;
        }
    }
}

fn serve(
    mut reading: TcpStream,
    peer: &str,
    events: &mut impl ClientEvents,
    shared: &Arc<Shared>,
    state: &Mutex<LinkState>,
) -> io::Result<()> {
    *shared.writer.lock().unwrap_or_else(PoisonError::into_inner) = Some(reading.try_clone()?);
    let input = *shared.input.lock().unwrap_or_else(PoisonError::into_inner);
    shared.send(&Message::Input(input))?;
    let started = Instant::now();
    let ended = Arc::new(AtomicBool::new(false));
    let pinger = std::thread::Builder::new().name("shocora-net-ping".into()).spawn({
        let (shared, ended) = (Arc::clone(shared), Arc::clone(&ended));
        move || {
            while !ended.load(Ordering::Relaxed) {
                if shared.send(&Message::Ping(protocol::nanos(started))).is_err() {
                    return;
                }
                let waited = Instant::now();
                while waited.elapsed() < PING_INTERVAL {
                    if ended.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(STOP_POLL);
                }
            }
        }
    })?;
    let result = receive(&mut reading, peer, events, shared, state, started);
    ended.store(true, Ordering::Relaxed);
    let _ = pinger.join();
    result
}

/// Handles what the host sends. The read timeout turns silence into an error.
fn receive(
    reading: &mut TcpStream,
    peer: &str,
    events: &mut impl ClientEvents,
    shared: &Shared,
    state: &Mutex<LinkState>,
    started: Instant,
) -> io::Result<()> {
    loop {
        match protocol::read_message(reading)? {
            Message::Status(status) => events.status(status),
            Message::Video(display, jpeg) => match protocol::decode_frame(display, &jpeg) {
                Ok(frame) => events.frame(frame),
                Err(error) => log::warn!("netplay: bad picture: {error}"),
            },
            Message::Audio(samples) => events.audio(&samples),
            Message::Ping(value) => shared.send(&Message::Pong(value))?,
            Message::Pong(sent) => {
                let round_trip =
                    Duration::from_nanos(protocol::nanos(started).saturating_sub(sent));
                let connected =
                    LinkState::Connected { peer: peer.to_owned(), round_trip: Some(round_trip) };
                set_state(state, connected, events);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use wiiu_peripherals::{GamePadState, controller::VpadButtons};

    use super::*;
    use crate::{
        host::{Host, HostConfig, HostEvents},
        protocol::{Display, HostState},
    };

    const LOOPBACK: &str = "127.0.0.1";

    /// A host's side of the greeting, for tests playing the host themselves.
    fn greet(stream: &mut std::net::TcpStream) {
        protocol::write_message(stream, &Message::Hello).unwrap();
        assert_eq!(protocol::read_message(stream).unwrap(), Message::Hello);
        protocol::write_message(stream, &Message::Challenge([3; 16])).unwrap();
        assert!(matches!(protocol::read_message(stream).unwrap(), Message::Answer(_)));
        protocol::write_message(stream, &Message::Welcome).unwrap();
    }

    fn guest_events() -> impl FnOnce() -> RecordingGuest + Send + 'static {
        let (frames, _f) = mpsc::channel();
        let (audio, _a) = mpsc::channel();
        let (statuses, _s) = mpsc::channel();
        move || RecordingGuest { frames, audio, statuses }
    }
    const PATIENCE: Duration = Duration::from_secs(10);

    /// Reports everything the host's side sees.
    struct RecordingHost {
        inputs: Mutex<mpsc::Sender<Option<GuestInput>>>,
        wanted: Mutex<mpsc::Sender<bool>>,
        streaming: AtomicBool,
    }

    impl HostEvents for RecordingHost {
        fn input(&self, input: Option<GuestInput>) {
            let _ = self.inputs.lock().unwrap().send(input);
        }

        fn streaming(&self, wanted: bool) {
            self.streaming.store(wanted, Ordering::Relaxed);
            let _ = self.wanted.lock().unwrap().send(wanted);
        }

        fn take_frames(&self) -> Vec<Frame> {
            if !self.streaming.swap(false, Ordering::Relaxed) {
                return Vec::new();
            }
            [(Display::Tv, 16), (Display::GamePad, 8)]
                .map(|(display, width)| Frame {
                    display,
                    width,
                    height: 8,
                    rgba: [10, 200, 30, 255].repeat(width as usize * 8),
                })
                .to_vec()
        }

        fn status(&self) -> HostStatus {
            HostStatus { state: HostState::Running, fps: 30.0, ..HostStatus::default() }
        }
    }

    struct RecordingGuest {
        frames: mpsc::Sender<(Display, u32, u32)>,
        audio: mpsc::Sender<Vec<i16>>,
        statuses: mpsc::Sender<HostStatus>,
    }

    impl ClientEvents for RecordingGuest {
        fn frame(&mut self, frame: Frame) {
            let _ = self.frames.send((frame.display, frame.width, frame.height));
        }

        fn audio(&mut self, samples: &[i16]) {
            let _ = self.audio.send(samples.to_vec());
        }

        fn status(&mut self, status: HostStatus) {
            let _ = self.statuses.send(status);
        }

        fn changed(&mut self) {}
    }

    struct Rig {
        host: Host,
        inputs: mpsc::Receiver<Option<GuestInput>>,
        wanted: mpsc::Receiver<bool>,
    }

    fn start_host() -> Rig {
        start_host_with("")
    }

    fn start_host_with(password: &str) -> Rig {
        let (inputs_tx, inputs) = mpsc::channel();
        let (wanted_tx, wanted) = mpsc::channel();
        let events = RecordingHost {
            inputs: Mutex::new(inputs_tx),
            wanted: Mutex::new(wanted_tx),
            streaming: AtomicBool::new(false),
        };
        let config = HostConfig::new("127.0.0.1/8", 0, password).unwrap();
        Rig { host: Host::start(config, events).unwrap(), inputs, wanted }
    }

    fn eventually(what: &str, condition: impl Fn() -> bool) {
        let start = Instant::now();
        while !condition() {
            assert!(start.elapsed() < PATIENCE, "never happened: {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn guest_plays_along_and_leaves() {
        let rig = start_host();
        let (frames_tx, frames) = mpsc::channel();
        let (audio_tx, audio) = mpsc::channel();
        let (statuses_tx, statuses) = mpsc::channel();
        let address = format!("{LOOPBACK}:{}", rig.host.port());
        let client = Client::start(&address, "", move || RecordingGuest {
            frames: frames_tx,
            audio: audio_tx,
            statuses: statuses_tx,
        })
        .unwrap();

        eventually("the link", || matches!(client.state(), LinkState::Connected { .. }));
        assert_eq!(rig.wanted.recv_timeout(PATIENCE), Ok(true));
        assert_eq!(frames.recv_timeout(PATIENCE), Ok((Display::Tv, 16, 8)));
        assert_eq!(frames.recv_timeout(PATIENCE), Ok((Display::GamePad, 8, 8)));
        let status = statuses.recv_timeout(PATIENCE).unwrap();
        assert_eq!((status.state, status.fps), (HostState::Running, 30.0));

        // A held button and a touch reach the host (the first input is the idle one sent on
        // joining).
        let pad = GamePadState {
            touch: Some((100, 200)),
            ..GamePadState::from_button_bits(VpadButtons::A.bits())
        };
        client.set_input(GuestInput { pad, to_gamepad: true });
        loop {
            let input = rig.inputs.recv_timeout(PATIENCE).unwrap().unwrap();
            if input.pad.held == VpadButtons::A {
                assert_eq!(input.pad.touch, Some((100, 200)));
                assert!(input.to_gamepad);
                break;
            }
        }

        // 48 kHz in, 16 kHz out, in 20 ms pieces.
        rig.host.audio_sink().send(vec![1000; 2 * 960]).unwrap();
        let samples = audio.recv_timeout(PATIENCE).unwrap();
        assert_eq!(samples.len(), 640);
        assert!(samples.iter().all(|sample| *sample == 1000));

        // The pings measure the round trip on both sides.
        let measured =
            |state: LinkState| matches!(state, LinkState::Connected { round_trip: Some(_), .. });
        eventually("the guest's round trip", || measured(client.state()));
        eventually("the host's round trip", || measured(rig.host.state()));

        drop(client);
        eventually("the host noticing", || rig.host.state() == LinkState::Waiting);
        // The controller gets unplugged.
        loop {
            if rig.inputs.recv_timeout(PATIENCE).unwrap().is_none() {
                break;
            }
        }
    }

    #[test]
    fn unreachable_hosts_are_reported_and_tried_again() {
        // Nobody listens on a port that was just free.
        let free = std::net::TcpListener::bind((LOOPBACK, 0)).unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);
        let (frames, _f) = mpsc::channel();
        let (audio, _a) = mpsc::channel();
        let (statuses, _s) = mpsc::channel();
        let client = Client::start(&format!("{LOOPBACK}:{port}"), "", move || RecordingGuest {
            frames,
            audio,
            statuses,
        })
        .unwrap();
        eventually("the report", || matches!(client.state(), LinkState::Unreachable(_)));

        // The host appears: the guest gets in by itself.
        let listener = std::net::TcpListener::bind((LOOPBACK, port)).unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        greet(&mut stream);
        eventually("the link", || matches!(client.state(), LinkState::Connected { .. }));
    }

    #[test]
    fn a_silent_host_counts_as_gone() {
        let listener = std::net::TcpListener::bind((LOOPBACK, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (frames, _f) = mpsc::channel();
        let (audio, _a) = mpsc::channel();
        let (statuses, _s) = mpsc::channel();
        let client = Client::start(&format!("{LOOPBACK}:{port}"), "", move || RecordingGuest {
            frames,
            audio,
            statuses,
        })
        .unwrap();
        // Answers the greeting, then never says anything again but keeps the socket open.
        let (mut stream, _) = listener.accept().unwrap();
        greet(&mut stream);
        eventually("the link", || matches!(client.state(), LinkState::Connected { .. }));
        eventually("the timeout", || matches!(client.state(), LinkState::Unreachable(_)));
    }

    #[test]
    fn the_seat_is_taken_while_a_guest_plays() {
        let rig = start_host();
        let address = format!("{LOOPBACK}:{}", rig.host.port());
        let guest = || {
            let (frames, _f) = mpsc::channel();
            let (audio, _a) = mpsc::channel();
            let (statuses, _s) = mpsc::channel();
            let address = address.clone();
            Client::start(&address, "", move || RecordingGuest { frames, audio, statuses }).unwrap()
        };
        let first = guest();
        eventually("the first guest", || matches!(first.state(), LinkState::Connected { .. }));
        let second = guest();
        eventually("the refusal", || matches!(second.state(), LinkState::Unreachable(_)));
        assert!(matches!(first.state(), LinkState::Connected { .. }));
    }

    #[test]
    fn the_right_password_opens_the_door() {
        let rig = start_host_with("secret");
        let address = format!("{LOOPBACK}:{}", rig.host.port());
        let guest = Client::start(&address, "secret", guest_events()).unwrap();
        eventually("the link", || matches!(guest.state(), LinkState::Connected { .. }));
        assert!(matches!(rig.host.state(), LinkState::Connected { .. }));
    }

    #[test]
    fn a_wrong_password_is_refused_and_not_tried_again() {
        let rig = start_host_with("secret");
        let address = format!("{LOOPBACK}:{}", rig.host.port());
        for password in ["wrong", ""] {
            let guest = Client::start(&address, password, guest_events()).unwrap();
            let refused = LinkState::Refused(Refusal::WrongPassword);
            eventually("the refusal", || guest.state() == refused);
            assert_eq!(rig.host.state(), LinkState::Waiting);
        }
    }

    #[test]
    fn a_guest_from_another_network_is_refused() {
        // Everything on loopback is this machine, which is always let in: refuses by hand.
        let listener = std::net::TcpListener::bind((LOOPBACK, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let guest = Client::start(&format!("{LOOPBACK}:{port}"), "", guest_events()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        crate::host::refuse(stream, Refusal::Network).unwrap();
        let refused = LinkState::Refused(Refusal::Network);
        eventually("the refusal", || guest.state() == refused);
    }

    fn silent_host_events() -> RecordingHost {
        let (inputs, _) = mpsc::channel();
        let (wanted, _) = mpsc::channel();
        RecordingHost {
            inputs: Mutex::new(inputs),
            wanted: Mutex::new(wanted),
            streaming: AtomicBool::new(false),
        }
    }

    /// A host on a network address lets a window on this machine in at any of its addresses.
    #[test]
    fn this_machine_joins_at_any_of_its_addresses() {
        // A link-local address of an adapter that is down cannot be reached by anyone.
        let reachable: Vec<String> = crate::local_addresses()
            .iter()
            .filter(|local| !local.address.is_loopback() && !local.address.is_link_local())
            .map(|local| local.address.to_string())
            .collect();
        // Machines without a network have nothing to try this on.
        let Some(first) = reachable.first() else { return };
        let config = HostConfig::new(&format!("{first}/32"), 0, "").unwrap();
        let host = Host::start(config, silent_host_events()).unwrap();
        let port = host.port();
        let mut addresses = vec![LOOPBACK.to_owned(), "localhost".to_owned()];
        addresses.extend(reachable.iter().cloned());
        for address in addresses {
            let guest = Client::start(&format!("{address}:{port}"), "", guest_events()).unwrap();
            let start = Instant::now();
            while !matches!(guest.state(), LinkState::Connected { .. }) {
                assert!(start.elapsed() < PATIENCE, "{address}: {:?}", guest.state());
                std::thread::sleep(Duration::from_millis(10));
            }
            drop(guest);
            eventually("the seat to free up", || host.state() == LinkState::Waiting);
        }
    }

    /// The default: a host on a loopback address only listens there.
    #[test]
    fn a_loopback_host_listens_on_loopback_only() {
        let config = HostConfig::new(LOOPBACK, 0, "").unwrap();
        let host = Host::start(config, silent_host_events()).unwrap();
        let port = host.port();
        let guest = Client::start(&format!("localhost:{port}"), "", guest_events()).unwrap();
        eventually("the link", || matches!(guest.state(), LinkState::Connected { .. }));
        drop(guest);

        let others = crate::local_addresses()
            .into_iter()
            .filter(|local| !local.address.is_loopback() && !local.address.is_link_local());
        for other in others {
            let at = SocketAddr::from((other.address, port));
            assert!(
                TcpStream::connect_timeout(&at, CONNECT_TIMEOUT).is_err(),
                "{}",
                other.interface
            );
        }
    }

    #[test]
    fn hosts_refuse_addresses_of_other_machines() {
        // From the block reserved for documentation: never a real adapter's.
        let config = HostConfig::new("192.0.2.1", 0, "").unwrap();
        assert!(Host::start(config, silent_host_events()).is_err());
    }

    #[test]
    fn a_network_is_as_large_as_the_adapter_says() {
        for local in crate::local_addresses() {
            let config = HostConfig::new(&local.address.to_string(), 0, "").unwrap();
            assert_eq!(config.prefix, local.prefix, "{}", local.interface);
        }
    }

    #[test]
    fn hosts_admit_their_own_network_only() {
        let ip = |text: &str| text.parse::<std::net::IpAddr>().unwrap();
        let home = HostConfig::new(" 192.168.1.10/24 ", 0, "").unwrap();
        let at = ip("192.168.1.10");
        assert_eq!(home.network(), "192.168.1.0/24");
        assert!(home.admits(ip("192.168.1.200"), at));
        assert!(home.admits(ip("::ffff:192.168.1.200"), at));
        assert!(!home.admits(ip("192.168.2.10"), at));
        assert!(!home.admits(ip("8.8.8.8"), at));
        assert!(!home.admits(ip("fe80::1"), at));

        let wide = HostConfig::new("10.1.2.3/16", 0, "").unwrap();
        let at = ip("10.1.2.3");
        assert_eq!(wide.network(), "10.1.0.0/16");
        assert!(wide.admits(ip("10.1.200.1"), at));
        assert!(!wide.admits(ip("10.2.0.1"), at));

        // Two machines on a VPN, whose network spans a whole /8.
        let vpn = HostConfig::new("26.1.2.3/8", 0, "").unwrap();
        let at = ip("26.1.2.3");
        assert_eq!(vpn.network(), "26.0.0.0/8");
        assert!(vpn.admits(ip("26.200.3.4"), at));
        assert!(!vpn.admits(ip("27.62.81.253"), at));

        // This machine, at whichever address, even one off the host's network.
        assert!(home.admits(ip("127.0.0.1"), ip("127.0.0.1")));
        assert!(home.admits(ip("::1"), ip("::1")));
        assert!(home.admits(ip("26.1.2.3"), ip("26.1.2.3")));
        // Another machine that merely reached this one at that address.
        assert!(!home.admits(ip("26.200.3.4"), ip("26.1.2.3")));

        assert!(HostConfig::new("not an address", 0, "").is_err());
        assert!(HostConfig::new("10.0.0.1/33", 0, "").is_err());
    }

    #[test]
    fn addresses_get_the_default_port() {
        assert_eq!(with_port(" 192.168.0.2 "), format!("192.168.0.2:{DEFAULT_PORT}"));
        assert_eq!(with_port("192.168.0.2:1234"), "192.168.0.2:1234");
    }
}
