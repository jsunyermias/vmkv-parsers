//! AAC in ADTS (`A_AAC`).
//!
//! - ID3v2, ID3v1, APEv2 and Lyrics3v2 tags are skipped.
//! - The payload of each unit is the raw AAC frame: the source bytes after
//!   the ADTS header, whose length (7 bytes, or 9 with CRC) is read from the
//!   header and never assumed.
//! - `codec_private` is the 2-byte AudioSpecificConfig built from the first
//!   header (object type, sampling frequency index, channel configuration).
//! - Each frame lasts 1024 samples at the ADTS sampling frequency. ADTS
//!   carries no encoder delay, implicit SBR or frame-length information, so
//!   none is assumed (`codec_delay_ns` and `output_sampling_frequency` are
//!   omitted).
//! - Rejected: several raw data blocks per frame, channel configuration 0
//!   (program config element in the payload) and parameter changes.

use vtj::cli::ParseError;
use vtj::*;

pub const SAMPLE_RATES: [u32; 13] =
    [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

/// Samples per AAC frame. ADTS cannot signal the 960-sample variant.
pub const FRAME_SAMPLES: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdtsHeader {
    /// MPEG-4 audio object type (ADTS profile + 1).
    pub object_type: u8,
    pub sampling_index: u8,
    pub channel_config: u8,
    /// 7, or 9 when a CRC follows the fixed header.
    pub header_len: u64,
    /// Whole ADTS frame, header included.
    pub frame_len: u64,
    pub raw_blocks: u8,
}

impl AdtsHeader {
    pub fn sample_rate(&self) -> u32 {
        SAMPLE_RATES[self.sampling_index as usize]
    }

    /// AudioSpecificConfig with a GASpecificConfig of all-zero flags.
    pub fn audio_specific_config(&self) -> [u8; 2] {
        let v: u16 =
            (self.object_type as u16) << 11 | (self.sampling_index as u16) << 7 | (self.channel_config as u16) << 3;
        v.to_be_bytes()
    }

    fn params(&self) -> (u8, u8, u8) {
        (self.object_type, self.sampling_index, self.channel_config)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderError {
    NoSync,
    Layer,
    SamplingIndex(u8),
    FrameLength(u64),
}

/// Parses the 7-byte fixed + variable ADTS header.
pub fn parse_header(b: [u8; 7]) -> Result<AdtsHeader, HeaderError> {
    if b[0] != 0xff || b[1] & 0xf0 != 0xf0 {
        return Err(HeaderError::NoSync);
    }
    if b[1] & 0x06 != 0 {
        return Err(HeaderError::Layer);
    }
    let protection_absent = b[1] & 1 == 1;
    let object_type = (b[2] >> 6) + 1;
    let sampling_index = (b[2] >> 2) & 0x0f;
    if sampling_index as usize >= SAMPLE_RATES.len() {
        return Err(HeaderError::SamplingIndex(sampling_index));
    }
    let channel_config = ((b[2] & 1) << 2) | (b[3] >> 6);
    let frame_len = (((b[3] & 3) as u64) << 11) | ((b[4] as u64) << 3) | (b[5] as u64 >> 5);
    let header_len = if protection_absent { 7 } else { 9 };
    if frame_len <= header_len {
        return Err(HeaderError::FrameLength(frame_len));
    }
    let raw_blocks = (b[6] & 3) + 1;
    Ok(AdtsHeader { object_type, sampling_index, channel_config, header_len, frame_len, raw_blocks })
}

pub struct Aac;

impl Parser for Aac {
    fn name(&self) -> &'static str {
        "vmkv-parser-aac"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let range = tags::audio_range(ctx.source(0))?;
        let mut pos = range.start;
        let mut first: Option<AdtsHeader> = None;
        let mut timeline: Option<Timeline> = None;
        let mut count: u64 = 0;

        while pos < range.end {
            if range.end - pos < 7 {
                return Err(ParseError::truncated(format!("frame {count} header cut at byte {}", range.end)));
            }
            let mut hb = [0u8; 7];
            ctx.source(0).read_at(pos, &mut hb)?;
            let h = parse_header(hb).map_err(|e| match e {
                HeaderError::NoSync => ParseError::invalid(format!("no ADTS sync at byte {pos}")),
                HeaderError::Layer => ParseError::invalid(format!("non-zero ADTS layer at byte {pos}")),
                HeaderError::SamplingIndex(i) => {
                    ParseError::invalid(format!("invalid sampling frequency index {i} at byte {pos}"))
                }
                HeaderError::FrameLength(l) => ParseError::invalid(format!("invalid frame length {l} at byte {pos}")),
            })?;
            if h.frame_len > range.end - pos {
                return Err(ParseError::truncated(format!("frame {count} cut at byte {}", range.end)));
            }
            if h.raw_blocks != 1 {
                return Err(ParseError::unsupported(format!(
                    "frame {count} carries {} raw data blocks; only one per ADTS frame is supported",
                    h.raw_blocks
                )));
            }
            match first {
                None => {
                    if h.channel_config == 0 {
                        return Err(ParseError::unsupported(
                            "channel configuration 0 (program config element) is not supported",
                        ));
                    }
                    first = Some(h);
                    timeline = Some(Timeline::new(Rational::new(h.sample_rate() as i64, 1), 0)?);
                }
                Some(f) if f.params() != h.params() => {
                    return Err(ParseError::new(
                        ErrorCode::InconsistentTrackParameters,
                        format!(
                            "frame {count} changes object type/sampling index/channels from {:?} to {:?}",
                            f.params(),
                            h.params()
                        ),
                    ));
                }
                Some(_) => {}
            }
            let (pts, dur) = timeline.as_mut().expect("set with the first frame").advance(FRAME_SAMPLES as i128)?;
            let payload = vec![Chunk::src(0, pos + h.header_len, h.frame_len - h.header_len)];
            ctx.emit(&Unit::new(pts, dur, Flags::NONE.with(Flag::RandomAccess), payload))?;
            count += 1;
            pos += h.frame_len;
        }

        let Some(first) = first else {
            return Err(ParseError::invalid("no ADTS frames"));
        };
        let mut track = Track::new(TrackType::Audio, "A_AAC");
        track.codec_private = Some(vec![Chunk::inline(first.audio_specific_config())]);
        let channels = match first.channel_config {
            7 => 8,
            c => c as u64,
        };
        track.audio = Some(Audio::new(Rational::new(first.sample_rate() as i64, 1), channels));
        Ok(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(profile: u8, sf: u8, ch: u8, len: u16, crc: bool, blocks: u8) -> [u8; 7] {
        [
            0xff,
            0xf0 | if crc { 0 } else { 1 },
            (profile << 6) | (sf << 2) | (ch >> 2),
            ((ch & 3) << 6) | (len >> 11) as u8,
            (len >> 3) as u8,
            ((len & 7) << 5) as u8 | 0x1f,
            0xfc | (blocks - 1),
        ]
    }

    #[test]
    fn spec_example_config() {
        let h = parse_header(header(1, 4, 2, 371, false, 1)).unwrap();
        assert_eq!(
            (h.object_type, h.sample_rate(), h.channel_config, h.header_len, h.frame_len),
            (2, 44100, 2, 7, 371)
        );
        assert_eq!(h.audio_specific_config(), [0x12, 0x10]);
    }

    #[test]
    fn crc_header_is_nine_bytes() {
        assert_eq!(parse_header(header(1, 3, 1, 200, true, 1)).unwrap().header_len, 9);
    }

    #[test]
    fn header_errors() {
        assert_eq!(parse_header([0xff, 0xe1, 0, 0, 0, 0, 0]), Err(HeaderError::NoSync));
        let mut l = header(1, 4, 2, 371, false, 1);
        l[1] |= 0x02;
        assert_eq!(parse_header(l), Err(HeaderError::Layer));
        assert_eq!(parse_header(header(1, 13, 2, 371, false, 1)), Err(HeaderError::SamplingIndex(13)));
        assert_eq!(parse_header(header(1, 4, 2, 7, false, 1)), Err(HeaderError::FrameLength(7)));
        assert_eq!(parse_header(header(1, 4, 2, 9, true, 1)), Err(HeaderError::FrameLength(9)));
        assert_eq!(parse_header(header(1, 4, 2, 371, false, 3)).unwrap().raw_blocks, 3);
    }
}
