//! Slice header fields up to picture order count, access unit assembly, and
//! picture order count itself (ITU-T H.264 §7.3.3, a simplified subset of
//! Annex 7.4.1.2.4, and §8.2.1).

use crate::bits::BitReader;
use crate::params::{PicOrderCntType, Pps, Sps};

pub const NAL_SLICE_NON_IDR: u8 = 1;
pub const NAL_SEI: u8 = 6;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;
pub const NAL_AUD: u8 = 9;
pub const NAL_SLICE_IDR: u8 = 5;
pub const NAL_FILLER: u8 = 12;

/// The slice header fields that matter for access unit boundaries and POC;
/// everything after them (reference picture lists, weighted prediction,
/// deblocking, ...) is never read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SliceKey {
    pic_parameter_set_id: u32,
    frame_num: u32,
    is_idr: bool,
    idr_pic_id: u32,
    poc_lsb: u32,
    is_reference: bool,
}

/// Parses a slice NAL's RBSP (NAL header byte stripped, emulation
/// prevention removed) up to and including picture order count.
///
/// `pic_parameter_set_id` is itself one of the first fields read, before
/// anything that depends on *which* PPS/SPS apply — so they cannot be
/// passed in ahead of time. `lookup` resolves the id, once read, to both.
fn parse_slice_key(
    is_idr: bool,
    nal_ref_idc: u8,
    rbsp: &[u8],
    lookup: impl FnOnce(u32) -> Option<(Sps, Pps)>,
) -> Result<(SliceKey, Sps), String> {
    let mut r = BitReader::new(rbsp);
    let _first_mb_in_slice = r.ue()?;
    let slice_type = r.ue()?;
    if slice_type > 9 {
        return Err(format!("slice_type {slice_type} is out of range"));
    }
    let (is_p, is_b, is_sp) = (slice_type % 5 == 0, slice_type % 5 == 1, slice_type % 5 == 3);
    let pic_parameter_set_id = r.ue()?;
    if pic_parameter_set_id > 255 {
        return Err(format!("pic_parameter_set_id {pic_parameter_set_id} is out of range (at most 255)"));
    }
    let pic_parameter_set_id = pic_parameter_set_id as u32;
    let (sps, pps) =
        lookup(pic_parameter_set_id).ok_or_else(|| format!("slice refers to unknown PPS {pic_parameter_set_id}"))?;
    let frame_num = r.u(sps.log2_max_frame_num)? as u32;
    // frame_mbs_only_flag is required true by `parse_sps` (interlaced is
    // rejected), so field_pic_flag/bottom_field_flag are never present.
    let idr_pic_id = if is_idr {
        let v = r.ue()?;
        if v > 65535 {
            return Err(format!("idr_pic_id {v} is out of range (at most 65535)"));
        }
        v as u32
    } else {
        0
    };
    // bottom_field_pic_order_in_frame_present_flag is rejected by
    // `parse_pps`, so delta_pic_order_cnt_bottom is never present.
    let poc_lsb = match sps.poc_type {
        PicOrderCntType::Type0 { log2_max_pic_order_cnt_lsb } => r.u(log2_max_pic_order_cnt_lsb)? as u32,
        PicOrderCntType::Type2 => 0,
    };
    if pps.redundant_pic_cnt_present_flag {
        let redundant_pic_cnt = r.ue()?;
        if redundant_pic_cnt != 0 {
            return Err("redundant coded slices are not supported".into());
        }
    }
    if nal_ref_idc != 0 {
        skip_to_ref_pic_marking(&mut r, &sps, &pps, is_p, is_b, is_sp)?;
        check_ref_pic_marking(&mut r, is_idr)?;
    }
    let key = SliceKey { pic_parameter_set_id, frame_num, is_idr, idr_pic_id, poc_lsb, is_reference: nal_ref_idc != 0 };
    Ok((key, sps))
}

