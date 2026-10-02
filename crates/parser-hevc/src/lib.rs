//! H.265/HEVC in Annex B (`V_MPEGH/ISO/HEVC`), decision 74.
//!
//! - A Matroska frame is an access unit (one coded picture with its SEI),
//!   each NAL unit behind a 4-byte length. VPS, SPS and PPS go into
//!   `codec_private`, never into a unit; access unit delimiters, end of
//!   sequence/bitstream and filler data are dropped.
//! - `codec_private` is the HEVCDecoderConfigurationRecord (`hvcC`) with one
//!   array each for the VPS, SPS and PPS (complete: none stays in-band),
//!   referenced from the source. Exactly one VPS, SPS and PPS id are
//!   supported; repeats must be identical.
//! - Presentation order is (period, picture order count): POC restarts at
//!   every random access picture with NoRaslOutputFlag (IDR, BLA, the first
//!   CRA or one after an end of sequence), and every picture before it in
//!   decode order is presented first. `pts` is the rank in that order at
//!   the frame rate, which comes from `--frame-rate` or from VUI timing
//!   whose HRD parameters set `fixed_pic_rate_general_flag`.
//! - IRAP pictures are the random access points.
//! - Rejected (`UNSUPPORTED_FEATURE`): layers other than the base layer,
//!   field-coded video, pictures that are not output, RASL pictures of a
//!   leading CRA, a second VPS/SPS/PPS id, reserved and unspecified NAL
//!   unit types (Dolby Vision included). Out-of-range fields and malformed
//!   headers are `INVALID_BITSTREAM`.

pub mod access_unit;
pub mod annexb;
pub mod bits;
pub mod colour;
pub mod params;

use std::collections::BTreeMap;

use access_unit::{AccessUnit, AuBuilder};
use params::{Pps, Sps, Vps};
use vtj::cli::{ParseError, FRAME_RATE};
use vtj::source::SourceFile;
use vtj::*;

/// Bytes of a slice segment read to parse its header up to the POC.
const SLICE_HEADER_BYTES: u64 = 1024;
/// Prefix SEI NAL units up to this size are read for HDR metadata.
const SEI_READ_BYTES: u64 = 1 << 20;

fn map_err(message: String) -> ParseError {
    if message.contains("not supported") {
        ParseError::unsupported(message)
    } else {
        ParseError::invalid(message)
    }
}

fn read_nal(src: &mut SourceFile, nal: annexb::Nal, max: u64) -> Result<Vec<u8>, ParseError> {
    let mut full = vec![0u8; nal.length.min(max) as usize];
    src.read_at(nal.offset, &mut full)?;
    Ok(full)
}

fn read_parameter_set(src: &mut SourceFile, nal: annexb::Nal, what: &str) -> Result<Vec<u8>, ParseError> {
    if nal.length > u16::MAX as u64 {
        return Err(ParseError::new(
            ErrorCode::UnrepresentableInVmkv,
            format!("{what} at byte {} is {} bytes, too long for its 16-bit length field", nal.offset, nal.length),
        ));
    }
    read_nal(src, nal, u16::MAX as u64)
}

/// The RBSP after the 2-byte NAL header, emulation prevention removed.
fn rbsp_of(full: &[u8]) -> Vec<u8> {
    bits::remove_emulation_prevention(full.get(2..).unwrap_or(&[]))
}

/// One parameter set: its parsed form, its bytes and where they are.
type Stored<T> = (T, Vec<u8>, u64);

/// Keeps the one id a stream may use for a parameter set kind.
fn store<T>(
    map: &mut Option<(u32, Stored<T>)>,
    id: u32,
    parsed: T,
    full: Vec<u8>,
    at: u64,
    what: &str,
) -> Result<(), ParseError> {
    match map {
        None => {
            *map = Some((id, (parsed, full, at)));
            Ok(())
        }
        Some((existing, _)) if *existing != id => {
            Err(ParseError::unsupported(format!("a second, distinct {what} id is not supported")))
        }
        Some((_, (_, bytes, _))) if *bytes != full => {
            Err(ParseError::new(ErrorCode::InconsistentTrackParameters, format!("{what} {id} changed mid-stream")))
        }
        Some(_) => Ok(()),
    }
}

