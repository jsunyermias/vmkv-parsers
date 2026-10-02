//! WavPack 4/5 (`A_WAVPACK4`), with an optional `.wvc` correction file as
//! source 1 (decision 73).
//!
//! - A frame is the run of blocks from `INITIAL_BLOCK` to `FINAL_BLOCK`
//!   (one block for mono or stereo, several for more channels); each frame
//!   is one unit, a random access point, timed by its `block_index`.
//! - The payload follows the Matroska WavPack mapping: the header is
//!   reduced to `block_samples`, `flags` and `crc` (each block after the
//!   first keeps `flags` and `crc`), and in a multi-block frame each block's
//!   data size is added inline. Everything else is referenced in the source.
//! - With a `.wvc`, the correction blocks of a hybrid file go into block
//!   addition 1 (`crc`, plus the data size for multi-block frames, then the
//!   data), and the track declares a block addition mapping of type 1.
//! - `codec_private` is the stream version from the first block header.
//! - Blocks without samples (metadata only) carry no audio and are left
//!   out. Trailing APEv2 and ID3v1 tags, and a leading ID3v2, are skipped.
//! - Rejected: DSD audio, versions outside 0x402 to 0x410, and parameter
//!   changes.

pub mod tags;

use vtj::cli::ParseError;
use vtj::source::SourceFile;
use vtj::*;

const HEADER: u64 = 32;
const INITIAL_BLOCK: u32 = 0x800;
const FINAL_BLOCK: u32 = 0x1000;
const MONO_FLAG: u32 = 4;
const HYBRID_FLAG: u32 = 8;
const FLOAT_DATA: u32 = 0x80;
const FALSE_STEREO: u32 = 0x4000_0000;
const DSD_FLAG: u32 = 0x8000_0000;
const RATES: [u32; 15] =
    [6000, 8000, 9600, 11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000, 64000, 88200, 96000, 192000];
/// Sub-block id of a non-standard sample rate.
const ID_SAMPLE_RATE: u8 = 0x27;
/// A block larger than this is rejected: libwavpack never writes blocks
/// near it, and it bounds what one read can allocate.
const MAX_BLOCK: u64 = 1 << 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    /// Bytes of the whole block, header included.
    pub size: u64,
    pub version: u16,
    pub index: u64,
    pub samples: u32,
    pub flags: u32,
}

impl Block {
    pub fn channels(&self) -> u64 {
        if self.flags & MONO_FLAG != 0 && self.flags & FALSE_STEREO == 0 {
            1
        } else {
            2
        }
    }

    fn rate_index(&self) -> u32 {
        (self.flags >> 23) & 15
    }
}

pub fn parse_header(h: &[u8; HEADER as usize]) -> Result<Block, String> {
    if &h[..4] != b"wvpk" {
        return Err("no wvpk block header".into());
    }
    let u32_at = |i: usize| u32::from_le_bytes(h[i..i + 4].try_into().expect("4 bytes"));
    let ck_size = u32_at(4) as u64;
    if ck_size + 8 < HEADER {
        return Err(format!("block size {} is smaller than its header", ck_size + 8));
    }
    let version = u16::from_le_bytes([h[8], h[9]]);
    if !(0x402..=0x410).contains(&version) {
        return Err(format!("stream version 0x{version:x}"));
    }
    Ok(Block {
        size: ck_size + 8,
        version,
        index: (h[10] as u64) << 32 | u32_at(16) as u64,
        samples: u32_at(20),
        flags: u32_at(24),
    })
}

/// The custom sample rate of a block, from its `ID_SAMPLE_RATE` sub-block.
fn custom_rate(data: &[u8]) -> Option<u32> {
    let mut p = 0;
    while p + 2 <= data.len() {
        let id = data[p];
        let (words, head) = if id & 0x80 != 0 {
            let b = data.get(p + 1..p + 4)?;
            (b[0] as usize | (b[1] as usize) << 8 | (b[2] as usize) << 16, 4)
        } else {
            (data[p + 1] as usize, 2)
        };
        let body = p + head;
        // An empty sub-block with the odd-size flag set has length 0, not -1.
        let len = (words * 2).saturating_sub(if id & 0x40 != 0 { 1 } else { 0 });
        if id & 0x3f == ID_SAMPLE_RATE {
            let b = data.get(body..body + len.min(4))?;
            return Some(b.iter().rev().fold(0u32, |v, &x| v << 8 | x as u32));
        }
        p = body + words * 2;
    }
    None
}

