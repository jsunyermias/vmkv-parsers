//! MPEG-1, MPEG-2 and MPEG-2.5 Layer III elementary streams (`A_MPEG/L3`).
//!
//! - ID3v2, ID3v1, APEv2 and Lyrics3v2 tags are skipped.
//! - Every frame is copied as is (header included), all frames are random
//!   access points.
//! - A leading Xing/Info or VBRI frame is not audio and is skipped. When it
//!   carries a LAME tag with a valid CRC, the encoder delay plus the decoder
//!   delay (529 samples) becomes `codec_delay_ns` and moves the timeline
//!   back, and the encoder padding minus 529 becomes `discard_padding_ns` of
//!   the last frame. Without a valid LAME tag no delay is assumed.
//! - Free-format bitrate, other layers and parameter changes are rejected.

use vtj::cli::ParseError;
use vtj::*;

/// Decoder delay of the MP3 synthesis filterbank, in samples.
pub const DECODER_DELAY: u32 = 529;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    Mpeg1,
    Mpeg2,
    Mpeg25,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub version: Version,
    pub protected: bool,
    pub bitrate_kbps: u32,
    pub sample_rate: u32,
    pub padding: bool,
    pub channels: u8,
    /// Frame length in bytes, header included.
    pub length: u64,
    /// PCM samples per channel.
    pub samples: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderError {
    NoSync,
    Reserved(&'static str),
    Layer(u8),
    FreeFormat,
}

const BITRATES_V1: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
const BITRATES_V2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

/// Parses a 4-byte Layer III frame header.
pub fn parse_header(b: [u8; 4]) -> Result<FrameHeader, HeaderError> {
    if b[0] != 0xff || b[1] & 0xe0 != 0xe0 {
        return Err(HeaderError::NoSync);
    }
    let version = match (b[1] >> 3) & 3 {
        0 => Version::Mpeg25,
        2 => Version::Mpeg2,
        3 => Version::Mpeg1,
        _ => return Err(HeaderError::Reserved("MPEG version")),
    };
    match (b[1] >> 1) & 3 {
        1 => {}
        0 => return Err(HeaderError::Reserved("layer")),
        l => return Err(HeaderError::Layer(4 - l)),
    }
    let protected = b[1] & 1 == 0;
    let bitrate_index = (b[2] >> 4) as usize;
    let bitrate_kbps = match bitrate_index {
        0 => return Err(HeaderError::FreeFormat),
        15 => return Err(HeaderError::Reserved("bitrate index")),
        i if version == Version::Mpeg1 => BITRATES_V1[i],
        i => BITRATES_V2[i],
    };
    let base = match (b[2] >> 2) & 3 {
        0 => 44100,
        1 => 48000,
        2 => 32000,
        _ => return Err(HeaderError::Reserved("sampling frequency")),
    };
    let sample_rate = match version {
        Version::Mpeg1 => base,
        Version::Mpeg2 => base / 2,
        Version::Mpeg25 => base / 4,
    };
    if b[3] & 3 == 2 {
        return Err(HeaderError::Reserved("emphasis"));
    }
    let padding = b[2] & 2 != 0;
    let channels = if b[3] >> 6 == 3 { 1 } else { 2 };
    let (coef, samples) = if version == Version::Mpeg1 { (144_000, 1152) } else { (72_000, 576) };
    let length = (coef * bitrate_kbps / sample_rate + padding as u32) as u64;
    Ok(FrameHeader { version, protected, bitrate_kbps, sample_rate, padding, channels, length, samples })
}

/// CRC-16/ARC (polynomial 0x8005 reflected, initial value 0), as used by the LAME tag.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xa001 } else { crc >> 1 };
        }
    }
    crc
}

/// The 190 bytes that encoders such as FFmpeg cover with the LAME tag CRC:
/// the start of the frame, zero-filled past its end, with the CRC field
/// itself zeroed. For MPEG-1 stereo frames this equals the bytes before the
/// CRC field.
fn lame_crc_window(frame: &[u8], crc_at: usize) -> [u8; 190] {
    let mut w = [0u8; 190];
    let n = frame.len().min(190);
    w[..n].copy_from_slice(&frame[..n]);
    if let Some(field) = w.get_mut(crc_at..(crc_at + 2).min(190)) {
        field.fill(0);
    }
    w
}

