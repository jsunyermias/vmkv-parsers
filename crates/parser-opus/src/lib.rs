//! Opus in Ogg (`A_OPUS`, RFC 7845).
//!
//! - `codec_private` is the OpusHead packet, referenced in the source.
//! - The OpusTags packet is skipped. As RFC 7845 requires, the pages that end
//!   OpusHead and OpusTags must have granule position 0.
//! - Each audio packet is one unit; a packet split across pages becomes
//!   several `src` chunks. Durations come from the TOC byte (RFC 6716).
//! - Time runs at 48 kHz. The first sample is at
//!   `start granule − pre-skip`, so the first unit usually has a negative
//!   `pts_ns` (rule 4). The pre-skip goes to `codec_delay_ns`, and
//!   `seek_preroll_ns` is the 80 ms the Matroska mapping recommends.
//! - Every page end is checked against the granule position. On the
//!   end-of-stream page a smaller granule means end trimming, written as
//!   `discard_padding_ns` of the last units of that page.

pub mod multistream;
pub mod ogg;

use ogg::{OggReader, Packet};
use vtj::cli::ParseError;
use vtj::source::SourceFile;
use vtj::*;

pub const RATE: Rational = Rational::new(48000, 1);
pub const SEEK_PREROLL_NS: i64 = 80_000_000;
/// RFC 7845 §6: a packet over this many octets per logical stream is not
/// interoperable and is rejected outright, before any allocation sized by
/// it is attempted (a packet can span many pages, so its length is not
/// otherwise bounded by any single page's own, much smaller, limit).
pub const MAX_PACKET_BYTES: u64 = 61_440;
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
    /// Number of physical Opus streams multiplexed into each Ogg-level
    /// packet (RFC 7845 §5.1.1). Always 1 for mapping family 0, which is a
    /// single (mono or stereo) Opus stream, never itself "multistream".
    pub stream_count: u8,
    /// How many of `stream_count` decode to 2 channels (the rest, 1).
    pub coupled_count: u8,
}