/// Reads past the slice header fields between the POC fields and
/// `dec_ref_pic_marking` (§7.3.3): `direct_spatial_mv_pred_flag`, the
/// active reference counts, `ref_pic_list_modification` and
/// `pred_weight_table`. Nothing in them is kept; they only have to be read
/// to reach the memory management operations.
fn skip_to_ref_pic_marking(
    r: &mut BitReader,
    sps: &Sps,
    pps: &Pps,
    is_p: bool,
    is_b: bool,
    is_sp: bool,
) -> Result<(), String> {
    if is_b {
        r.u1()?; // direct_spatial_mv_pred_flag
    }
    let (mut l0, mut l1) = (pps.num_ref_idx_l0_default_active_minus1, pps.num_ref_idx_l1_default_active_minus1);
    // num_ref_idx_active_override_flag
    if (is_p || is_sp || is_b) && r.u1()? {
        l0 = ue_max(r, 31, "num_ref_idx_l0_active_minus1")?;
        if is_b {
            l1 = ue_max(r, 31, "num_ref_idx_l1_active_minus1")?;
        }
    }
    let lists = if is_b {
        2
    } else if is_p || is_sp {
        1
    } else {
        0
    };
    for _ in 0..lists {
        if r.u1()? {
            // ref_pic_list_modification_flag_lX
            loop {
                match r.ue()? {
                    0..=2 => {
                        r.ue()?; // abs_diff_pic_num_minus1 or long_term_pic_num
                    }
                    3 => break,
                    idc => return Err(format!("modification_of_pic_nums_idc {idc} is out of range")),
                }
            }
        }
    }
    if (pps.weighted_pred_flag && (is_p || is_sp)) || (pps.weighted_bipred_idc == 1 && is_b) {
        r.ue()?; // luma_log2_weight_denom
        let chroma = sps.chroma_format_idc != 0;
        if chroma {
            r.ue()?; // chroma_log2_weight_denom
        }
        let counts: &[u32] = if is_b { &[l0, l1] } else { &[l0] };
        for &count in counts {
            for _ in 0..=count {
                if r.u1()? {
                    r.se()?; // luma_weight
                    r.se()?; // luma_offset
                }
                if chroma && r.u1()? {
                    for _ in 0..4 {
                        r.se()?; // chroma weight and offset, Cb and Cr
                    }
                }
            }
        }
    }
    Ok(())
}

/// Reads `dec_ref_pic_marking` (§7.3.3.3) and rejects memory management
/// operation 5, which resets POC and frame_num as an IDR would without
/// being one: not modelled (decision 71).
fn check_ref_pic_marking(r: &mut BitReader, is_idr: bool) -> Result<(), String> {
    if is_idr {
        r.u1()?; // no_output_of_prior_pics_flag
        r.u1()?; // long_term_reference_flag
        return Ok(());
    }
    if !r.u1()? {
        // adaptive_ref_pic_marking_mode_flag
        return Ok(());
    }
    loop {
        match r.ue()? {
            0 => return Ok(()),
            1 | 2 | 4 | 6 => {
                r.ue()?;
            }
            3 => {
                r.ue()?; // difference_of_pic_nums_minus1
                r.ue()?; // long_term_frame_idx
            }
            5 => return Err("memory management operation 5 is not supported".into()),
            op => return Err(format!("memory_management_control_operation {op} is out of range")),
        }
    }
}

fn ue_max(r: &mut BitReader, max: u64, what: &str) -> Result<u32, String> {
    let v = r.ue()?;
    if v > max {
        return Err(format!("{what} {v} is out of range (at most {max})"));
    }
    Ok(v as u32)
}

/// Running state for picture order count (§8.2.1): type 0 tracks the
/// previous reference picture's `(PicOrderCntMsb, pic_order_cnt_lsb)`; type
/// 2 tracks the previous picture's `(FrameNumOffset, frame_num)`. Only one
/// side is ever read, for the SPS's own `poc_type`, but both are kept
/// (cheap, two `i64`s) rather than branching the state's own type.
#[derive(Debug, Clone, Copy, Default)]
struct PocState {
    prev_msb: i64,
    prev_lsb: i64,
    prev_frame_num_offset: i64,
    prev_frame_num: u32,
}

