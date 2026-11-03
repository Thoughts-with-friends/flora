//! What travels over the connection: `tag: u8, length: u32, payload`, big-endian.
use std::{
    io::{self, Cursor, Read, Write},
    time::Instant,
};

use image::{ExtendedColorType, ImageFormat, ImageReader, Limits, codecs::jpeg::JpegEncoder};
use sha2::{Digest, Sha256};
use wiiu_peripherals::{
    GamePadState,
    controller::{Stick, VpadButtons},
};

/// Port the host listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 47_800;
/// Rate of the sound on the wire: interleaved stereo.
pub const AUDIO_RATE: u32 = 16_000;
/// Rate of the sound the emulator produces.
const INPUT_AUDIO_RATE: u32 = 48_000;
/// Bumped when the messages change: both sides must speak the same version.
const VERSION: u16 = 2;
/// Anything longer did not come from a peer speaking this protocol.
const MAX_PAYLOAD: usize = 4 << 20;
/// Largest picture a guest decodes.
const MAX_FRAME_SIDE: u32 = 4096;
const JPEG_QUALITY: u8 = 70;

mod tag {
    pub const HELLO: u8 = 1;
    pub const PING: u8 = 2;
    pub const PONG: u8 = 3;
    pub const INPUT: u8 = 4;
    pub const VIDEO: u8 = 5;
    pub const AUDIO: u8 = 6;
    pub const CHALLENGE: u8 = 7;
    pub const ANSWER: u8 = 8;
    pub const WELCOME: u8 = 9;
    pub const REFUSED: u8 = 10;
}

/// Bytes of the random value a host greets a guest with.
const CHALLENGE_SIZE: usize = 16;
/// Bytes of a guest's answer: a SHA-256 digest.
const ANSWER_SIZE: usize = 32;

/// Why a host does not let a guest in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The guest is not on the host's network.
    Network,
    WrongPassword,
}

/// A fresh value for a guest to prove it knows the password with.
///
/// # Errors
/// Fails if the system has no randomness to give.
pub fn new_challenge() -> io::Result<[u8; CHALLENGE_SIZE]> {
    let mut challenge = [0; CHALLENGE_SIZE];
    getrandom::fill(&mut challenge).map_err(|error| io::Error::other(error.to_string()))?;
    Ok(challenge)
}

/// What proves the password without sending it: a recorded answer is no use for the next
/// greeting, whose challenge differs.
pub fn answer(challenge: &[u8; CHALLENGE_SIZE], password: &str) -> [u8; ANSWER_SIZE] {
    let mut hash = Sha256::new();
    hash.update(b"shocora netplay");
    hash.update(challenge);
    hash.update(password.as_bytes());
    hash.finalize().into()
}

/// Whether two answers are equal, taking the same time wherever they differ.
pub fn answers_match(a: &[u8; ANSWER_SIZE], b: &[u8; ANSWER_SIZE]) -> bool {
    a.iter().zip(b).fold(0, |difference, (x, y)| difference | (x ^ y)) == 0
}

/// A picture as RGBA8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Hello,
    Ping(u64),
    Pong(u64),
    Input(GamePadState),
    /// A JPEG picture.
    Video(Vec<u8>),
    /// Interleaved stereo at [`AUDIO_RATE`].
    Audio(Vec<i16>),
    Challenge([u8; CHALLENGE_SIZE]),
    Answer([u8; ANSWER_SIZE]),
    Welcome,
    Refused(Refusal),
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

fn encode_input(pad: &GamePadState) -> Vec<u8> {
    let mut out = Vec::with_capacity(24);
    out.extend(pad.held.bits().to_be_bytes());
    for value in [pad.left_stick.x, pad.left_stick.y, pad.right_stick.x, pad.right_stick.y] {
        out.extend(value.to_be_bytes());
    }
    out
}

fn decode_input(payload: &[u8]) -> io::Result<GamePadState> {
    let word = |at: usize| -> io::Result<[u8; 4]> {
        payload
            .get(at..at + 4)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| invalid("short input message"))
    };
    // What the other side sends is not trusted to be a real stick position.
    let axis = |at: usize| -> io::Result<f32> {
        let value = f32::from_be_bytes(word(at)?);
        Ok(if value.is_finite() { value.clamp(-1.0, 1.0) } else { 0.0 })
    };
    Ok(GamePadState {
        held: VpadButtons::from_bits_truncate(u32::from_be_bytes(word(0)?)),
        left_stick: Stick { x: axis(4)?, y: axis(8)? },
        right_stick: Stick { x: axis(12)?, y: axis(16)? },
        touch: None,
    })
}