/// The HEVCDecoderConfigurationRecord (ISO/IEC 14496-15 §8.3.3.1).
/// `arrays`: NAL type, bytes, offset and whether every NAL of that type is
/// in the record (array_completeness).
fn hvcc(vps: &Vps, sps: &Sps, pps: &Pps, arrays: [(u8, &[u8], u64, bool); 3]) -> Result<DataChain, ParseError> {
    let parallelism = if sps.min_spatial_segmentation_idc == 0 {
        0
    } else {
        match (pps.entropy_coding_sync_enabled, pps.tiles_enabled) {
            (true, true) => 0,
            (true, false) => 3,
            (false, true) => 2,
            (false, false) => 1,
        }
    };
    let layers = vps.max_sub_layers.max(sps.max_sub_layers);
    let mut head = vec![1u8];
    head.extend(sps.profile_tier_level);
    head.extend([
        0xf0 | (sps.min_spatial_segmentation_idc >> 8) as u8,
        sps.min_spatial_segmentation_idc as u8,
        0xfc | parallelism,
        0xfc | sps.chroma_format_idc as u8,
        0xf8 | sps.bit_depth_luma_minus8 as u8,
        0xf8 | sps.bit_depth_chroma_minus8 as u8,
        0,
        0, // avgFrameRate: unspecified
        (layers as u8) << 3 | (sps.temporal_id_nesting as u8) << 2 | 3,
        3,
    ]);
    let mut chain = vec![];
    for (kind, bytes, at, complete) in arrays {
        let len = u16::try_from(bytes.len()).expect("checked when read");
        head.push(if complete { 0x80 } else { 0 } | kind);
        head.extend(1u16.to_be_bytes());
        head.extend(len.to_be_bytes());
        chain.push(Chunk::inline(std::mem::take(&mut head)));
        chain.push(Chunk::src(0, at, bytes.len() as u64));
    }
    Ok(chain)
}

pub struct Hevc;

impl Parser for Hevc {
    fn name(&self) -> &'static str {
        "vmkv-parser-hevc"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn params(&self) -> &'static [vtj::cli::ParamSpec] {
        &[FRAME_RATE]
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let nals = annexb::scan(ctx.source(0))?;
        let mut vps: Option<(u32, Stored<Vps>)> = None;
        let mut sps_set: Option<(u32, Stored<Sps>)> = None;
        let mut pps_set: Option<(u32, Stored<Pps>)> = None;
        let mut sps_by_id: BTreeMap<u32, Sps> = BTreeMap::new();
        let mut pps_by_id: BTreeMap<u32, Pps> = BTreeMap::new();
        let mut builder = AuBuilder::new();
        let mut hdr = colour::Hdr::default();
        let mut active_pps: Option<Vec<u8>> = None;
        let mut pps_in_band = false;
        let mut aus: Vec<AccessUnit> = Vec::new();