fn read_header(src: &mut SourceFile, pos: u64, end: u64, what: &str) -> Result<Block, ParseError> {
    if end - pos < HEADER {
        return Err(ParseError::truncated(format!("{what} header at byte {pos} cut at byte {end}")));
    }
    let mut h = [0u8; HEADER as usize];
    src.read_at(pos, &mut h)?;
    let b = parse_header(&h).map_err(|e| {
        if e.starts_with("stream version") {
            ParseError::new(ErrorCode::UnsupportedCodecVariant, format!("{what} at byte {pos}: {e}"))
        } else {
            ParseError::invalid(format!("{what} at byte {pos}: {e}"))
        }
    })?;
    if b.size > MAX_BLOCK {
        return Err(ParseError::unsupported(format!("{what} at byte {pos} is {} bytes", b.size)));
    }
    if b.size > end - pos {
        return Err(ParseError::truncated(format!("{what} at byte {pos} cut at byte {end}")));
    }
    Ok(b)
}

pub struct WavPack;

impl Parser for WavPack {
    fn name(&self) -> &'static str {
        "vmkv-parser-wavpack"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn inputs(&self) -> (usize, usize) {
        (1, 2)
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let range = tags::audio_range(ctx.source(0))?;
        let correction = ctx.sources().len() == 2;
        let wvc_range = if correction { Some(tags::audio_range(ctx.source(1))?) } else { None };
        let mut wvc_pos = wvc_range.map_or(0, |r| r.start);

        let mut pos = range.start;
        let mut first: Option<(Block, u32, u64)> = None; // block, rate, channels
        let mut timeline: Option<Timeline> = None;
        let mut next_index = 0u64;
        let mut count = 0u64;
        let flags = Flags::NONE.with(Flag::RandomAccess);

        while pos < range.end {
            // One frame: blocks up to FINAL_BLOCK.
            let frame_start = pos;
            let mut blocks: Vec<(u64, Block)> = Vec::new();
            loop {
                let b = read_header(ctx.source(0), pos, range.end, &format!("frame {count} block"))?;
                if blocks.is_empty() && b.flags & INITIAL_BLOCK == 0 {
                    return Err(ParseError::invalid(format!(
                        "block at byte {pos} starts a frame without INITIAL_BLOCK"
                    )));
                }
                if let Some(&(_, f)) = blocks.first() {
                    if b.flags & INITIAL_BLOCK != 0 || (b.index, b.samples) != (f.index, f.samples) {
                        return Err(ParseError::invalid(format!(
                            "frame {count} at byte {frame_start} has no FINAL_BLOCK"
                        )));
                    }
                }
                if b.flags & DSD_FLAG != 0 {
                    return Err(ParseError::unsupported(format!("block at byte {pos} holds DSD audio")));
                }
                blocks.push((pos, b));
                pos += b.size;
                if b.flags & FINAL_BLOCK != 0 {
                    break;
                }
                if pos >= range.end {
                    return Err(ParseError::truncated(format!("frame {count} cut at byte {}", range.end)));
                }
            }
            let head = blocks[0].1;

            // Correction blocks, one per block of the frame.
            let mut corr: Vec<(u64, Block)> = Vec::new();
            if let Some(r) = wvc_range {
                for &(_, b) in &blocks {
                    if b.samples == 0 {
                        break;
                    }
                    let c = read_header(ctx.source(1), wvc_pos, r.end, &format!("frame {count} correction block"))?;
                    if (c.index, c.samples, c.flags) != (b.index, b.samples, b.flags) {
                        return Err(ParseError::invalid(format!(
                            "correction block at byte {wvc_pos} does not match frame {count} (sample {})",
                            b.index
                        )));
                    }
                    corr.push((wvc_pos, c));
                    wvc_pos += c.size;
                }
            }
            if head.samples == 0 {
                // Metadata only: no audio, not a unit.
                continue;
            }
            if correction && head.flags & HYBRID_FLAG == 0 {
                return Err(ParseError::invalid(format!("a .wvc was given but frame {count} is not hybrid")));
            }

            let channels: u64 = blocks.iter().map(|(_, b)| b.channels()).sum();
            let rate = match head.rate_index() {
                15 => {
                    let mut data = vec![0u8; (head.size - HEADER) as usize];
                    ctx.source(0).read_at(blocks[0].0 + HEADER, &mut data)?;
                    custom_rate(&data).filter(|&r| r > 0).ok_or_else(|| {
                        ParseError::new(
                            ErrorCode::MissingInitializationData,
                            format!("frame {count}: custom sample rate without its sub-block"),
                        )
                    })?
                }
                i => RATES[i as usize],
            };
            match first {
                None => {
                    first = Some((head, rate, channels));
                    timeline = Some(Timeline::new(Rational::new(rate as i64, 1), head.index as i128)?);
                    next_index = head.index;
                }
                Some((f, r, c)) => {
                    let (fb, hb) = (f.flags & 3, head.flags & 3);
                    if (r, c, fb, f.flags & FLOAT_DATA) != (rate, channels, hb, head.flags & FLOAT_DATA) {
                        return Err(ParseError::new(
                            ErrorCode::InconsistentTrackParameters,
                            format!(
                                "frame {count} at byte {frame_start} changes sample rate, channels or sample format"
                            ),
                        ));
                    }
                }
            }
            if head.index != next_index {
                return Err(ParseError::invalid(format!(
                    "frame {count} at byte {frame_start} starts at sample {} instead of {next_index}",
                    head.index
                )));
            }
            next_index = head.index + head.samples as u64;

            let size_le = |b: &Block| Chunk::inline(((b.size - HEADER) as u32).to_le_bytes().to_vec());
            let mut payload = Vec::new();
            let mut addition = Vec::new();
            if let [(p, b)] = blocks[..] {
                payload.push(Chunk::src(0, p + 20, b.size - 20));
                if let [(cp, c)] = corr[..] {
                    addition.push(Chunk::src(1, cp + 28, c.size - 28));
                }
            } else {
                for (i, &(p, b)) in blocks.iter().enumerate() {
                    let from = if i == 0 { 20 } else { 24 };
                    payload.extend([
                        Chunk::src(0, p + from, HEADER - from),
                        size_le(&b),
                        Chunk::src(0, p + HEADER, b.size - HEADER),
                    ]);
                }
                for &(cp, c) in &corr {
                    addition.extend([
                        Chunk::src(1, cp + 28, 4),
                        size_le(&c),
                        Chunk::src(1, cp + HEADER, c.size - HEADER),
                    ]);
                }
            }
            let (pts, dur) = timeline.as_mut().expect("set with the first frame").advance(head.samples as i128)?;
            let mut unit = Unit::new(pts, dur, flags, payload);
            if !addition.is_empty() {
                unit.block_additions.push(BlockAddition { id: 1, data: addition });
            }
            ctx.emit(&unit)?;
            count += 1;
        }
        if let Some(r) = wvc_range {
            if wvc_pos < r.end {
                return Err(ParseError::invalid(format!("the .wvc has data past byte {wvc_pos} that no frame uses")));
            }
        }

        let Some((first, rate, channels)) = first else {
            return Err(ParseError::invalid("no WavPack audio blocks"));
        };
        let mut track = Track::new(TrackType::Audio, "A_WAVPACK4");
        track.codec_private = Some(vec![Chunk::inline(first.version.to_le_bytes().to_vec())]);
        let mut audio = Audio::new(Rational::new(rate as i64, 1), channels);
        audio.bit_depth = Some(if first.flags & FLOAT_DATA != 0 { 32 } else { ((first.flags & 3) as u64 + 1) * 8 });
        track.audio = Some(audio);
        if correction {
            track.block_addition_mappings.push(BlockAdditionMapping {
                id_value: None,
                name: None,
                kind: 1,
                extra_data: None,
            });
        }
        Ok(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_header() {
        // The first block header of a WavPack 5.9.0 stereo 16-bit 44.1 kHz file.
        let h: [u8; 32] = [
            0x77, 0x76, 0x70, 0x6b, 0x44, 0x14, 0, 0, 0x10, 0x04, 0, 0, 0x44, 0xac, 0, 0, 0, 0, 0, 0, 0x11, 0x2b, 0, 0,
            0x01, 0x18, 0xbc, 0x54, 0x4a, 0x65, 0xa0, 0xb0,
        ];
        let b = parse_header(&h).unwrap();
        assert_eq!(b, Block { size: 0x144c, version: 0x410, index: 0, samples: 11025, flags: 0x54bc_1801 });
        assert_eq!((b.channels(), RATES[b.rate_index() as usize]), (2, 44100));
        assert_ne!(b.flags & INITIAL_BLOCK, 0);
        assert_ne!(b.flags & FINAL_BLOCK, 0);
        let mut v = h;
        v[8] = 0x01;
        assert!(parse_header(&v).unwrap_err().contains("stream version 0x401"));
    }

    #[test]
    fn custom_rate_sub_block() {
        // ID_SAMPLE_RATE with the odd-size flag: 3 bytes, 0x01e078 = 123000.
        let data = [0x0a, 1, 0xaa, 0xbb, ID_SAMPLE_RATE | 0x40, 2, 0x78, 0xe0, 0x01, 0];
        assert_eq!(custom_rate(&data), Some(123000));
        assert_eq!(custom_rate(&[0x0a, 1, 0, 0]), None);
        // Zero words with the odd-size flag: a stress-found underflow.
        assert_eq!(custom_rate(&[ID_SAMPLE_RATE | 0x40, 0]), Some(0));
    }
}