/// What a leading Xing/Info or VBRI frame says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InfoFrame {
    /// Audio frame count declared by a Xing/Info header.
    pub frames: Option<u32>,
    /// `(encoder delay, encoder padding)` from a LAME tag whose CRC matches.
    pub lame: Option<(u32, u32)>,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes(b[..4].try_into().expect("4 bytes"))
}

/// Recognizes a Xing/Info or VBRI frame from its first bytes.
pub fn parse_info_frame(h: &FrameHeader, frame: &[u8]) -> Option<InfoFrame> {
    let side = match (h.version, h.channels) {
        (Version::Mpeg1, 1) => 17,
        (Version::Mpeg1, _) => 32,
        (_, 1) => 9,
        _ => 17,
    };
    let xo = 4 + if h.protected { 2 } else { 0 } + side;
    if frame.get(36..40) == Some(b"VBRI") {
        return Some(InfoFrame::default());
    }
    let tag = frame.get(xo..xo + 8)?;
    if &tag[..4] != b"Xing" && &tag[..4] != b"Info" {
        return None;
    }
    let flags = be32(&tag[4..]);
    let mut p = xo + 8;
    let mut info = InfoFrame::default();
    if flags & 1 != 0 {
        info.frames = frame.get(p..p + 4).map(be32);
        p += 4;
    }
    for (bit, size) in [(2, 4), (4, 100), (8, 4)] {
        if flags & bit != 0 {
            p += size;
        }
    }
    if let Some(lame) = frame.get(p..p + 36) {
        let stored = u16::from_be_bytes([lame[34], lame[35]]);
        if crc16(&frame[..p + 34]) == stored || crc16(&lame_crc_window(frame, p + 34)) == stored {
            let delay = ((lame[21] as u32) << 4) | (lame[22] as u32 >> 4);
            let padding = (((lame[22] & 0x0f) as u32) << 8) | lame[23] as u32;
            info.lame = Some((delay, padding));
        }
    }
    Some(info)
}

pub struct Mp3;

impl Parser for Mp3 {
    fn name(&self) -> &'static str {
        "vmkv-parser-mp3"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let range = tags::audio_range(ctx.source(0))?;
        let mut pos = range.start;
        let mut first: Option<FrameHeader> = None;
        let mut info: Option<InfoFrame> = None;
        let mut timeline: Option<Timeline> = None;
        let mut pending: Option<(Unit, u32)> = None;
        let mut count: u64 = 0;

        while pos < range.end {
            if range.end - pos < 4 {
                return Err(ParseError::truncated(format!("frame {count} header cut at byte {}", range.end)));
            }
            let mut hb = [0u8; 4];
            ctx.source(0).read_at(pos, &mut hb)?;
            let h = parse_header(hb).map_err(|e| match e {
                HeaderError::NoSync => ParseError::invalid(format!("no frame sync at byte {pos}")),
                HeaderError::Reserved(what) => ParseError::invalid(format!("reserved {what} at byte {pos}")),
                HeaderError::Layer(l) => {
                    ParseError::new(ErrorCode::UnsupportedCodecVariant, format!("MPEG audio layer {l} at byte {pos}"))
                }
                HeaderError::FreeFormat => ParseError::unsupported(format!("free-format bitrate at byte {pos}")),
            })?;
            if h.length > range.end - pos {
                return Err(ParseError::truncated(format!("frame {count} cut at byte {}", range.end)));
            }

            if first.is_none() && info.is_none() {
                let mut buf = vec![0u8; h.length.min(256) as usize];
                ctx.source(0).read_at(pos, &mut buf)?;
                if let Some(i) = parse_info_frame(&h, &buf) {
                    info = Some(i);
                    pos += h.length;
                    continue;
                }
            }

            match first {
                None => {
                    first = Some(h);
                    let delay = info.and_then(|i| i.lame).map(|(d, _)| d + DECODER_DELAY).unwrap_or(0);
                    timeline = Some(Timeline::new(Rational::new(h.sample_rate as i64, 1), -(delay as i128))?);
                }
                Some(f) if (f.version, f.sample_rate, f.channels) != (h.version, h.sample_rate, h.channels) => {
                    return Err(ParseError::new(
                        ErrorCode::InconsistentTrackParameters,
                        format!(
                            "frame {count} changes from {} Hz {} ch to {} Hz {} ch",
                            f.sample_rate, f.channels, h.sample_rate, h.channels
                        ),
                    ));
                }
                Some(_) => {}
            }

            let (pts, dur) = timeline.as_mut().expect("set with the first frame").advance(h.samples as i128)?;
            let unit = Unit::new(pts, dur, Flags::NONE.with(Flag::RandomAccess), vec![Chunk::src(0, pos, h.length)]);
            if let Some((prev, _)) = pending.replace((unit, h.samples)) {
                ctx.emit(&prev)?;
            }
            count += 1;
            pos += h.length;
        }