        for &nal in &nals {
            if nal.length < 2 {
                return Err(ParseError::invalid(format!("NAL unit at byte {} is shorter than its header", nal.offset)));
            }
            let mut h = [0u8; 2];
            ctx.source(0).read_at(nal.offset, &mut h)?;
            if h[0] & 0x80 != 0 {
                return Err(ParseError::invalid(format!("NAL unit at byte {} has forbidden_zero_bit set", nal.offset)));
            }
            let nal_type = (h[0] >> 1) & 0x3f;
            let layer = ((h[0] & 1) << 5) | (h[1] >> 3);
            let tid_plus1 = h[1] & 7;
            if tid_plus1 == 0 {
                return Err(ParseError::invalid(format!(
                    "NAL unit at byte {} has nuh_temporal_id_plus1 0",
                    nal.offset
                )));
            }
            if layer != 0 {
                return Err(ParseError::unsupported(format!(
                    "NAL unit at byte {} is in layer {layer}; layers other than the base layer are not supported",
                    nal.offset
                )));
            }
            let at = nal.offset;
            let flushed = match nal_type {
                t if access_unit::is_vcl(t) => {
                    if nal.length > u32::MAX as u64 {
                        return Err(ParseError::new(
                            ErrorCode::UnrepresentableInVmkv,
                            format!("slice at byte {at} is too long for a 4-byte NAL length"),
                        ));
                    }
                    let rbsp = rbsp_of(&read_nal(ctx.source(0), nal, SLICE_HEADER_BYTES)?);
                    builder
                        .feed_vcl((nal.offset, nal.length), t, tid_plus1 - 1, &rbsp, |pps_id| {
                            let pps = *pps_by_id.get(&pps_id)?;
                            Some((*sps_by_id.get(&pps.sps_id)?, pps))
                        })
                        .map_err(|e| map_err(format!("slice at byte {at}: {e}")))?
                }
                access_unit::VPS => {
                    let full = read_parameter_set(ctx.source(0), nal, "VPS")?;
                    let v =
                        params::parse_vps(&rbsp_of(&full)).map_err(|e| map_err(format!("VPS at byte {at}: {e}")))?;
                    store(&mut vps, v.vps_id, v, full, at, "VPS")?;
                    builder.feed_prefix(None)
                }
                access_unit::SPS => {
                    let full = read_parameter_set(ctx.source(0), nal, "SPS")?;
                    let s =
                        params::parse_sps(&rbsp_of(&full)).map_err(|e| map_err(format!("SPS at byte {at}: {e}")))?;
                    sps_by_id.insert(s.sps_id, s);
                    store(&mut sps_set, s.sps_id, s, full, at, "SPS")?;
                    builder.feed_prefix(None)
                }
                access_unit::PPS => {
                    let full = read_parameter_set(ctx.source(0), nal, "PPS")?;
                    let p =
                        params::parse_pps(&rbsp_of(&full)).map_err(|e| map_err(format!("PPS at byte {at}: {e}")))?;
                    pps_by_id.insert(p.pps_id, p);
                    match &pps_set {
                        // A PPS may change between pictures (decision 75):
                        // a new content stays in-band, in front of the
                        // pictures that use it; a repeat of the active one
                        // is dropped.
                        Some((id, _)) if *id == p.pps_id => {
                            if active_pps.as_ref() != Some(&full) {
                                pps_in_band = true;
                                active_pps = Some(full);
                                builder.feed_prefix(Some((nal.offset, nal.length)))
                            } else {
                                builder.feed_prefix(None)
                            }
                        }
                        _ => {
                            store(&mut pps_set, p.pps_id, p, full.clone(), at, "PPS")?;
                            active_pps = Some(full);
                            builder.feed_prefix(None)
                        }
                    }
                }
                access_unit::AUD => builder.feed_prefix(None),
                access_unit::EOS | access_unit::EOB => builder.end_of_sequence(),
                access_unit::FD => None,
                access_unit::PREFIX_SEI => {
                    if nal.length > u32::MAX as u64 {
                        return Err(ParseError::new(
                            ErrorCode::UnrepresentableInVmkv,
                            format!("SEI at byte {at} is too long for a 4-byte NAL length"),
                        ));
                    }
                    // Static HDR metadata: an SEI too large to read whole is
                    // not one of those few-byte messages.
                    if nal.length <= SEI_READ_BYTES {
                        let rbsp = rbsp_of(&read_nal(ctx.source(0), nal, SEI_READ_BYTES)?);
                        colour::read_sei(&rbsp, &mut hdr).map_err(|e| {
                            let code = if e.contains("changes") {
                                ErrorCode::InconsistentTrackParameters
                            } else {
                                ErrorCode::InvalidBitstream
                            };
                            ParseError::new(code, format!("SEI at byte {at}: {e}"))
                        })?;
                    }
                    builder.feed_prefix(Some((nal.offset, nal.length)))
                }
                access_unit::SUFFIX_SEI => {
                    builder
                        .feed_suffix((nal.offset, nal.length))
                        .map_err(|e| map_err(format!("NAL unit at byte {at}: {e}")))?;
                    None
                }
                other => {
                    return Err(ParseError::unsupported(format!(
                        "NAL unit type {other} at byte {at} is not supported"
                    )));
                }
            };
            if let Some(au) = flushed {
                aus.push(au);
            }
        }
        if let Some(au) = builder.flush() {
            aus.push(au);
        }

