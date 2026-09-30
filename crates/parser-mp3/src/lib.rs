//! MPEG-1, MPEG-2 and MPEG-2.5 Layer III elementary streams (`A_MPEG/L3`).
//!
//! - ID3v2, ID3v1, APEv2 and Lyrics3v2 tags are skipped, and so is zero
//!   padding before the first frame or after the last complete frame
//!   (tagger padding). Zeros between frames are an error.
//! - Every frame is copied as is (header included), all frames are random
//!   access points.
//! - A leading Xing/Info or VBRI frame is not audio and is skipped. When it
//!   carries a LAME tag with a valid CRC, the encoder delay plus the decoder
//!   delay (529 samples) becomes `codec_delay_ns` and moves the timeline
//!   back, and the encoder padding minus 529 becomes `discard_padding_ns` of
//!   the last frames (the padding can be longer than one frame). Without a
//!   valid LAME tag no delay is assumed.
//! - Free-format bitrate, other layers and parameter changes are rejected.

pub mod tags;

use vtj::cli::{ParamSpec, ParseError};
use vtj::*;

/// Decoder delay of the MP3 synthesis filterbank, in samples.
pub const DECODER_DELAY: u32 = 529;

/// Frames kept back before writing, so the end padding can be spread over
/// them. The LAME padding field has 12 bits (at most 4095 samples), which
/// 8 frames of 576 samples always cover.
const HOLD_FRAMES: usize = 8;

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

/// A LAME tag's gapless fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LameTag {
    pub delay: u32,
    pub padding: u32,
    /// The tag CRC matches (see [`crc16`]).
    pub crc_ok: bool,
}

/// What a leading Xing/Info or VBRI frame says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InfoFrame {
    /// Audio frame count declared by a Xing/Info header.
    pub frames: Option<u32>,
    pub lame: Option<LameTag>,
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
        let crc_ok = crc16(&frame[..p + 34]) == stored || crc16(&lame_crc_window(frame, p + 34)) == stored;
        let delay = ((lame[21] as u32) << 4) | (lame[22] as u32 >> 4);
        let padding = (((lame[22] & 0x0f) as u32) << 8) | lame[23] as u32;
        info.lame = Some(LameTag { delay, padding, crc_ok });
    }
    Some(info)
}

const GAPLESS: ParamSpec = ParamSpec::choice(
    "gapless",
    &["auto", "off"],
    "auto: delay and padding from a LAME tag or the overrides below; off: none at all",
)
.default("auto");
const LAME_CRC: ParamSpec = ParamSpec::choice(
    "lame_crc",
    &["verify", "ignore"],
    "ignore: use a LAME tag's delay and padding even if its CRC does not match",
)
.default("verify");
const ENCODER_DELAY: ParamSpec =
    ParamSpec::int("encoder_delay", 0, 65535, "encoder delay in samples, instead of the LAME tag's (0 without tag)")
        .default("from the LAME tag");
const ENCODER_PADDING: ParamSpec = ParamSpec::int(
    "encoder_padding",
    0,
    65535,
    "encoder padding in samples, as in the LAME tag (includes the decoder delay)",
)
.default("from the LAME tag");
const DECODER_DELAY_PARAM: ParamSpec =
    ParamSpec::int("decoder_delay", 0, 4096, "decoder delay added to the encoder delay").default("529");
const XING_COUNT_MISMATCH: ParamSpec = ParamSpec::choice(
    "xing_count_mismatch",
    &["keep-delay", "use-padding", "ignore-tag"],
    "when the Xing frame count differs from the frames found: keep the delay but drop the padding, use both, or ignore the tag",
)
.default("keep-delay");
const INFO_FRAME: ParamSpec = ParamSpec::choice(
    "info_frame",
    &["skip", "keep"],
    "keep: write a Xing/Info/VBRI frame as a unit (it decodes to silence) and add its duration to the delay",
)
.default("skip");
const JUNK: ParamSpec = ParamSpec::choice(
    "junk",
    &["error", "resync"],
    "resync: skip bytes that are not a frame up to the next frame consistent with the stream (drops those bytes)",
)
.default("error");
const ZERO_PADDING: ParamSpec = ParamSpec::choice(
    "zero_padding",
    &["skip", "error"],
    "zero bytes before the first frame or after the last one: skip them or fail",
)
.default("skip");
const INCOMPLETE_END: ParamSpec = ParamSpec::choice(
    "incomplete_end",
    &["error", "drop"],
    "a last frame cut short by the end of the audio: fail, or drop its bytes",
)
.default("error");
const BYTE_RANGE: ParamSpec =
    ParamSpec::string("byte_range", "parse exactly the bytes A:B (or A: to the end), ignoring tag detection")
        .default("between the leading and trailing tags");

