//! H.264 in Annex B (`V_MPEG4/ISO/AVC`).
//!
//! - A Matroska frame is an access unit (one coded picture), which can
//!   carry several NAL units; a frame is not a NAL.
//! - `codec_private` is the AVCDecoderConfigurationRecord the Matroska AVC
//!   mapping expects: version, profile/level, a 4-byte length size, then
//!   the SPS and PPS referenced from the source. Exactly one SPS id and one
//!   PPS id are supported for the whole track (decision, see
//!   `docs/spec/DECISIONS.md`); a stream that defines a second, distinct id
//!   of either fails with `UNSUPPORTED_FEATURE` rather than silently
//!   describing only one of them.
//! - Frames are held in memory and written in a second pass: `pts`/
//!   `duration` follow presentation order (picture order count), not file
//!   order, so B-frame reordering cannot be resolved until every access
//!   unit has been seen.
//! - Interlaced video, `pic_order_cnt_type` 1, slice groups (FMO), redundant
//!   coded slices and 4:2:2/4:4:4 chroma are rejected with
//!   `UNSUPPORTED_FEATURE`: each needs state or fields this parser does not
//!   track.

pub mod access_unit;
pub mod annexb;
pub mod bits;
pub mod params;

use std::collections::BTreeMap;

use access_unit::{AccessUnit, AuBuilder};
use params::{Pps, Sps};
use vtj::cli::{ParseError, FRAME_RATE};
use vtj::source::SourceFile;
use vtj::*;

/// A parser-internal rejection becomes `UNSUPPORTED_FEATURE` when its
/// message says so (every deliberate scope limit in `params`/`access_unit`
/// is worded this way); anything else is a structural `INVALID_BITSTREAM`.
fn map_err(message: String) -> ParseError {
    if message.contains("not supported") {
        ParseError::unsupported(message)
    } else {
        ParseError::invalid(message)
    }
}

/// Reads a whole NAL unit's on-disk bytes, header included, untouched
/// (emulation prevention and all): what a `src` chunk must reference,
/// verbatim, for either `codec_private` or a unit's own payload.
fn read_nal(src: &mut SourceFile, nal: annexb::Nal) -> Result<Vec<u8>, ParseError> {
    let mut full = vec![0u8; nal.length as usize];
    src.read_at(nal.offset, &mut full)?;
    Ok(full)
}

/// The RBSP exp-golomb fields are read from: the one-byte NAL header
/// stripped, emulation prevention removed. A parsing-only copy; `full`
/// (what `read_nal` returns) is what ever gets stored or referenced.
fn rbsp_of(full: &[u8]) -> Vec<u8> {
    bits::remove_emulation_prevention(&full[1..])
}

/// The AVCDecoderConfigurationRecord-style chain the spec's own example
/// documents: `configurationVersion`, `AVCProfileIndication`,
/// `profile_compatibility` and `AVCLevelIndication` come straight from the
/// SPS NAL's own bytes (right after its one-byte header, identical in both
/// syntaxes); the length size is fixed at 4 (`0xFF`); exactly one SPS and
/// one PPS are referenced, each as its whole on-disk NAL unit (decision,
/// cross-checked against `ffmpeg`'s own `avcC` box for this session's real
/// fixture, byte for byte).
fn codec_private(sps_nal: &[u8], sps_offset: u64, pps_nal: &[u8], pps_offset: u64) -> Result<DataChain, ParseError> {
    let too_long = |what: &str| {
        ParseError::new(ErrorCode::UnrepresentableInVmkv, format!("{what} is too long for its 16-bit length field"))
    };
    let sps_len = u16::try_from(sps_nal.len()).map_err(|_| too_long("SPS"))?;
    let pps_len = u16::try_from(pps_nal.len()).map_err(|_| too_long("PPS"))?;
    let mut head = vec![1u8, sps_nal[1], sps_nal[2], sps_nal[3], 0xFF, 0xE1];
    head.extend(sps_len.to_be_bytes());
    let mut tail_head = vec![1u8];
    tail_head.extend(pps_len.to_be_bytes());
    Ok(vec![
        Chunk::inline(head),
        Chunk::src(0, sps_offset, sps_nal.len() as u64),
        Chunk::inline(tail_head),
        Chunk::src(0, pps_offset, pps_nal.len() as u64),
    ])
}

pub struct H264;

impl Parser for H264 {
    fn name(&self) -> &'static str {
        "vmkv-parser-h264"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn params(&self) -> &'static [vtj::cli::ParamSpec] {
        &[FRAME_RATE]
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let nals = annexb::scan(ctx.source(0))?;

        let mut sps_map: BTreeMap<u32, (Sps, Vec<u8>, u64)> = BTreeMap::new();
        let mut pps_map: BTreeMap<u32, (Pps, Vec<u8>, u64)> = BTreeMap::new();
        let (mut active_sps_id, mut active_pps_id): (Option<u32>, Option<u32>) = (None, None);
        let mut builder = AuBuilder::default();
        let mut aus: Vec<AccessUnit> = Vec::new();

