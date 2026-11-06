//! Pictures and sound as they travel: JPEG frames and 16 kHz samples.
use super::*;

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

/// Decompresses a picture of `display` from the other side.
///
/// # Errors
/// Fails if it is not a JPEG of a sensible size.
pub fn decode_frame(display: Display, jpeg: &[u8]) -> io::Result<Frame> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_FRAME_SIDE);
    limits.max_image_height = Some(MAX_FRAME_SIDE);
    let mut reader = ImageReader::with_format(Cursor::new(jpeg), ImageFormat::Jpeg);
    reader.limits(limits);
    let image = reader.decode().map_err(io::Error::other)?.into_rgba8();
    Ok(Frame { display, width: image.width(), height: image.height(), rgba: image.into_raw() })
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