/// What a ping carries: its time of sending on the clock of the side that sends it.
pub(crate) fn nanos(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Writes one message.
///
/// # Errors
/// Fails when the connection does.
pub fn write_message(out: &mut impl Write, message: &Message) -> io::Result<()> {
    let (tag, payload) = match message {
        Message::Hello => (tag::HELLO, VERSION.to_be_bytes().to_vec()),
        Message::Ping(value) => (tag::PING, value.to_be_bytes().to_vec()),
        Message::Pong(value) => (tag::PONG, value.to_be_bytes().to_vec()),
        Message::Input(pad) => (tag::INPUT, encode_input(pad)),
        Message::Video(jpeg) => (tag::VIDEO, jpeg.clone()),
        Message::Audio(samples) => {
            (tag::AUDIO, samples.iter().flat_map(|sample| sample.to_be_bytes()).collect())
        }
        Message::Challenge(challenge) => (tag::CHALLENGE, challenge.to_vec()),
        Message::Answer(answer) => (tag::ANSWER, answer.to_vec()),
        Message::Welcome => (tag::WELCOME, Vec::new()),
        Message::Refused(Refusal::Network) => (tag::REFUSED, vec![0]),
        Message::Refused(Refusal::WrongPassword) => (tag::REFUSED, vec![1]),
    };
    let length = u32::try_from(payload.len()).map_err(|_| invalid("message too long"))?;
    let mut packet = Vec::with_capacity(5 + payload.len());
    packet.push(tag);
    packet.extend(length.to_be_bytes());
    packet.extend(payload);
    // In one piece: with `TCP_NODELAY` a header on its own would be a packet of its own.
    out.write_all(&packet)
}

/// Reads one message.
///
/// # Errors
/// Fails when the connection does or the bytes make no sense.
pub fn read_message(input: &mut impl Read) -> io::Result<Message> {
    let mut header = [0; 5];
    input.read_exact(&mut header)?;
    let length = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
    if length > MAX_PAYLOAD {
        return Err(invalid("message too long"));
    }
    let mut payload = vec![0; length];
    input.read_exact(&mut payload)?;
    let value = || -> io::Result<u64> {
        let bytes: [u8; 8] =
            payload.as_slice().try_into().map_err(|_| invalid("bad ping message"))?;
        Ok(u64::from_be_bytes(bytes))
    };
    match header[0] {
        tag::HELLO => {
            if payload != VERSION.to_be_bytes() {
                return Err(invalid("the other side runs a different shocora version"));
            }
            Ok(Message::Hello)
        }
        tag::PING => Ok(Message::Ping(value()?)),
        tag::PONG => Ok(Message::Pong(value()?)),
        tag::INPUT => decode_input(&payload).map(Message::Input),
        tag::VIDEO => Ok(Message::Video(payload)),
        tag::AUDIO => Ok(Message::Audio(
            payload.as_chunks::<2>().0.iter().map(|bytes| i16::from_be_bytes(*bytes)).collect(),
        )),
        tag::CHALLENGE => payload
            .as_slice()
            .try_into()
            .map(Message::Challenge)
            .map_err(|_| invalid("bad challenge")),
        tag::ANSWER => {
            payload.as_slice().try_into().map(Message::Answer).map_err(|_| invalid("bad answer"))
        }
        tag::WELCOME => Ok(Message::Welcome),
        tag::REFUSED => match payload.as_slice() {
            [0] => Ok(Message::Refused(Refusal::Network)),
            [1] => Ok(Message::Refused(Refusal::WrongPassword)),
            _ => Err(invalid("bad refusal")),
        },
        _ => Err(invalid("unknown message")),
    }
}

/// Compresses a picture.
///
/// # Errors
/// Fails if the sizes do not match the data.
pub fn encode_frame(frame: &Frame) -> io::Result<Vec<u8>> {
    let pixels = frame.width as usize * frame.height as usize;
    if frame.rgba.len() != pixels * 4 {
        return Err(invalid("picture size mismatch"));
    }
    let rgb: Vec<u8> =
        frame.rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
        .encode(&rgb, frame.width, frame.height, ExtendedColorType::Rgb8)
        .map_err(io::Error::other)?;
    Ok(out)
}

/// Decompresses a picture from the other side.
///
/// # Errors
/// Fails if it is not a JPEG of a sensible size.
pub fn decode_frame(jpeg: &[u8]) -> io::Result<Frame> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_FRAME_SIDE);
    limits.max_image_height = Some(MAX_FRAME_SIDE);
    let mut reader = ImageReader::with_format(Cursor::new(jpeg), ImageFormat::Jpeg);
    reader.limits(limits);
    let image = reader.decode().map_err(io::Error::other)?.into_rgba8();
    Ok(Frame { width: image.width(), height: image.height(), rgba: image.into_raw() })
}

