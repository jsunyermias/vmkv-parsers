//! MPEG-1, MPEG-2 and MPEG-2.5 audio Layer II (`A_MPEG/L2`).
//!
//! - ID3v2, ID3v1, APEv2 and Lyrics3v2 tags are skipped (own copy of the
//!   tag skipper, decision 30).
//! - Each unit is one whole frame, header included, lasting 1152 samples.
//!   Layer II keeps no state between frames, so every frame is a random
//!   access point.
//! - Rejected: Layer I (no real sample to verify against) and Layer III
//!   (`vmkv-parser-mp3`) as `UNSUPPORTED_CODEC_VARIANT`; free-format
//!   bitrate as `UNSUPPORTED_FEATURE`; and a change of version, sample rate
//!   or channel count (decision 66).

pub mod tags;

use vtj::cli::ParseError;
use vtj::*;

/// Kbit/s per bitrate index for Layer II: MPEG-1, then MPEG-2/2.5 (LSF).
const BITRATES_V1: [u32; 15] = [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384];
const BITRATES_LSF: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];
const RATES_V1: [u32; 3] = [44100, 48000, 32000];
const SAMPLES: u32 = 1152;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    Mpeg1,
    Mpeg2,
    Mpeg25,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub version: Version,
    pub sample_rate: u32,
    pub channels: u8,
    pub frame_len: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderError {
    NoSync,
    Layer(u8),
    FreeFormat,
    Bitrate,
    SampleRate,
}

pub fn parse_header(b: [u8; 4]) -> Result<Header, HeaderError> {
    if b[0] != 0xff || b[1] & 0xe0 != 0xe0 {
        return Err(HeaderError::NoSync);
    }
    let version = match (b[1] >> 3) & 3 {
        0 => Version::Mpeg25,
        2 => Version::Mpeg2,
        3 => Version::Mpeg1,
        _ => return Err(HeaderError::NoSync),
    };
    match (b[1] >> 1) & 3 {
        2 => {}
        0 => return Err(HeaderError::NoSync),
        l => return Err(HeaderError::Layer(4 - l)),
    }
    let index = (b[2] >> 4) as usize;
    if index == 0 {
        return Err(HeaderError::FreeFormat);
    }
    if index == 15 {
        return Err(HeaderError::Bitrate);
    }
    let sr_index = ((b[2] >> 2) & 3) as usize;
    if sr_index == 3 {
        return Err(HeaderError::SampleRate);
    }
    let sample_rate = match version {
        Version::Mpeg1 => RATES_V1[sr_index],
        Version::Mpeg2 => RATES_V1[sr_index] / 2,
        Version::Mpeg25 => RATES_V1[sr_index] / 4,
    };
    let kbps = if version == Version::Mpeg1 { BITRATES_V1[index] } else { BITRATES_LSF[index] };
    let padding = ((b[2] >> 1) & 1) as u64;
    let frame_len = 144 * kbps as u64 * 1000 / sample_rate as u64 + padding;
    let channels = if b[3] >> 6 == 3 { 1 } else { 2 };
    Ok(Header { version, sample_rate, channels, frame_len })
}

pub struct Mp2;

impl Parser for Mp2 {
    fn name(&self) -> &'static str {
        "vmkv-parser-mp2"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let range = tags::audio_range(ctx.source(0))?;
        let mut pos = range.start;
        let mut first: Option<Header> = None;
        let mut timeline: Option<Timeline> = None;
        let mut count = 0u64;