fn poc_type0(max_lsb_bits: u32, key: &SliceKey, state: &mut PocState) -> i64 {
    let max_lsb: i64 = 1 << max_lsb_bits;
    let (prev_msb, prev_lsb) = if key.is_idr { (0, 0) } else { (state.prev_msb, state.prev_lsb) };
    let lsb = key.poc_lsb as i64;
    let msb = if lsb < prev_lsb && prev_lsb - lsb >= max_lsb / 2 {
        prev_msb + max_lsb
    } else if lsb > prev_lsb && lsb - prev_lsb > max_lsb / 2 {
        prev_msb - max_lsb
    } else {
        prev_msb
    };
    if key.is_reference {
        state.prev_msb = msb;
        state.prev_lsb = lsb;
    }
    msb + lsb
}

fn poc_type2(max_frame_num_bits: u32, key: &SliceKey, state: &mut PocState) -> i64 {
    let max_frame_num: i64 = 1 << max_frame_num_bits;
    let frame_num = key.frame_num as i64;
    let frame_num_offset = if key.is_idr {
        0
    } else if state.prev_frame_num as i64 > frame_num {
        state.prev_frame_num_offset + max_frame_num
    } else {
        state.prev_frame_num_offset
    };
    let poc = if key.is_idr { 0 } else { 2 * (frame_num_offset + frame_num) - i64::from(!key.is_reference) };
    state.prev_frame_num_offset = frame_num_offset;
    state.prev_frame_num = key.frame_num;
    poc
}

/// One coded picture, in decode order: the NALs that belong to it (SPS/PPS
/// excluded — those feed parameter set state, never a unit's payload), its
/// presentation-order key, and whether it is a random access point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessUnit {
    pub nals: Vec<(u64, u64)>,
    pub is_idr: bool,
    pub poc: i64,
}

/// Accumulates NAL units into access units and computes each one's POC as
/// it completes — POC needs the *previous* reference picture's state, so it
/// cannot be deferred to a later pass without keeping that state anyway.
#[derive(Default)]
pub struct AuBuilder {
    poc_state: PocState,
    current_key: Option<SliceKey>,
    current_poc: Option<i64>,
    pending: Vec<(u64, u64)>,
}

impl AuBuilder {
    /// Feeds one slice NAL. Returns the access unit that was just completed
    /// (the one before this slice), if this slice starts a new one.
    pub fn feed_slice(
        &mut self,
        nal: (u64, u64),
        is_idr: bool,
        nal_ref_idc: u8,
        rbsp: &[u8],
        lookup: impl FnOnce(u32) -> Option<(Sps, Pps)>,
    ) -> Result<Option<AccessUnit>, String> {
        let (key, sps) = parse_slice_key(is_idr, nal_ref_idc, rbsp, lookup)?;
        let starts_new = match &self.current_key {
            None => true,
            Some(prev) => {
                prev.pic_parameter_set_id != key.pic_parameter_set_id
                    || prev.frame_num != key.frame_num
                    || prev.is_reference != key.is_reference
                    || prev.is_idr != key.is_idr
                    || (key.is_idr && prev.idr_pic_id != key.idr_pic_id)
                    || prev.poc_lsb != key.poc_lsb
            }
        };
        let completed = if starts_new { self.flush() } else { None };
        if starts_new {
            let poc = match sps.poc_type {
                PicOrderCntType::Type0 { log2_max_pic_order_cnt_lsb } => {
                    poc_type0(log2_max_pic_order_cnt_lsb, &key, &mut self.poc_state)
                }
                PicOrderCntType::Type2 => poc_type2(sps.log2_max_frame_num, &key, &mut self.poc_state),
            };
            self.current_key = Some(key);
            self.current_poc = Some(poc);
        }
        self.pending.push(nal);
        Ok(completed)
    }

    /// Feeds a non-parameter-set, non-slice NAL that still carries content
    /// (SEI; filler and other markers are dropped by the caller instead).
    /// It precedes the next picture (§7.4.1.2.3): one met after a picture's
    /// slices ends that picture's access unit, which is returned, and starts
    /// the prefix of the next one.
    pub fn feed_other(&mut self, nal: (u64, u64)) -> Option<AccessUnit> {
        let completed = self.flush();
        self.pending.push(nal);
        completed
    }