pub fn parse_head(b: &[u8]) -> Result<OpusHead, ParseError> {
    if b.len() < 19 || &b[..8] != b"OpusHead" {
        return Err(ParseError::new(
            ErrorCode::MissingInitializationData,
            "the first packet is not an OpusHead header",
        ));
    }
    let mut head = OpusHead {
        version: b[8],
        channels: b[9],
        pre_skip: u16::from_le_bytes([b[10], b[11]]),
        input_sample_rate: u32::from_le_bytes(b[12..16].try_into().expect("4 bytes")),
        mapping_family: b[18],
        stream_count: 1,
        coupled_count: 0,
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
        0 => head.coupled_count = if head.channels == 2 { 1 } else { 0 },
        _ => {
            if b.len() < 21 + head.channels as usize {
                return Err(ParseError::invalid("OpusHead channel mapping table is incomplete"));
            }
            // RFC 7845 §5.1.1: the stream and channel mapping table that
            // follows the mapping family byte for any family but 0.
            let (stream_count, coupled_count) = (b[19], b[20]);
            if stream_count == 0 {
                return Err(ParseError::invalid("OpusHead declares 0 streams"));
            }
            if coupled_count > stream_count {
                return Err(ParseError::invalid(format!(
                    "OpusHead declares {coupled_count} coupled streams, more than its {stream_count} streams"
                )));
            }
            let total_streams = stream_count as u16 + coupled_count as u16;
            if total_streams > 255 {
                return Err(ParseError::invalid(format!(
                    "OpusHead declares {stream_count} streams and {coupled_count} coupled ({total_streams} in total), more than 255"
                )));
            }
            let total_streams = total_streams as u8;
            for (i, &m) in b[21..21 + head.channels as usize].iter().enumerate() {
                if m != 255 && m >= total_streams {
                    return Err(ParseError::invalid(format!(
                        "OpusHead channel mapping index {m} for channel {i} is neither below {total_streams} streams nor 255"
                    )));
                }
            }
            head.stream_count = stream_count;
            head.coupled_count = coupled_count;
        }
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

/// Splits a multistream Ogg-level Opus packet into `stream_count` per-stream
/// packets — the first `stream_count - 1` self-delimited (RFC 6716
/// Appendix B), the last with normal framing consuming the rest — and
/// returns their shared duration. RFC 7845 requires every stream to last
/// the same number of samples; the payload itself is kept as the whole
/// original packet (`p.chain(0)`, decision 47) — this only exists to
/// validate the framing and compute the unit's timing, never to reference
/// or rewrite the sub-packets' own bytes.
fn multistream_samples(stream_count: u8, packet: &[u8]) -> Result<u32, String> {
    let mut pos = 0usize;
    let mut samples = None;
    for i in 0..stream_count {
        let self_delimited = i + 1 < stream_count;
        let (used, s) =
            multistream::parse_subpacket(&packet[pos..], self_delimited).map_err(|e| format!("stream {i}: {e}"))?;
        pos += used;
        match samples {
            None => samples = Some(s),
            Some(prev) if prev != s => {
                return Err(format!("stream {i} lasts {s} samples, stream 0 lasts {prev}"));
            }
            Some(_) => {}
        }
    }
    // In practice this cannot fire: the last stream's normal framing always
    // consumes exactly what remains of `packet` by construction (that is
    // the point of only self-delimiting the streams before it — Ogg's
    // lacing, not Opus framing, delimits the packet as a whole), so a stray
    // trailing byte is absorbed into it rather than left over. Kept as a
    // defensive invariant in case that ever stops holding.
    if pos != packet.len() {
        return Err(format!("{} trailing byte(s) after {stream_count} streams", packet.len() - pos));
    }
    Ok(samples.expect("stream_count > 0, checked when OpusHead is parsed"))
}

/// Reads a whole packet after checking it is not absurdly large (RFC 7845
/// §6, decision 52): a packet can span many pages, so nothing else bounds
/// its length before this. `streams` scales the limit for a multistream
/// group, matching decision 47's `stream_count` sub-packets.
fn read_bounded_packet(p: &Packet, src: &mut SourceFile, streams: u8, what: &str) -> Result<Vec<u8>, ParseError> {
    let max = MAX_PACKET_BYTES * streams as u64;
    if p.len() > max {
        return Err(ParseError::new(
            ErrorCode::UnsupportedFeature,
            format!("{what} is {} bytes, more than the {max}-byte limit for {streams} stream(s)", p.len()),
        ));
    }
    p.read(src)
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
        let head = parse_head(&read_bounded_packet(&head_packet, ctx.source(0), 1, "OpusHead")?)?;
        if head_packet.page_end.is_none_or(|e| e.granule != 0) {
            return Err(ParseError::invalid("OpusHead must be alone on the first page with granule position 0"));
        }
        let tags = next(ctx, &mut reader)?
            .ok_or_else(|| ParseError::new(ErrorCode::MissingInitializationData, "no OpusTags header"))?;
        if tags.prefix(ctx.source(0), 8)? != b"OpusTags" {
            return Err(ParseError::invalid("the second packet is not an OpusTags header"));
        }
        match tags.page_end {
            None => return Err(ParseError::invalid("audio data starts on the page that ends the OpusTags header")),
            Some(end) if end.granule != 0 => {
                return Err(ParseError::invalid(format!(
                    "the page that ends the OpusTags header has granule position {} instead of 0",
                    end.granule
                )));
            }
            Some(_) => {}
        }

        let pre_skip = head.pre_skip as i128;
        let flags = Flags::NONE.with(Flag::RandomAccess);
        let mut first_page: Vec<Pending> = Vec::new();
        let mut timeline: Option<Timeline> = None;
        let mut start: i128 = 0;
        let mut total: i128 = 0;
        let mut held: Vec<Unit> = Vec::new();
        let mut trim: i128 = 0;
        let mut any = false;

        while let Some(p) = next(ctx, &mut reader)? {
            if p.is_empty() {
                return Err(ParseError::invalid(format!("packet {} is empty", p.index)));
            }
            // Every packet goes through the same RFC 6716 §3.2 framing
            // parser, `stream_count == 1` included (decision 52): the cheap
            // 2-byte-prefix duration-only read `packet_samples` used to
            // take for that common case computed a duration without
            // checking the packing rules it implies (frame parity, VBR/CBR
            // sizes, the 1275-byte frame cap), so a malformed single-stream
            // packet with a plausible TOC could pass unexamined.
            let bytes = read_bounded_packet(&p, ctx.source(0), head.stream_count, "an audio packet")?;
            let samples = multistream_samples(head.stream_count, &bytes)
                .map_err(|e| ParseError::invalid(format!("packet {}: {e}", p.index)))?;
            let page_end = p.page_end;
            total += samples as i128;
            any = true;

            match timeline.as_mut() {
                None => {
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
                        held.push(Unit::new(pts, dur, flags, q.packet.chain(0)));
                    }
                    timeline = Some(tl);
                }
                Some(tl) => {
                    let (pts, dur) = tl.advance(samples as i128)?;
                    held.push(Unit::new(pts, dur, flags, p.chain(0)));
                    let Some(end) = page_end else { continue };
                    let expected = start + total;
                    let granule = end.granule as i128;
                    if end.eos && granule < expected {
                        trim = expected - granule;
                    } else if granule != expected {
                        return Err(ParseError::invalid(format!(
                            "granule position {granule} of page {} does not match the {expected} samples decoded",
                            end.sequence
                        )));
                    }
                }
            }
            if page_end.is_some_and(|e| !e.eos) {
                for u in held.drain(..) {
                    ctx.emit(&u)?;
                }
            }
        }

        let Some(tl) = timeline else {
            return Err(if any {
                ParseError::truncated("the stream ends before any page with a granule position completes")
            } else {
                ParseError::invalid("no audio packets")
            });
        };
        if trim > 0 {
            let audible_end = ticks_to_ns(tl.position() - trim, RATE)?;
            if !trim_end(&mut held, audible_end) {
                return Err(ParseError::new(
                    ErrorCode::UnrepresentableInVmkv,
                    format!("end trimming of {trim} samples reaches before the last page"),
                ));
            }
        }
        for u in &held {
            ctx.emit(u)?;
        }

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

    /// `family, channels, stream_count, coupled_count, mapping` -> a well-formed
    /// OpusHead packet (RFC 7845 §5.1.1 layout).
    fn multichannel_head(family: u8, channels: u8, stream_count: u8, coupled_count: u8, mapping: &[u8]) -> Vec<u8> {
        let mut h = b"OpusHead".to_vec();
        h.extend([1, channels, 0x38, 0x01, 0x80, 0xbb, 0, 0, 0, 0, family, stream_count, coupled_count]);
        h.extend_from_slice(mapping);
        h
    }

    #[test]
    fn multichannel_mapping_table_rfc7845_5_1_1() {
        // 4 channels over 2 streams, both coupled (a plausible quadraphonic layout).
        let h = multichannel_head(1, 4, 2, 2, &[0, 1, 2, 3]);
        let head = parse_head(&h).unwrap();
        assert_eq!(head.channels, 4);

        // A mapping index of 255 (silence) is always allowed, even with few streams.
        let h = multichannel_head(1, 3, 1, 0, &[0, 255, 0]);
        assert!(parse_head(&h).is_ok());

        let h = multichannel_head(1, 2, 0, 0, &[0, 0]);
        assert!(parse_head(&h).unwrap_err().message.contains("0 streams"), "{:?}", parse_head(&h));

        let h = multichannel_head(1, 2, 1, 2, &[0, 0]);
        assert!(parse_head(&h).unwrap_err().message.contains("coupled streams"), "{:?}", parse_head(&h));

        let h = multichannel_head(1, 2, 200, 200, &[0, 0]);
        assert!(parse_head(&h).unwrap_err().message.contains("more than 255"), "{:?}", parse_head(&h));

        // total_streams = 2 (1 stream + 1 coupled); index 2 is out of range and not 255.
        let h = multichannel_head(1, 2, 1, 1, &[0, 2]);
        let err = parse_head(&h).unwrap_err();
        assert!(err.message.contains("mapping index 2") && err.message.contains("neither below 2"), "{err:?}");
    }
}
