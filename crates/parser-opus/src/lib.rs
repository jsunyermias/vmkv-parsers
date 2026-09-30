//! Opus in Ogg (`A_OPUS`, RFC 7845).
//!
//! - `codec_private` is the OpusHead packet, referenced in the source.
//! - The OpusTags packet is skipped.
//! - Each audio packet is one unit; a packet split across pages becomes
//!   several `src` chunks. Durations come from the TOC byte (RFC 6716).
//! - Time runs at 48 kHz. The first sample is at
//!   `start granule − pre-skip`, so the first unit usually has a negative
//!   `pts_ns` (rule 4). The pre-skip goes to `codec_delay_ns`, and
//!   `seek_preroll_ns` is the 80 ms the Matroska mapping recommends.
//! - Every page end is checked against the granule position. On the
//!   end-of-stream page a smaller granule means end trimming, written as
//!   `discard_padding_ns` of the last unit.

use vmkv_ogg::{OggReader, Packet};
use vtj::cli::ParseError;
use vtj::*;

pub const RATE: Rational = Rational::new(48000, 1);
pub const SEEK_PREROLL_NS: i64 = 80_000_000;
/// Longest packet allowed by RFC 6716: 120 ms.
pub const MAX_PACKET_SAMPLES: u32 = 5760;

/// Fields of the OpusHead identification header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusHead {
    pub version: u8,
    pub channels: u8,
    pub pre_skip: u16,
    pub input_sample_rate: u32,
    pub mapping_family: u8,
}

pub fn parse_head(b: &[u8]) -> Result<OpusHead, ParseError> {
    if b.len() < 19 || &b[..8] != b"OpusHead" {
        return Err(ParseError::new(
            ErrorCode::MissingInitializationData,
            "the first packet is not an OpusHead header",
        ));
    }
    let head = OpusHead {
        version: b[8],
        channels: b[9],
        pre_skip: u16::from_le_bytes([b[10], b[11]]),
        input_sample_rate: u32::from_le_bytes(b[12..16].try_into().expect("4 bytes")),
        mapping_family: b[18],
    };
    if head.version >> 4 != 0 {
        return Err(ParseError::new(ErrorCode::UnsupportedCodecVariant, format!("OpusHead version {}", head.version)));
    }
    if head.channels == 0 {
        return Err(ParseError::invalid("OpusHead declares 0 channels"));
    }
    match head.mapping_family {
        0 if head.channels > 2 => {
            return Err(ParseError::invalid(format!("mapping family 0 with {} channels", head.channels)));
        }
        0 => {}
        _ if b.len() < 21 + head.channels as usize => {
            return Err(ParseError::invalid("OpusHead channel mapping table is incomplete"));
        }
        _ => {}
    }
    Ok(head)
}

/// Samples at 48 kHz of a packet, from its first two bytes (RFC 6716 §3.1).
pub fn packet_samples(prefix: &[u8]) -> Result<u32, String> {
    let toc = *prefix.first().ok_or("empty packet")?;
    let config = toc >> 3;
    let frame = match config {
        0..=11 => [480, 960, 1920, 2880][(config % 4) as usize],
        12..=15 => [480, 960][(config % 2) as usize],
        _ => [120, 240, 480, 960][(config % 4) as usize],
    };
    let frames = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => {
            let m = (*prefix.get(1).ok_or("code 3 packet without frame count byte")? & 0x3f) as u32;
            if m == 0 {
                return Err("code 3 packet with 0 frames".into());
            }
            m
        }
    };
    let total = frame * frames;
    if total > MAX_PACKET_SAMPLES {
        return Err(format!("packet lasts {total} samples, more than 120 ms"));
    }
    Ok(total)
}

pub struct Opus;

struct Pending {
    packet: Packet,
    samples: u32,
}

impl Parser for Opus {
    fn name(&self) -> &'static str {
        "vmkv-parser-opus"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let mut reader = OggReader::new(ctx.source(0));
        let next = |ctx: &mut Context<'_>, reader: &mut OggReader| reader.next_packet(ctx.source(0));

        let head_packet = next(ctx, &mut reader)?
            .ok_or_else(|| ParseError::new(ErrorCode::MissingInitializationData, "no OpusHead header"))?;
        let head = parse_head(&head_packet.read(ctx.source(0))?)?;
        if head_packet.page_end.is_none_or(|e| e.granule != 0) {
            return Err(ParseError::invalid("OpusHead must be alone on the first page with granule position 0"));
        }
        let tags = next(ctx, &mut reader)?
            .ok_or_else(|| ParseError::new(ErrorCode::MissingInitializationData, "no OpusTags header"))?;
        if tags.prefix(ctx.source(0), 8)? != b"OpusTags" {
            return Err(ParseError::invalid("the second packet is not an OpusTags header"));
        }
        if tags.page_end.is_none() {
            return Err(ParseError::invalid("audio data starts on the page that ends the OpusTags header"));
        }

        let pre_skip = head.pre_skip as i128;
        let mut first_page: Vec<Pending> = Vec::new();
        let mut timeline: Option<Timeline> = None;
        let mut start: i128 = 0;
        let mut total: i128 = 0;
        let mut last: Option<Unit> = None;
        let mut trim: i128 = 0;
        let mut last_samples: u32 = 0;
        let flags = Flags::NONE.with(Flag::RandomAccess);