        while pos < range.end {
            if range.end - pos < 4 {
                return Err(ParseError::truncated(format!("frame {count} header cut at byte {}", range.end)));
            }
            let mut hb = [0u8; 4];
            ctx.source(0).read_at(pos, &mut hb)?;
            let at = format!("frame {count} at byte {pos}");
            let h = parse_header(hb).map_err(|e| match e {
                HeaderError::NoSync => ParseError::invalid(format!("no MPEG audio sync at byte {pos}")),
                HeaderError::Layer(1) => {
                    ParseError::new(ErrorCode::UnsupportedCodecVariant, format!("{at} is Layer I"))
                }
                HeaderError::Layer(l) => ParseError::new(
                    ErrorCode::UnsupportedCodecVariant,
                    format!("{at} is Layer {}; use vmkv-parser-mp3", ["", "I", "II", "III"][l as usize]),
                ),
                HeaderError::FreeFormat => ParseError::unsupported(format!("{at} uses the free-format bitrate")),
                HeaderError::Bitrate => ParseError::invalid(format!("{at} has the invalid bitrate index 15")),
                HeaderError::SampleRate => ParseError::invalid(format!("{at} has the reserved sample rate index")),
            })?;
            if h.frame_len > range.end - pos {
                return Err(ParseError::truncated(format!("frame {count} cut at byte {}", range.end)));
            }
            match first {
                None => {
                    first = Some(h);
                    timeline = Some(Timeline::new(Rational::new(h.sample_rate as i64, 1), 0)?);
                }
                Some(f) if (f.version, f.sample_rate, f.channels) != (h.version, h.sample_rate, h.channels) => {
                    return Err(ParseError::new(
                        ErrorCode::InconsistentTrackParameters,
                        format!(
                            "{at} changes version/sample rate/channels from {:?} to {:?}",
                            (f.version, f.sample_rate, f.channels),
                            (h.version, h.sample_rate, h.channels)
                        ),
                    ));
                }
                Some(_) => {}
            }
            let (pts, dur) = timeline.as_mut().expect("set with the first frame").advance(SAMPLES as i128)?;
            ctx.emit(&Unit::new(
                pts,
                dur,
                Flags::NONE.with(Flag::RandomAccess),
                vec![Chunk::src(0, pos, h.frame_len)],
            ))?;
            count += 1;
            pos += h.frame_len;
        }

        let Some(first) = first else {
            return Err(ParseError::invalid("no MPEG audio frames"));
        };
        let mut track = Track::new(TrackType::Audio, "A_MPEG/L2");
        track.audio = Some(Audio::new(Rational::new(first.sample_rate as i64, 1), first.channels as u64));
        Ok(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers() {
        // MPEG-1 Layer II, 192 kbit/s, 48 kHz, stereo: 576 bytes.
        let h = parse_header([0xff, 0xfd, 0xa4, 0x00]).unwrap();
        assert_eq!(h, Header { version: Version::Mpeg1, sample_rate: 48000, channels: 2, frame_len: 576 });
        // 44.1 kHz with padding: 144 * 128000 / 44100 + 1 = 418.
        assert_eq!(parse_header([0xff, 0xfd, 0x82, 0xc0]).unwrap().frame_len, 418);
        // MPEG-2 LSF, 64 kbit/s, 22.05 kHz, mono.
        let h = parse_header([0xff, 0xf5, 0x80, 0xc0]).unwrap();
        assert_eq!((h.version, h.sample_rate, h.channels, h.frame_len), (Version::Mpeg2, 22050, 1, 417));
        assert_eq!(parse_header([0xff, 0xfb, 0x90, 0x00]), Err(HeaderError::Layer(3)));
        assert_eq!(parse_header([0xff, 0xff, 0x90, 0x00]), Err(HeaderError::Layer(1)));
        assert_eq!(parse_header([0xff, 0xfd, 0x04, 0x00]), Err(HeaderError::FreeFormat));
        assert_eq!(parse_header([0xff, 0xfd, 0xf4, 0x00]), Err(HeaderError::Bitrate));
        assert_eq!(parse_header([0xff, 0xfd, 0xac, 0x00]), Err(HeaderError::SampleRate));
        assert_eq!(parse_header([0xff, 0xed, 0xa4, 0x00]), Err(HeaderError::NoSync), "version 01 is reserved");
    }
}
