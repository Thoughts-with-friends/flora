//! A second player on the local network.
//!
//! ```text
//!   host: runs the title                     guest: only a screen and a controller
//!   ───────────────────                      ─────────────────────────────────────
//!   TV, thinned to 20 fps ── JPEG ─────────► shown
//!   sound, 48 kHz → 16 kHz ── PCM ─────────► played, upsampled by the audio output
//!   Pro Controller 1 ◄───── buttons, sticks  keyboard and controllers
//!   ping ◄──────────────────────────────────► ping: 3 s of silence ends the link
//! ```
//!
//! One TCP connection carries everything. The host listens on the address it is given and
//! only lets in guests from that address's network; a password is optional. The password
//! itself never travels, but nothing is encrypted: anyone on the network can see the pictures,
//! and could try to guess the password from a recorded greeting.
pub mod client;
pub mod host;
pub mod protocol;

use std::time::Duration;

pub use client::{Client, ClientEvents};
pub use host::{Host, HostConfig, HostEvents};
pub use protocol::{DEFAULT_PORT, Frame, Refusal};

/// How often each side asks whether the other is still there.
pub const PING_INTERVAL: Duration = Duration::from_secs(1);
/// Silence after which the other side counts as gone.
pub const PEER_TIMEOUT: Duration = Duration::from_secs(3);

/// What a link is doing, for the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkState {
    /// The host waits for a guest.
    Waiting,
    /// The guest tries to reach the host.
    Connecting,
    /// The host did not answer; the guest keeps trying.
    Unreachable(String),
    /// The host said no; trying again would not help.
    Refused(Refusal),
    Connected {
        peer: String,
        /// Round trip of the last ping, once one came back.
        round_trip: Option<Duration>,
    },
}

/// The address of this machine others on the network would use, if it has one.
pub fn local_address() -> Option<std::net::IpAddr> {
    // Connecting a UDP socket sends nothing: it only makes the system choose a route.
    let socket = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("192.0.2.1", 9)).ok()?;
    socket.local_addr().ok().map(|address| address.ip())
}