        while let Some(p) = next(ctx, &mut reader)? {
            if p.is_empty() {
                return Err(ParseError::invalid(format!("packet {} is empty", p.index)));
            }
            let samples = packet_samples(&p.prefix(ctx.source(0), 2)?)
                .map_err(|e| ParseError::invalid(format!("packet {}: {e}", p.index)))?;
            let page_end = p.page_end;
            total += samples as i128;

            if timeline.is_none() {
                first_page.push(Pending { packet: p, samples });
                let Some(end) = page_end else { continue };
                let granule = end.granule as i128;
                if granule >= total {
                    start = granule - total;
                } else if end.eos {
                    start = 0;
                    trim = total - granule;
                } else {
                    return Err(ParseError::invalid(format!(
                        "granule position {granule} of page {} is smaller than the {total} samples it completes",
                        end.sequence
                    )));
                }
                let mut tl = Timeline::new(RATE, start - pre_skip)?;
                for q in first_page.drain(..) {
                    let (pts, dur) = tl.advance(q.samples as i128)?;
                    if let Some(prev) = last.replace(Unit::new(pts, dur, flags, q.packet.chain(0))) {
                        ctx.emit(&prev)?;
                    }
                    last_samples = q.samples;
                }
                timeline = Some(tl);
                continue;
            }

            let tl = timeline.as_mut().expect("checked above");
            let (pts, dur) = tl.advance(samples as i128)?;
            if let Some(prev) = last.replace(Unit::new(pts, dur, flags, p.chain(0))) {
                ctx.emit(&prev)?;
            }
            last_samples = samples;
            if let Some(end) = page_end {
                let expected = start + total;
                let granule = end.granule as i128;
                if granule == expected {
                    continue;
                }
                if end.eos && granule < expected {
                    trim = expected - granule;
                } else {
                    return Err(ParseError::invalid(format!(
                        "granule position {granule} of page {} does not match the {expected} samples decoded",
                        end.sequence
                    )));
                }
            }
        }

        if timeline.is_none() {
            if first_page.is_empty() {
                return Err(ParseError::invalid("no audio packets"));
            }
            return Err(ParseError::truncated("the stream ends before any page with a granule position completes"));
        }
        let Some(mut last) = last else {
            return Err(ParseError::invalid("no audio packets"));
        };
        if trim > 0 {
            if trim > last_samples as i128 {
                return Err(ParseError::new(
                    ErrorCode::UnrepresentableInVmkv,
                    format!("end trimming of {trim} samples exceeds the last packet"),
                ));
            }
            last.discard_padding_ns = Some(ticks_to_ns(trim, RATE)?);
        }
        ctx.emit(&last)?;

        let mut track = Track::new(TrackType::Audio, "A_OPUS");
        track.codec_private = Some(head_packet.chain(0));
        track.codec_delay_ns = Some(ticks_to_ns(pre_skip, RATE)?);
        track.seek_preroll_ns = Some(SEEK_PREROLL_NS);
        track.audio = Some(Audio::new(RATE, head.channels as u64));
        Ok(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toc_durations() {
        assert_eq!(packet_samples(&[0xfc]), Ok(960), "CELT 20 ms, 1 frame");
        assert_eq!(packet_samples(&[0x60]), Ok(480), "hybrid 10 ms");
        assert_eq!(packet_samples(&[0x78]), Ok(960), "hybrid 20 ms");
        assert_eq!(packet_samples(&[0x18]), Ok(2880), "SILK 60 ms");
        assert_eq!(packet_samples(&[0xfd]), Ok(1920), "code 1: 2 frames");
        assert_eq!(packet_samples(&[0x83, 0x03]), Ok(360), "CELT 2.5 ms x3");
        assert_eq!(packet_samples(&[0x1b, 0x02]), Ok(5760), "SILK 60 ms x2 = 120 ms");
        assert!(packet_samples(&[0x1b, 0x03]).unwrap_err().contains("more than 120 ms"));
        assert!(packet_samples(&[0xff, 0x00]).unwrap_err().contains("0 frames"));
        assert!(packet_samples(&[0xff]).is_err());
        assert!(packet_samples(&[]).is_err());
    }

    #[test]
    fn head_validation() {
        let mut h = b"OpusHead".to_vec();
        h.extend([1, 2, 0x38, 0x01, 0x80, 0xbb, 0, 0, 0, 0, 0]);
        let head = parse_head(&h).unwrap();
        assert_eq!((head.channels, head.pre_skip, head.input_sample_rate), (2, 312, 48000));
        let mut v = h.clone();
        v[8] = 0x10;
        assert_eq!(parse_head(&v).unwrap_err().code, ErrorCode::UnsupportedCodecVariant);
        let mut c = h.clone();
        c[9] = 3;
        assert!(parse_head(&c).is_err());
        let mut f = h.clone();
        f[18] = 1;
        assert!(parse_head(&f).unwrap_err().message.contains("mapping table"));
        assert_eq!(parse_head(b"OpusTags").unwrap_err().code, ErrorCode::MissingInitializationData);
    }
}