        for &nal in &nals {
            let mut header = [0u8; 1];
            ctx.source(0).read_at(nal.offset, &mut header)?;
            let nal_type = header[0] & 0x1f;
            let nal_ref_idc = (header[0] >> 5) & 3;

            match nal_type {
                access_unit::NAL_SPS => {
                    if let Some(au) = builder.flush() {
                        aus.push(au);
                    }
                    let full = read_nal(ctx.source(0), nal)?;
                    let sps = params::parse_sps(&rbsp_of(&full)).map_err(map_err)?;
                    let id = sps.seq_parameter_set_id;
                    if active_sps_id.is_some_and(|a| a != id) {
                        return Err(ParseError::unsupported("a second, distinct SPS id is not supported"));
                    }
                    if let Some((_, existing, _)) = sps_map.get(&id) {
                        if *existing != full {
                            return Err(ParseError::new(
                                ErrorCode::InconsistentTrackParameters,
                                format!("SPS {id} changed mid-stream"),
                            ));
                        }
                    } else {
                        sps_map.insert(id, (sps, full, nal.offset));
                        active_sps_id = Some(id);
                    }
                }
                access_unit::NAL_PPS => {
                    if let Some(au) = builder.flush() {
                        aus.push(au);
                    }
                    let full = read_nal(ctx.source(0), nal)?;
                    let pps = params::parse_pps(&rbsp_of(&full)).map_err(map_err)?;
                    let id = pps.pic_parameter_set_id;
                    if active_pps_id.is_some_and(|a| a != id) {
                        return Err(ParseError::unsupported("a second, distinct PPS id is not supported"));
                    }
                    if let Some((_, existing, _)) = pps_map.get(&id) {
                        if *existing != full {
                            return Err(ParseError::new(
                                ErrorCode::InconsistentTrackParameters,
                                format!("PPS {id} changed mid-stream"),
                            ));
                        }
                    } else {
                        pps_map.insert(id, (pps, full, nal.offset));
                        active_pps_id = Some(id);
                    }
                }
                access_unit::NAL_SLICE_IDR | access_unit::NAL_SLICE_NON_IDR => {
                    let is_idr = nal_type == access_unit::NAL_SLICE_IDR;
                    let rbsp = rbsp_of(&read_nal(ctx.source(0), nal)?);
                    let completed = builder
                        .feed_slice((nal.offset, nal.length), is_idr, nal_ref_idc, &rbsp, |pps_id| {
                            let (pps, ..) = pps_map.get(&pps_id)?;
                            let (sps, ..) = sps_map.get(&pps.seq_parameter_set_id)?;
                            Some((*sps, *pps))
                        })
                        .map_err(map_err)?;
                    if let Some(au) = completed {
                        aus.push(au);
                    }
                }
                access_unit::NAL_AUD => {
                    if let Some(au) = builder.flush() {
                        aus.push(au);
                    }
                }
                access_unit::NAL_FILLER | 10 | 11 => {
                    // Filler data, end of sequence, end of stream: carry no
                    // picture content and are dropped, not an error.
                }
                access_unit::NAL_SEI => builder.feed_other((nal.offset, nal.length)),
                other => {
                    return Err(ParseError::unsupported(format!("NAL unit type {other} is not supported")));
                }
            }
        }
        if let Some(au) = builder.flush() {
            aus.push(au);
        }

        let sps_id =
            active_sps_id.ok_or_else(|| ParseError::new(ErrorCode::MissingInitializationData, "no SPS found"))?;
        let pps_id =
            active_pps_id.ok_or_else(|| ParseError::new(ErrorCode::MissingInitializationData, "no PPS found"))?;
        if aus.is_empty() {
            return Err(ParseError::invalid("no slices found"));
        }
        let (sps, sps_bytes, sps_offset) = sps_map.get(&sps_id).expect("set together with active_sps_id");
        let (_, pps_bytes, pps_offset) = pps_map.get(&pps_id).expect("set together with active_pps_id");

        let frame_rate = match ctx.param_rational("frame_rate") {
            Some(r) => r,
            None => match sps.vui_timing {
                Some((num_units, scale)) if num_units > 0 => Rational::new(scale as i64, 2 * num_units as i64),
                _ => {
                    return Err(ParseError::new(
                        ErrorCode::TimingRequired,
                        "the stream carries no timing; pass --frame-rate",
                    ))
                }
            },
        };

        // Presentation order (picture order count) gives each access
        // unit's rank; `pts` at a constant frame rate is just that rank's
        // tick (the spec's own H.264 example uses this same construction).
        let mut presentation_order: Vec<usize> = (0..aus.len()).collect();
        presentation_order.sort_by_key(|&i| aus[i].poc);
        let mut rank = vec![0i128; aus.len()];
        for (r, &i) in presentation_order.iter().enumerate() {
            rank[i] = r as i128;
        }
        let pts: Vec<i64> = rank.iter().map(|&r| ticks_to_ns(r, frame_rate)).collect::<Result<_, _>>()?;
        let end_ns = ticks_to_ns(aus.len() as i128, frame_rate)?;
        let durations = durations_from_pts(&pts, Some(end_ns))?;

        for (i, au) in aus.iter().enumerate() {
            let flags = if au.is_idr { Flags::NONE.with(Flag::RandomAccess) } else { Flags::NONE };
            let payload: DataChain = au
                .nals
                .iter()
                .flat_map(|&(off, len)| [Chunk::inline((len as u32).to_be_bytes()), Chunk::src(0, off, len)])
                .collect();
            ctx.emit(&Unit::new(pts[i], durations[i], flags, payload))?;
        }

        let mut track = Track::new(TrackType::Video, "V_MPEG4/ISO/AVC");
        track.codec_private = Some(codec_private(sps_bytes, *sps_offset, pps_bytes, *pps_offset)?);
        track.video = Some(Video::new(sps.pic_width, sps.pic_height));
        Ok(track)
    }
}