const PARAMS: &[ParamSpec] = &[
    GAPLESS,
    LAME_CRC,
    ENCODER_DELAY,
    ENCODER_PADDING,
    DECODER_DELAY_PARAM,
    XING_COUNT_MISMATCH,
    INFO_FRAME,
    JUNK,
    ZERO_PADDING,
    INCOMPLETE_END,
    BYTE_RANGE,
];

/// Parses `A:B` or `A:`.
pub fn parse_byte_range(s: &str) -> Result<(u64, Option<u64>), String> {
    let (a, b) = s.split_once(':').ok_or_else(|| format!("--byte-range: \"{s}\" is not A:B or A:"))?;
    let a: u64 = a.parse().map_err(|_| format!("--byte-range: invalid start \"{a}\""))?;
    let b: Option<u64> =
        if b.is_empty() { None } else { Some(b.parse().map_err(|_| format!("--byte-range: invalid end \"{b}\""))?) };
    if b.is_some_and(|b| b <= a) {
        return Err(format!("--byte-range: end must be greater than start in \"{s}\""));
    }
    Ok((a, b))
}

/// The parameters in effect for one run.
#[derive(Debug, Clone)]
struct Config {
    gapless: bool,
    lame_crc_ignore: bool,
    encoder_delay: Option<u32>,
    encoder_padding: Option<u32>,
    decoder_delay: u32,
    xing_mismatch: String,
    keep_info_frame: bool,
    resync: bool,
    skip_zeros: bool,
    drop_incomplete_end: bool,
    byte_range: Option<(u64, Option<u64>)>,
}

impl Config {
    fn from(ctx: &Context<'_>) -> Result<Self, ParseError> {
        let s = |n: &str| ctx.param_str(n).map(str::to_string);
        let int = |n: &str| ctx.param_int(n).map(|v| v as u32);
        Ok(Config {
            gapless: s("gapless").as_deref() != Some("off"),
            lame_crc_ignore: s("lame_crc").as_deref() == Some("ignore"),
            encoder_delay: int("encoder_delay"),
            encoder_padding: int("encoder_padding"),
            decoder_delay: int("decoder_delay").unwrap_or(DECODER_DELAY),
            xing_mismatch: s("xing_count_mismatch").unwrap_or_else(|| "keep-delay".into()),
            keep_info_frame: s("info_frame").as_deref() == Some("keep"),
            resync: s("junk").as_deref() == Some("resync"),
            skip_zeros: s("zero_padding").as_deref() != Some("error"),
            drop_incomplete_end: s("incomplete_end").as_deref() == Some("drop"),
            byte_range: s("byte_range").map(|r| parse_byte_range(&r)).transpose().map_err(ParseError::invalid)?,
        })
    }
}

fn header_error(e: HeaderError, pos: u64) -> ParseError {
    match e {
        HeaderError::NoSync => ParseError::invalid(format!("no frame sync at byte {pos}")),
        HeaderError::Reserved(what) => ParseError::invalid(format!("reserved {what} at byte {pos}")),
        HeaderError::Layer(l) => {
            ParseError::new(ErrorCode::UnsupportedCodecVariant, format!("MPEG audio layer {l} at byte {pos}"))
        }
        HeaderError::FreeFormat => ParseError::unsupported(format!("free-format bitrate at byte {pos}")),
    }
}

fn same_stream(a: &FrameHeader, b: &FrameHeader) -> bool {
    (a.version, a.sample_rate, a.channels) == (b.version, b.sample_rate, b.channels)
}

