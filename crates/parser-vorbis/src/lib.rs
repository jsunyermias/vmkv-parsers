//! Vorbis in Ogg (`A_VORBIS`).
//!
//! - `codec_private` is the three header packets in Xiph lacing, as the
//!   Matroska mapping defines it: the lacing prefix inline, the packets
//!   referenced in the source.
//! - Each audio packet is one unit. Its duration is a quarter of the
//!   previous block size plus a quarter of its own (Vorbis I §1.3.2); the
//!   first packet only primes the decoder and lasts 0. Block sizes come
//!   from each packet's mode, whose block flags are in the setup header.
//! - Times are sample positions at the stream's sample rate. Every page end
//!   is checked against its granule position. On the first audio page a
//!   smaller granule means samples to discard at the start, which get
//!   negative times (rule 4); on the end-of-stream page it means end
//!   trimming, written as `discard_padding_ns` (decision 61).
//! - Every packet is a random access point: decoding from any packet only
//!   loses that packet's own output, as with MP3's bit reservoir.

pub mod headers;
pub mod ogg;

use headers::Identification;
use ogg::{OggReader, Packet};
use vtj::cli::ParseError;
use vtj::source::SourceFile;
use vtj::*;

/// Generous bounds for the header packets read whole: a real setup header
/// is a few kilobytes; the identification header is exactly 30 bytes.
const MAX_SETUP_BYTES: u64 = 4 << 20;
const MAX_ID_BYTES: u64 = 64;

fn read_bounded(p: &Packet, src: &mut SourceFile, max: u64, what: &str) -> Result<Vec<u8>, ParseError> {
    if p.len() > max {
        return Err(ParseError::unsupported(format!("{what} is {} bytes, more than the {max}-byte limit", p.len())));
    }
    p.read(src)
}

/// Xiph lacing of a packet length: `len / 255` bytes of 255, then the rest.
fn lace(len: u64, out: &mut Vec<u8>) {
    out.extend(std::iter::repeat_n(255u8, (len / 255) as usize));
    out.push((len % 255) as u8);
}

pub struct Vorbis;

struct Pending {
    packet: Packet,
    samples: u32,
}

impl Parser for Vorbis {
    fn name(&self) -> &'static str {
        "vmkv-parser-vorbis"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let mut reader = OggReader::new(ctx.source(0));
        let next = |ctx: &mut Context<'_>, reader: &mut OggReader| reader.next_packet(ctx.source(0));
        let missing = |what: &str| ParseError::new(ErrorCode::MissingInitializationData, format!("no {what} header"));

        let id_packet = next(ctx, &mut reader)?.ok_or_else(|| missing("identification"))?;
        let id_bytes = read_bounded(&id_packet, ctx.source(0), MAX_ID_BYTES, "the identification header")?;
        let id: Identification = headers::parse_identification(&id_bytes)
            .map_err(|e| ParseError::new(ErrorCode::MissingInitializationData, format!("first packet: {e}")))?;
        if id_packet.page_end.is_none_or(|e| e.granule != 0) {
            return Err(ParseError::invalid(
                "the identification header must be alone on the first page with granule position 0",
            ));
        }
        let comment = next(ctx, &mut reader)?.ok_or_else(|| missing("comment"))?;
        headers::check_comment(&comment.prefix(ctx.source(0), 7)?)
            .map_err(|e| ParseError::invalid(format!("second packet: {e}")))?;
        let setup = next(ctx, &mut reader)?.ok_or_else(|| missing("setup"))?;
        let setup_bytes = read_bounded(&setup, ctx.source(0), MAX_SETUP_BYTES, "the setup header")?;
        let modes = headers::parse_setup(&setup_bytes, id.channels)
            .map_err(|e| ParseError::invalid(format!("third packet: {e}")))?;
        match setup.page_end {
            None => return Err(ParseError::invalid("audio data starts on the page that ends the setup header")),
            Some(end) if end.granule != 0 => {
                return Err(ParseError::invalid(format!(
                    "the page that ends the setup header has granule position {} instead of 0",
                    end.granule
                )));
            }
            Some(_) => {}
        }

        let mut private = vec![2u8];
        lace(id_packet.len(), &mut private);
        lace(comment.len(), &mut private);
        let mut codec_private = vec![Chunk::inline(private)];
        for p in [&id_packet, &comment, &setup] {
            codec_private.extend(p.chain(0));
        }

        let rate = Rational::new(id.sample_rate as i64, 1);
        let flags = Flags::NONE.with(Flag::RandomAccess);
        let mut previous: Option<u32> = None;
        let mut first_page: Vec<Pending> = Vec::new();
        let mut timeline: Option<Timeline> = None;
        let mut start: i128 = 0;
        let mut total: i128 = 0;
        let mut held: Vec<Unit> = Vec::new();
        let mut trim: i128 = 0;
        let mut any = false;

        while let Some(p) = next(ctx, &mut reader)? {
            let blocksize = headers::packet_blocksize(&p.prefix(ctx.source(0), 1)?, &id, &modes)
                .map_err(|e| ParseError::invalid(format!("packet {}: {e}", p.index)))?;
            let samples = previous.map_or(0, |prev| prev / 4 + blocksize / 4);
            previous = Some(blocksize);
            let page_end = p.page_end;
            total += samples as i128;
            any = true;

            match timeline.as_mut() {
                None => {
                    first_page.push(Pending { packet: p, samples });
                    let Some(end) = page_end else { continue };
                    let granule = end.granule as i128;
                    if end.eos && granule < total {
                        // The first audio page is also the last: a smaller
                        // granule trims the end, as on any final page.
                        start = 0;
                        trim = total - granule;
                    } else {
                        // Otherwise a smaller granule means samples to drop
                        // at the start: they get negative times.
                        start = granule - total;
                    }
                    let mut tl = Timeline::new(rate, start)?;
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
            let audible_end = ticks_to_ns(tl.position() - trim, rate)?;
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

        let mut track = Track::new(TrackType::Audio, "A_VORBIS");
        track.codec_private = Some(codec_private);
        track.audio = Some(Audio::new(rate, id.channels as u64));
        Ok(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xiph_lacing() {
        let mut v = Vec::new();
        lace(30, &mut v);
        lace(255, &mut v);
        lace(600, &mut v);
        assert_eq!(v, [30, 255, 0, 255, 255, 90]);
    }
}