    /// Ends the access unit in progress, if any (encountering a parameter
    /// set, an access unit delimiter, or the end of the file).
    pub fn flush(&mut self) -> Option<AccessUnit> {
        let key = self.current_key.take()?;
        let poc = self.current_poc.take().expect("set together with current_key");
        Some(AccessUnit { nals: std::mem::take(&mut self.pending), is_idr: key.is_idr, poc })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{parse_pps, parse_sps};
    use vtj::source::SourceFile;

    /// Drives the real fixture through scanning, SPS/PPS tracking and
    /// `AuBuilder`, mirroring (a simplified version of) what the full
    /// parser will do, to check these pieces agree with each other before
    /// lib.rs wires them into one.
    fn access_units_of(path: &std::path::Path) -> Vec<AccessUnit> {
        let mut src = SourceFile::open(0, path).unwrap();
        let nals = crate::annexb::scan(&mut src).unwrap();
        let mut sps: Option<Sps> = None;
        let mut pps: Option<Pps> = None;
        let mut builder = AuBuilder::default();
        let mut aus = Vec::new();
        for nal in &nals {
            let mut header = [0u8; 1];
            src.read_at(nal.offset, &mut header).unwrap();
            let nal_type = header[0] & 0x1f;
            let nal_ref_idc = (header[0] >> 5) & 3;
            let mut ebsp = vec![0u8; nal.length as usize - 1];
            src.read_at(nal.offset + 1, &mut ebsp).unwrap();
            let rbsp = crate::bits::remove_emulation_prevention(&ebsp);
            match nal_type {
                NAL_SPS => sps = Some(parse_sps(&rbsp).unwrap()),
                NAL_PPS => pps = Some(parse_pps(&rbsp).unwrap()),
                NAL_SLICE_IDR | NAL_SLICE_NON_IDR => {
                    let is_idr = nal_type == NAL_SLICE_IDR;
                    if let Some(completed) = builder
                        .feed_slice((nal.offset, nal.length), is_idr, nal_ref_idc, &rbsp, |_pps_id| {
                            Some((sps.unwrap(), pps.unwrap()))
                        })
                        .unwrap()
                    {
                        aus.push(completed);
                    }
                }
                NAL_AUD => {
                    if let Some(completed) = builder.flush() {
                        aus.push(completed);
                    }
                }
                NAL_FILLER => {}
                _ => {
                    if let Some(completed) = builder.feed_other((nal.offset, nal.length)) {
                        aus.push(completed);
                    }
                }
            }
        }
        if let Some(completed) = builder.flush() {
            aus.push(completed);
        }
        aus
    }

    #[test]
    fn real_fixture_access_units_and_presentation_order() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/media/h264_sample.h264");
        let aus = access_units_of(&path);
        assert_eq!(aus.len(), 20, "I + 7 P + 12 B, one slice each (ffprobe frame count)");
        assert!(aus[0].is_idr);
        assert!(aus[1..].iter().all(|a| !a.is_idr));
        // Every AU's only NAL should be its one slice (the leading SPS/PPS
        // and the single SEI before the IDR are not part of any AU's
        // slice-derived payload list by this point in the test, since this
        // driver only ever calls feed_other for NAL types other than the
        // ones matched above — there are none here besides the SEI, which
        // attaches to the IDR access unit).
        assert_eq!(aus[0].nals.len(), 2, "the leading SEI, then the IDR slice");
        assert!(aus[1..].iter().all(|a| a.nals.len() == 1));

        // Decode order is I, then six (P, B, B) groups, then a trailing P
        // (ffprobe's own frame-order dump, read in this session when the
        // fixture was generated): presentation order reorders each group's
        // P after its two B's.
        let mut expected_presentation_order = vec![0usize];
        for g in 0..6 {
            let (p, b1, b2) = (1 + 3 * g, 2 + 3 * g, 3 + 3 * g);
            expected_presentation_order.extend([b1, b2, p]);
        }
        expected_presentation_order.push(19);

        let mut by_poc: Vec<usize> = (0..aus.len()).collect();
        by_poc.sort_by_key(|&i| aus[i].poc);
        assert_eq!(by_poc, expected_presentation_order);
    }

