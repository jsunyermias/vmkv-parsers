//! HDMV Presentation Graphics subtitles in a `.sup` file (`S_HDMV/PGS`).
//!
//! - A `.sup` file is a run of segments, each behind a 13-byte header:
//!   `PG`, a 90 kHz PTS and DTS, the segment type and its size. Matroska
//!   blocks hold the segments from that type byte on, without the `PG`,
//!   PTS and DTS.
//! - Each unit is one display set, from its presentation composition
//!   segment (PCS) to its END segment, as real muxers store it (decision
//!   65): one `src` chunk per segment. Its time is the PCS PTS; its
//!   duration is unknown (`-1`), since a display set stays on screen until
//!   the next one.
//! - `random_access` marks the display sets that need nothing before them:
//!   an epoch start or acquisition point, or one with no composition
//!   objects (it clears the screen). A normal-case update with objects
//!   builds on the decoder's previous state.

use vtj::cli::ParseError;
use vtj::*;

const HEADER: u64 = 13;
const PDS: u8 = 0x14;
const ODS: u8 = 0x15;
const PCS: u8 = 0x16;
const WDS: u8 = 0x17;
const END: u8 = 0x80;
const RATE: Rational = Rational::new(90000, 1);

/// PCS `composition_state` values with a complete description.
const EPOCH_START: u8 = 0x80;
const ACQUISITION_POINT: u8 = 0x40;

pub struct Pgs;

impl Parser for Pgs {
    fn name(&self) -> &'static str {
        "vmkv-parser-pgs"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let size = ctx.source(0).size();
        let mut pos = 0u64;
        let mut set: Option<(u32, Flags, Vec<Chunk>)> = None;
        let mut count = 0u64;

        while pos < size {
            if size - pos < HEADER {
                return Err(ParseError::truncated(format!("segment header at byte {pos} cut at byte {size}")));
            }
            let mut h = [0u8; HEADER as usize];
            ctx.source(0).read_at(pos, &mut h)?;
            if &h[..2] != b"PG" {
                return Err(ParseError::invalid(format!("no PG segment header at byte {pos}")));
            }
            let pts = u32::from_be_bytes(h[2..6].try_into().expect("4 bytes"));
            let kind = h[10];
            let len = u16::from_be_bytes([h[11], h[12]]) as u64;
            if len > size - pos - HEADER {
                return Err(ParseError::truncated(format!("segment at byte {pos} cut at byte {size}")));
            }
            let chunk = Chunk::src(0, pos + 10, 3 + len);
            match (kind, set.as_mut()) {
                (PCS, None) => {
                    if len < 11 {
                        return Err(ParseError::invalid(format!("PCS at byte {pos} is {len} bytes")));
                    }
                    let mut pcs = [0u8; 11];
                    ctx.source(0).read_at(pos + HEADER, &mut pcs)?;
                    let (state, objects) = (pcs[7], pcs[10]);
                    let mut flags = Flags::NONE;
                    if state & (EPOCH_START | ACQUISITION_POINT) != 0 || objects == 0 {
                        flags.insert(Flag::RandomAccess);
                    }
                    set = Some((pts, flags, vec![chunk]));
                }
                (PCS, Some(_)) => {
                    return Err(ParseError::invalid(format!(
                        "display set {count}: a second PCS at byte {pos} before END"
                    )));
                }
                (_, None) => {
                    return Err(ParseError::invalid(format!(
                        "display set {count} at byte {pos} starts with segment type 0x{kind:02x} instead of a PCS"
                    )));
                }
                (PDS | ODS | WDS, Some((_, _, chunks))) => chunks.push(chunk),
                (END, Some(_)) => {
                    if len != 0 {
                        return Err(ParseError::invalid(format!("END segment at byte {pos} is {len} bytes")));
                    }
                    let (pts, flags, mut chunks) = set.take().expect("matched Some");
                    chunks.push(chunk);
                    ctx.emit(&Unit::new(ticks_to_ns(pts as i128, RATE)?, -1, flags, chunks))?;
                    count += 1;
                }
                (k, Some(_)) => {
                    return Err(ParseError::invalid(format!("unknown segment type 0x{k:02x} at byte {pos}")));
                }
            }
            pos += HEADER + len;
        }
        if set.is_some() {
            return Err(ParseError::truncated(format!("display set {count} has no END segment at byte {size}")));
        }
        if count == 0 {
            return Err(ParseError::invalid("no display sets"));
        }
        Ok(Track::new(TrackType::Subtitle, "S_HDMV/PGS"))
    }
}