        let Some(first) = first else {
            return Err(ParseError::invalid("no audio frames"));
        };
        let rate = Rational::new(first.sample_rate as i64, 1);
        let lame = info.and_then(|i| i.lame);
        let frames_match = info.and_then(|i| i.frames).is_none_or(|n| n as u64 == count);
        if let Some((mut last, samples)) = pending {
            if let (Some((_, padding)), true) = (lame, frames_match) {
                let discard = padding.saturating_sub(DECODER_DELAY);
                if discard > samples {
                    return Err(ParseError::new(
                        ErrorCode::UnrepresentableInVmkv,
                        format!("LAME padding of {padding} samples exceeds the last frame"),
                    ));
                }
                if discard > 0 {
                    last.discard_padding_ns = Some(ticks_to_ns(discard as i128, rate)?);
                }
            }
            ctx.emit(&last)?;
        }

        let mut track = Track::new(TrackType::Audio, "A_MPEG/L3");
        if let Some((delay, _)) = lame {
            track.codec_delay_ns = Some(ticks_to_ns((delay + DECODER_DELAY) as i128, rate)?);
        }
        track.audio = Some(Audio::new(rate, first.channels as u64));
        Ok(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_tables() {
        let h = parse_header([0xff, 0xfb, 0x90, 0x64]).unwrap();
        assert_eq!(
            (h.version, h.bitrate_kbps, h.sample_rate, h.channels, h.length, h.samples),
            (Version::Mpeg1, 128, 44100, 2, 417, 1152)
        );
        let h = parse_header([0xff, 0xfb, 0x92, 0x64]).unwrap();
        assert_eq!(h.length, 418);
        let h = parse_header([0xff, 0xf3, 0x80, 0xc4]).unwrap();
        assert_eq!(
            (h.version, h.bitrate_kbps, h.sample_rate, h.channels, h.length, h.samples),
            (Version::Mpeg2, 64, 22050, 1, 208, 576)
        );
        let h = parse_header([0xff, 0xe3, 0x10, 0xc4]).unwrap();
        assert_eq!((h.version, h.sample_rate, h.samples), (Version::Mpeg25, 11025, 576));
    }

    #[test]
    fn header_errors() {
        assert_eq!(parse_header([0x00, 0xfb, 0x90, 0x64]), Err(HeaderError::NoSync));
        assert_eq!(parse_header([0xff, 0xeb, 0x90, 0x64]), Err(HeaderError::Reserved("MPEG version")));
        assert_eq!(parse_header([0xff, 0xfd, 0x90, 0x64]), Err(HeaderError::Layer(2)));
        assert_eq!(parse_header([0xff, 0xff, 0x90, 0x64]), Err(HeaderError::Layer(1)));
        assert_eq!(parse_header([0xff, 0xf9, 0x90, 0x64]), Err(HeaderError::Reserved("layer")));
        assert_eq!(parse_header([0xff, 0xfb, 0x00, 0x64]), Err(HeaderError::FreeFormat));
        assert_eq!(parse_header([0xff, 0xfb, 0xf0, 0x64]), Err(HeaderError::Reserved("bitrate index")));
        assert_eq!(parse_header([0xff, 0xfb, 0x9c, 0x64]), Err(HeaderError::Reserved("sampling frequency")));
        assert_eq!(parse_header([0xff, 0xfb, 0x90, 0x66]), Err(HeaderError::Reserved("emphasis")));
    }

    #[test]
    fn crc16_arc() {
        assert_eq!(crc16(b"123456789"), 0xbb3d);
    }
}