    fn bits_to_bytes(bits: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let (mut byte, mut n) = (0u8, 0);
        for c in bits.chars() {
            byte = (byte << 1) | if c == '1' { 1 } else { 0 };
            n += 1;
            if n == 8 {
                out.push(byte);
                byte = 0;
                n = 0;
            }
        }
        if n > 0 {
            out.push(byte << (8 - n));
        }
        out
    }

    /// A minimal non-IDR, type-2-POC P slice header: first_mb_in_slice
    /// ue(0), slice_type ue(0), pic_parameter_set_id ue(0), `frame_num` in
    /// `bits` fixed-width bits, then no reference count override, no list
    /// modification and no adaptive reference marking.
    fn minimal_slice_bits(frame_num: u32, bits: u32) -> Vec<u8> {
        let s = format!("111{:0width$b}000", frame_num, width = bits as usize);
        bits_to_bytes(&s)
    }

    fn test_sps() -> Sps {
        Sps {
            seq_parameter_set_id: 0,
            profile_idc: 66,
            level_idc: 10,
            chroma_format_idc: 1,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            log2_max_frame_num: 4,
            poc_type: PicOrderCntType::Type2,
            max_num_ref_frames: 1,
            pic_width: 16,
            pic_height: 16,
            vui_timing: None,
            fixed_frame_rate: false,
            signal: None,
            chroma_loc: None,
        }
    }

    fn test_pps() -> Pps {
        Pps {
            pic_parameter_set_id: 0,
            seq_parameter_set_id: 0,
            num_ref_idx_l0_default_active_minus1: 0,
            num_ref_idx_l1_default_active_minus1: 0,
            weighted_pred_flag: false,
            weighted_bipred_idc: 0,
            redundant_pic_cnt_present_flag: false,
        }
    }

    #[test]
    fn two_slices_with_the_same_key_merge_into_one_access_unit() {
        let (sps, pps) = (test_sps(), test_pps());
        let mut b = AuBuilder::default();
        let rbsp = minimal_slice_bits(5, 4);
        assert!(
            b.feed_slice((0, 10), false, 2, &rbsp, |_| Some((sps, pps))).unwrap().is_none(),
            "first slice of the AU"
        );
        assert!(
            b.feed_slice((10, 8), false, 2, &rbsp, |_| Some((sps, pps))).unwrap().is_none(),
            "second slice, same picture"
        );
        let au = b.flush().unwrap();
        assert_eq!(au.nals, [(0, 10), (10, 8)], "both slices in one access unit");
    }

    #[test]
    fn a_different_frame_num_starts_a_new_access_unit() {
        let (sps, pps) = (test_sps(), test_pps());
        let mut b = AuBuilder::default();
        let first = minimal_slice_bits(5, 4);
        let second = minimal_slice_bits(6, 4);
        assert!(b.feed_slice((0, 10), false, 2, &first, |_| Some((sps, pps))).unwrap().is_none());
        let completed = b.feed_slice((10, 8), false, 2, &second, |_| Some((sps, pps))).unwrap();
        assert_eq!(completed.unwrap().nals, [(0, 10)]);
        assert_eq!(b.flush().unwrap().nals, [(10, 8)]);
    }

    #[test]
    fn nonzero_redundant_pic_cnt_is_rejected() {
        let (sps, mut pps) = (test_sps(), test_pps());
        pps.redundant_pic_cnt_present_flag = true;
        // first_mb(0) slice_type(0) pps_id(0) frame_num(0000) redundant_pic_cnt=1 -> ue(1)="010"
        let rbsp = bits_to_bytes("1110000010");
        let mut b = AuBuilder::default();
        let err = b.feed_slice((0, 10), false, 2, &rbsp, |_| Some((sps, pps))).unwrap_err();
        assert_eq!(err, "redundant coded slices are not supported");
    }
}