        let missing = |what: &str| ParseError::new(ErrorCode::MissingInitializationData, format!("no {what} found"));
        let (_, (vps, vps_bytes, vps_at)) = vps.ok_or_else(|| missing("VPS"))?;
        let (_, (sps, sps_bytes, sps_at)) = sps_set.ok_or_else(|| missing("SPS"))?;
        let (_, (pps, pps_bytes, pps_at)) = pps_set.ok_or_else(|| missing("PPS"))?;
        if aus.is_empty() {
            return Err(ParseError::invalid("no slices found"));
        }
        if sps.vps_id != vps.vps_id || pps.sps_id != sps.sps_id {
            return Err(ParseError::invalid("the SPS and PPS do not refer to the stream's VPS and SPS"));
        }

        let frame_rate = match ctx.param_rational("frame_rate") {
            Some(r) => r,
            None => match sps.vui_timing {
                Some(t) => match t.fixed_ticks {
                    Some(n) => Rational::new(t.time_scale as i64, t.num_units_in_tick as i64 * n as i64),
                    None => {
                        return Err(ParseError::new(
                            ErrorCode::TimingRequired,
                            "the VUI timing does not fix the picture rate (no fixed_pic_rate_general_flag); pass --frame-rate",
                        ))
                    }
                },
                None => return Err(ParseError::new(ErrorCode::TimingRequired, "the stream carries no timing; pass --frame-rate")),
            },
        };

        let mut period = Vec::with_capacity(aus.len());
        let mut current = 0usize;
        for (i, au) in aus.iter().enumerate() {
            if au.new_period && i > 0 {
                current += 1;
            }
            period.push(current);
        }
        let mut order: Vec<usize> = (0..aus.len()).collect();
        order.sort_by_key(|&i| (period[i], aus[i].poc));
        let mut rank = vec![0i128; aus.len()];
        for (r, &i) in order.iter().enumerate() {
            rank[i] = r as i128;
        }
        let pts: Vec<i64> = rank.iter().map(|&r| ticks_to_ns(r, frame_rate)).collect::<Result<_, _>>()?;
        let end_ns = ticks_to_ns(aus.len() as i128, frame_rate)?;
        let durations = durations_from_pts(&pts, Some(end_ns))?;

        for (i, au) in aus.iter().enumerate() {
            let flags = if au.irap { Flags::NONE.with(Flag::RandomAccess) } else { Flags::NONE };
            let payload: DataChain = au
                .nals
                .iter()
                .flat_map(|&(off, len)| [Chunk::inline((len as u32).to_be_bytes()), Chunk::src(0, off, len)])
                .collect();
            ctx.emit(&Unit::new(pts[i], durations[i], flags, payload))?;
        }

        let mut track = Track::new(TrackType::Video, "V_MPEGH/ISO/HEVC");
        track.codec_private = Some(hvcc(
            &vps,
            &sps,
            &pps,
            [
                (access_unit::VPS, &vps_bytes, vps_at, true),
                (access_unit::SPS, &sps_bytes, sps_at, true),
                (access_unit::PPS, &pps_bytes, pps_at, !pps_in_band),
            ],
        )?);
        let mut video = Video::new(sps.width, sps.height);
        video.colour =
            Some(colour::colour(sps.chroma_format_idc, sps.bit_depth_luma_minus8, sps.signal, sps.chroma_loc, &hdr));
        track.video = Some(video);
        Ok(track)
    }
}