/// The next offset after `from` where a frame starts that is consistent
/// with `like` (if known) and is followed by another such frame or by `end`.
fn find_sync(
    src: &mut vtj::source::SourceFile,
    from: u64,
    end: u64,
    like: Option<&FrameHeader>,
) -> Result<Option<u64>, ParseError> {
    let fits = |h: &FrameHeader| like.is_none_or(|l| same_stream(l, h));
    let header_at = |src: &mut vtj::source::SourceFile, at: u64| -> Result<Option<FrameHeader>, ParseError> {
        if end.saturating_sub(at) < 4 {
            return Ok(None);
        }
        let mut b = [0u8; 4];
        src.read_at(at, &mut b)?;
        Ok(parse_header(b).ok())
    };
    const CHUNK: u64 = 64 * 1024;
    let mut base = from;
    while base < end {
        let n = CHUNK.min(end - base);
        let mut buf = vec![0u8; n as usize];
        src.read_at(base, &mut buf)?;
        for (i, &b) in buf.iter().enumerate() {
            if b != 0xff {
                continue;
            }
            let at = base + i as u64;
            let Some(h) = header_at(src, at)?.filter(|h| fits(h)) else { continue };
            let next = at + h.length;
            if next == end || header_at(src, next)?.is_some_and(|h2| same_stream(&h, &h2)) {
                return Ok(Some(at));
            }
        }
        base += n;
    }
    Ok(None)
}

pub struct Mp3;