/// Thins 48 kHz stereo out to [`AUDIO_RATE`], averaging what it drops so that the highest
/// tones fade instead of folding back as noise.
#[derive(Debug, Default)]
pub struct Downsampler {
    sum: [i32; 2],
    count: u32,
}

impl Downsampler {
    const FACTOR: u32 = INPUT_AUDIO_RATE / AUDIO_RATE;

    /// Appends the samples `input` adds up to.
    pub fn push(&mut self, input: &[i16], out: &mut Vec<i16>) {
        for frame in input.as_chunks::<2>().0 {
            self.sum[0] += i32::from(frame[0]);
            self.sum[1] += i32::from(frame[1]);
            self.count += 1;
            if self.count == Self::FACTOR {
                #[allow(clippy::cast_possible_truncation)]
                out.extend(self.sum.map(|sum| (sum / Self::FACTOR as i32) as i16));
                self.sum = [0; 2];
                self.count = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(message: &Message) -> Message {
        let mut bytes = Vec::new();
        write_message(&mut bytes, message).unwrap();
        read_message(&mut bytes.as_slice()).unwrap()
    }

    #[test]
    fn messages_survive_the_wire() {
        let pad = GamePadState {
            held: VpadButtons::A | VpadButtons::DOWN,
            left_stick: Stick { x: -0.5, y: 1.0 },
            right_stick: Stick { x: 0.25, y: 0.0 },
            touch: None,
        };
        for message in [
            Message::Hello,
            Message::Ping(7),
            Message::Pong(u64::MAX),
            Message::Input(pad),
            Message::Video(vec![1, 2, 3]),
            Message::Audio(vec![-1, 2, i16::MIN, i16::MAX]),
            Message::Challenge([7; CHALLENGE_SIZE]),
            Message::Answer([9; ANSWER_SIZE]),
            Message::Welcome,
            Message::Refused(Refusal::Network),
            Message::Refused(Refusal::WrongPassword),
        ] {
            assert_eq!(round_trip(&message), message);
        }
    }

    #[test]
    fn answers_depend_on_both_challenge_and_password() {
        let (one, two) = ([1; CHALLENGE_SIZE], [2; CHALLENGE_SIZE]);
        assert!(answers_match(&answer(&one, "secret"), &answer(&one, "secret")));
        assert!(!answers_match(&answer(&one, "secret"), &answer(&one, "Secret")));
        assert!(!answers_match(&answer(&one, "secret"), &answer(&two, "secret")));
        assert_ne!(new_challenge().unwrap(), new_challenge().unwrap());
    }

    #[test]
    fn nonsense_is_refused() {
        // Longer than any real message: must fail before allocating that much.
        let huge = [tag::VIDEO, 0xFF, 0xFF, 0xFF, 0xFF];
        assert!(read_message(&mut huge.as_slice()).is_err());
        let unknown = [200, 0, 0, 0, 0];
        assert!(read_message(&mut unknown.as_slice()).is_err());
        let other_version = [tag::HELLO, 0, 0, 0, 2, 0xFF, 0xFF];
        assert!(read_message(&mut other_version.as_slice()).is_err());
        let short_input = [tag::INPUT, 0, 0, 0, 1, 0];
        assert!(read_message(&mut short_input.as_slice()).is_err());
    }

    #[test]
    fn stick_values_are_sane_whatever_arrives() {
        let mut payload = vec![0; 20];
        payload[4..8].copy_from_slice(&f32::NAN.to_be_bytes());
        payload[8..12].copy_from_slice(&5.0_f32.to_be_bytes());
        let pad = decode_input(&payload).unwrap();
        assert_eq!((pad.left_stick.x, pad.left_stick.y), (0.0, 1.0));
    }

    #[test]
    fn pictures_keep_their_size_and_rough_color() {
        let frame = Frame { width: 32, height: 16, rgba: [200, 40, 10, 255].repeat(32 * 16) };
        let decoded = decode_frame(&encode_frame(&frame).unwrap()).unwrap();
        assert_eq!((decoded.width, decoded.height), (32, 16));
        assert!(decoded.rgba[0].abs_diff(200) < 8 && decoded.rgba[1].abs_diff(40) < 8);
        assert!(decode_frame(b"not a picture").is_err());
    }

    #[test]
    fn three_frames_of_sound_become_their_average() {
        let mut down = Downsampler::default();
        let mut out = Vec::new();
        down.push(&[3, 30, 6, 60], &mut out);
        assert!(out.is_empty(), "two frames are not enough yet");
        down.push(&[9, 90, 100, 100], &mut out);
        assert_eq!(out, [6, 60]);
    }
}