impl Parser for Mp3 {
    fn name(&self) -> &'static str {
        "vmkv-parser-mp3"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn params(&self) -> &'static [ParamSpec] {
        PARAMS
    }

    fn check_params(&self, p: &std::collections::BTreeMap<String, ParamValue>) -> Result<(), String> {
        let off = matches!(p.get("gapless"), Some(ParamValue::String(v)) if v == "off");
        if off {
            for n in ["lame_crc", "encoder_delay", "encoder_padding", "decoder_delay", "xing_count_mismatch"] {
                if p.contains_key(n) {
                    return Err(format!("--gapless off conflicts with --{}", n.replace('_', "-")));
                }
            }
        }
        if let Some(ParamValue::String(r)) = p.get("byte_range") {
            parse_byte_range(r)?;
            if p.contains_key("zero_padding") {
                return Err("--byte-range conflicts with --zero-padding: the range is parsed exactly".into());
            }
        }
        Ok(())
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let cfg = Config::from(ctx)?;
        let range = match cfg.byte_range {
            Some((a, b)) => {
                let size = ctx.source(0).size();
                let b = b.unwrap_or(size);
                if b > size {
                    return Err(ParseError::truncated(format!(
                        "--byte-range end {b} is past the end of the file ({size})"
                    )));
                }
                tags::AudioRange { start: a, end: b }
            }
            None => {
                let tagged = tags::audio_range(ctx.source(0))?;
                if cfg.skip_zeros {
                    tags::skip_leading_zeros(ctx.source(0), tagged)?
                } else {
                    tagged
                }
            }
        };
        let skip_trailing_zeros = cfg.skip_zeros && cfg.byte_range.is_none();
        let mut pos = range.start;
        let mut first: Option<FrameHeader> = None;
        let mut info: Option<InfoFrame> = None;
        let mut info_frame_samples: u32 = 0;
        let mut raw_units: Vec<(u64, u64, u32)> = Vec::new();
        let mut count: u64 = 0;

        while pos < range.end {
            let mut lead = [0u8; 1];
            ctx.source(0).read_at(pos, &mut lead)?;
            if lead[0] == 0 && count > 0 && skip_trailing_zeros && tags::is_zero_padding(ctx.source(0), pos, range.end)?
            {
                break;
            }
            let parsed = if range.end - pos < 4 {
                Err(None)
            } else {
                let mut hb = [0u8; 4];
                ctx.source(0).read_at(pos, &mut hb)?;
                parse_header(hb).map_err(Some)
            };
            let h = match parsed {
                Ok(h) if first.as_ref().is_none_or(|f| same_stream(f, &h)) || !cfg.resync => h,
                other => {
                    if let (Ok(h), Some(f)) = (&other, &first) {
                        let next = pos + h.length;
                        let mut nb = [0u8; 4];
                        let followed = next == range.end
                            || (range.end.saturating_sub(next) >= 4
                                && ctx.source(0).read_at(next, &mut nb).is_ok()
                                && parse_header(nb).is_ok_and(|n| same_stream(h, &n)));
                        if followed {
                            return Err(ParseError::new(
                                ErrorCode::InconsistentTrackParameters,
                                format!(
                                    "frame {count} changes from {} Hz {} ch to {} Hz {} ch",
                                    f.sample_rate, f.channels, h.sample_rate, h.channels
                                ),
                            ));
                        }
                    }
                    if cfg.resync {
                        match find_sync(ctx.source(0), pos + 1, range.end, first.as_ref())? {
                            Some(next) => {
                                pos = next;
                                continue;
                            }
                            None if count > 0 => break,
                            None => return Err(ParseError::invalid("no audio frames")),
                        }
                    }
                    match other {
                        Err(None) if count > 0 && cfg.drop_incomplete_end => break,
                        Err(None) => {
                            return Err(ParseError::truncated(format!(
                                "frame {count} header cut at byte {}",
                                range.end
                            )))
                        }
                        Err(Some(e)) => return Err(header_error(e, pos)),
                        Ok(h) => h,
                    }
                }
            };
            if h.length > range.end - pos {
                if count > 0 && cfg.drop_incomplete_end {
                    break;
                }
                return Err(ParseError::truncated(format!("frame {count} cut at byte {}", range.end)));
            }

            if first.is_none() && info.is_none() {
                let mut buf = vec![0u8; h.length.min(256) as usize];
                ctx.source(0).read_at(pos, &mut buf)?;
                if let Some(i) = parse_info_frame(&h, &buf) {
                    info = Some(i);
                    if cfg.keep_info_frame {
                        info_frame_samples = h.samples;
                        raw_units.push((pos, h.length, h.samples));
                    }
                    pos += h.length;
                    continue;
                }
            }

            match first {
                None => first = Some(h),
                Some(f) if !same_stream(&f, &h) => {
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
            raw_units.push((pos, h.length, h.samples));
            count += 1;
            pos += h.length;
        }

        let Some(first) = first else {
            return Err(ParseError::invalid("no audio frames"));
        };
        let rate = Rational::new(first.sample_rate as i64, 1);

        let tag = info.and_then(|i| i.lame).filter(|t| t.crc_ok || cfg.lame_crc_ignore).filter(|_| cfg.gapless);
        let count_matches = info.and_then(|i| i.frames).is_none_or(|n| n as u64 == count);
        let (tag_delay_ok, tag_padding_ok) = match (count_matches, cfg.xing_mismatch.as_str()) {
            (true, _) | (false, "use-padding") => (true, true),
            (false, "ignore-tag") => (false, false),
            (false, _) => (true, false),
        };
        let delay = cfg.encoder_delay.or(tag.filter(|_| tag_delay_ok).map(|t| t.delay));
        let padding = cfg.encoder_padding.or(tag.filter(|_| tag_padding_ok).map(|t| t.padding));
        let gapless = cfg.gapless && (delay.is_some() || padding.is_some());
        let codec_delay = if gapless { delay.unwrap_or(0) + cfg.decoder_delay + info_frame_samples } else { 0 };

        let mut timeline = Timeline::new(rate, -(codec_delay as i128))?;
        let mut pending: std::collections::VecDeque<Unit> = std::collections::VecDeque::new();
        let flags = Flags::NONE.with(Flag::RandomAccess);
        for (off, len, samples) in raw_units {
            let (pts, dur) = timeline.advance(samples as i128)?;
            pending.push_back(Unit::new(pts, dur, flags, vec![Chunk::src(0, off, len)]));
            if pending.len() > HOLD_FRAMES {
                ctx.emit(&pending.pop_front().expect("non-empty"))?;
            }
        }
        if let (true, Some(padding)) = (gapless, padding) {
            let discard = padding.saturating_sub(cfg.decoder_delay) as i128;
            if discard > 0 {
                let audible_end = ticks_to_ns(timeline.position() - discard, rate)?;
                if !trim_end(pending.make_contiguous(), audible_end) {
                    return Err(ParseError::new(
                        ErrorCode::UnrepresentableInVmkv,
                        format!("an encoder padding of {padding} samples exceeds the stream"),
                    ));
                }
            }
        }
        for u in &pending {
            ctx.emit(u)?;
        }

        let mut track = Track::new(TrackType::Audio, "A_MPEG/L3");
        if gapless {
            track.codec_delay_ns = Some(ticks_to_ns(codec_delay as i128, rate)?);
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
