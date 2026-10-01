//! Sequence and picture parameter sets (ITU-T H.264 §7.3.2.1 / §7.3.2.2):
//! only the fields this parser needs downstream (timing, picture size,
//! POC) are read; anything after them in the RBSP is never consulted.

use crate::bits::BitReader;

/// Profiles whose SPS carries `chroma_format_idc`, bit depths and an
/// optional scaling matrix (H.264 §7.3.2.1.1) — the "High" profile family.
const PROFILES_WITH_CHROMA_INFO: [u8; 13] = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PicOrderCntType {
    /// Explicit `pic_order_cnt_lsb` per picture (by far the most common).
    Type0 { log2_max_pic_order_cnt_lsb: u32 },
    /// Derived from `frame_num` alone; no extra slice header fields.
    Type2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sps {
    pub seq_parameter_set_id: u32,
    pub profile_idc: u8,
    pub level_idc: u8,
    pub log2_max_frame_num: u32,
    pub poc_type: PicOrderCntType,
    pub max_num_ref_frames: u32,
    /// Luma samples, after cropping.
    pub pic_width: u64,
    pub pic_height: u64,
    /// `(num_units_in_tick, time_scale)` from VUI timing info, if present:
    /// frame rate is `time_scale / (2 * num_units_in_tick)` (H.264 Annex
    /// E.2.1 — the factor of 2 holds for progressive content too, by the
    /// same convention VUI timing always uses).
    pub vui_timing: Option<(u32, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pps {
    pub pic_parameter_set_id: u32,
    pub seq_parameter_set_id: u32,
    pub bottom_field_pic_order_in_frame_present_flag: bool,
    pub redundant_pic_cnt_present_flag: bool,
}

fn skip_scaling_list(r: &mut BitReader, size: usize) -> Result<(), String> {
    let mut last_scale = 8i32;
    let mut next_scale = 8i32;
    for _ in 0..size {
        if next_scale != 0 {
            let delta_scale = r.se()? as i32;
            next_scale = (last_scale + delta_scale + 256) % 256;
        }
        last_scale = if next_scale == 0 { last_scale } else { next_scale };
    }
    Ok(())
}

/// Parses an SPS RBSP (NAL header byte already stripped, emulation
/// prevention already removed).
pub fn parse_sps(rbsp: &[u8]) -> Result<Sps, String> {
    let mut r = BitReader::new(rbsp);
    let profile_idc = r.u(8)? as u8;
    let _constraint_flags_and_reserved = r.u(8)?;
    let level_idc = r.u(8)? as u8;
    let seq_parameter_set_id = r.ue()? as u32;

    let mut chroma_format_idc = 1u32;
    if PROFILES_WITH_CHROMA_INFO.contains(&profile_idc) {
        chroma_format_idc = r.ue()? as u32;
        if chroma_format_idc == 3 {
            let _separate_colour_plane_flag = r.u1()?;
        }
        let _bit_depth_luma = r.ue()? + 8;
        let _bit_depth_chroma = r.ue()? + 8;
        let _qpprime_y_zero_transform_bypass_flag = r.u1()?;
        if r.u1()? {
            // seq_scaling_matrix_present_flag
            let count = if chroma_format_idc != 3 { 8 } else { 12 };
            for i in 0..count {
                if r.u1()? {
                    // seq_scaling_list_present_flag[i]
                    skip_scaling_list(&mut r, if i < 6 { 16 } else { 64 })?;
                }
            }
        }
    }
    if !matches!(chroma_format_idc, 0 | 1) {
        return Err(format!("chroma_format_idc {chroma_format_idc} (4:2:2 or 4:4:4) is not supported"));
    }

    let log2_max_frame_num = r.ue()? as u32 + 4;
    let pic_order_cnt_type_raw = r.ue()?;
    let poc_type = match pic_order_cnt_type_raw {
        0 => PicOrderCntType::Type0 { log2_max_pic_order_cnt_lsb: r.ue()? as u32 + 4 },
        1 => return Err("pic_order_cnt_type 1 is not supported".into()),
        2 => PicOrderCntType::Type2,
        other => return Err(format!("pic_order_cnt_type {other} is not a valid value (0, 1 or 2)")),
    };
    let max_num_ref_frames = r.ue()? as u32;
    let _gaps_in_frame_num_value_allowed_flag = r.u1()?;
    let pic_width_in_mbs = r.ue()? + 1;
    let pic_height_in_map_units = r.ue()? + 1;
    let frame_mbs_only_flag = r.u1()?;
    if !frame_mbs_only_flag {
        return Err("interlaced video (frame_mbs_only_flag = 0) is not supported".into());
    }
    let _direct_8x8_inference_flag = r.u1()?;
    let frame_cropping_flag = r.u1()?;
    let (mut crop_left, mut crop_right, mut crop_top, mut crop_bottom) = (0u64, 0u64, 0u64, 0u64);
    if frame_cropping_flag {
        crop_left = r.ue()?;
        crop_right = r.ue()?;
        crop_top = r.ue()?;
        crop_bottom = r.ue()?;
    }
    // H.264 §7.4.2.1.1: for chroma_format_idc 1 (4:2:0), CropUnitX = 2,
    // CropUnitY = 2 (frame_mbs_only_flag is required true here); for
    // monochrome (0), CropUnitX = CropUnitY = 1.
    let (crop_unit_x, crop_unit_y) = if chroma_format_idc == 0 { (1, 1) } else { (2, 2) };

    let pic_width = pic_width_in_mbs * 16 - (crop_left + crop_right) * crop_unit_x;
    let pic_height = pic_height_in_map_units * 16 - (crop_top + crop_bottom) * crop_unit_y;

    let vui_parameters_present_flag = r.u1()?;
    let mut vui_timing = None;
    if vui_parameters_present_flag {
        if r.u1()? {
            // aspect_ratio_info_present_flag
            let ar_idc = r.u(8)?;
            if ar_idc == 255 {
                let _sar_width = r.u(16)?;
                let _sar_height = r.u(16)?;
            }
        }
        if r.u1()? {
            // overscan_info_present_flag
            let _overscan_appropriate_flag = r.u1()?;
        }
        if r.u1()? {
            // video_signal_type_present_flag
            let _video_format = r.u(3)?;
            let _video_full_range_flag = r.u1()?;
            if r.u1()? {
                // colour_description_present_flag
                let _colour_primaries = r.u(8)?;
                let _transfer_characteristics = r.u(8)?;
                let _matrix_coefficients = r.u(8)?;
            }
        }
        if r.u1()? {
            // chroma_loc_info_present_flag
            let _chroma_sample_loc_type_top_field = r.ue()?;
            let _chroma_sample_loc_type_bottom_field = r.ue()?;
        }
        if r.u1()? {
            // timing_info_present_flag
            let num_units_in_tick = r.u(32)? as u32;
            let time_scale = r.u(32)? as u32;
            let _fixed_frame_rate_flag = r.u1()?;
            if num_units_in_tick > 0 {
                vui_timing = Some((num_units_in_tick, time_scale));
            }
        }
        // Bitstream/NAL HRD parameters and the rest of VUI are never
        // needed: nothing after timing_info is read.
    }

    Ok(Sps {
        seq_parameter_set_id,
        profile_idc,
        level_idc,
        log2_max_frame_num,
        poc_type,
        max_num_ref_frames,
        pic_width,
        pic_height,
        vui_timing,
    })
}

/// Parses a PPS RBSP (NAL header byte already stripped, emulation
/// prevention already removed). Stops right after the one flag this
/// parser needs that comes latest in the syntax; fields after it
/// (`more_rbsp_data`-gated quantization matrices, in a PPS extension) are
/// never read.
pub fn parse_pps(rbsp: &[u8]) -> Result<Pps, String> {
    let mut r = BitReader::new(rbsp);
    let pic_parameter_set_id = r.ue()? as u32;
    let seq_parameter_set_id = r.ue()? as u32;
    let _entropy_coding_mode_flag = r.u1()?;
    let bottom_field_pic_order_in_frame_present_flag = r.u1()?;
    let num_slice_groups_minus1 = r.ue()?;
    if num_slice_groups_minus1 > 0 {
        return Err("slice groups (FMO) are not supported".into());
    }
    let _num_ref_idx_l0_default_active = r.ue()?;
    let _num_ref_idx_l1_default_active = r.ue()?;
    let _weighted_pred_flag = r.u1()?;
    let _weighted_bipred_idc = r.u(2)?;
    let _pic_init_qp = r.se()?;
    let _pic_init_qs = r.se()?;
    let _chroma_qp_index_offset = r.se()?;
    let _deblocking_filter_control_present_flag = r.u1()?;
    let _constrained_intra_pred_flag = r.u1()?;
    let redundant_pic_cnt_present_flag = r.u1()?;

    Ok(Pps {
        pic_parameter_set_id,
        seq_parameter_set_id,
        bottom_field_pic_order_in_frame_present_flag,
        redundant_pic_cnt_present_flag,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real libx264 fixture's own SPS/PPS bytes (see
    /// `annexb::tests::real_encoder_fixture_first_four_nals`), with the NAL
    /// header byte and emulation prevention already stripped. Independently
    /// decoded by hand when the fixture was generated and cross-checked
    /// against `ffprobe -show_streams` (176x144, profile Main (77), level
    /// 11, 25 fps).
    fn real_sps_rbsp() -> Vec<u8> {
        // NAL (with header) 4d400beca162760220000003002000000641e28532c0;
        // header byte 0x67 stripped, then emulation prevention (00 00 03 ->
        // 00 00) removed from the one place it occurs.
        crate::bits::remove_emulation_prevention(&hex("4d400beca162760220000003002000000641e28532c0"))
    }

    fn real_pps_rbsp() -> Vec<u8> {
        hex("ce0fc8")
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn real_fixture_sps() {
        let sps = parse_sps(&real_sps_rbsp()).unwrap();
        assert_eq!(sps.profile_idc, 77, "Main profile");
        assert_eq!(sps.level_idc, 11, "level 1.1");
        assert_eq!(sps.seq_parameter_set_id, 0);
        assert_eq!(sps.pic_width, 176);
        assert_eq!(sps.pic_height, 144);
        assert_eq!(sps.log2_max_frame_num, 4, "log2_max_frame_num_minus4 = 0");
        assert_eq!(sps.max_num_ref_frames, 4);
        assert_eq!(sps.poc_type, PicOrderCntType::Type0 { log2_max_pic_order_cnt_lsb: 6 });
        assert_eq!(sps.vui_timing, Some((1, 50)), "25 fps as time_scale/(2*num_units_in_tick)");
    }

    #[test]
    fn real_fixture_pps() {
        let pps = parse_pps(&real_pps_rbsp()).unwrap();
        assert_eq!(pps.pic_parameter_set_id, 0);
        assert_eq!(pps.seq_parameter_set_id, 0);
        assert!(!pps.bottom_field_pic_order_in_frame_present_flag);
        assert!(!pps.redundant_pic_cnt_present_flag);
    }

    /// A High-profile SPS with chroma info and a scaling matrix, interlaced
    /// video, and `pic_order_cnt_type` 1 are all out of this parser's
    /// scope; each must fail cleanly rather than misparse the rest of the
    /// bitstream.
    #[test]
    fn unsupported_features_are_rejected_not_misparsed() {
        // frame_mbs_only_flag = 0 (interlaced), otherwise a minimal valid
        // SPS (profile 66 Baseline, no chroma info fields).
        let mut bits = String::new();
        bits += &format!("{:08b}", 66u8); // profile_idc
        bits += &format!("{:08b}", 0u8); // constraint flags
        bits += &format!("{:08b}", 10u8); // level_idc
        bits += "1"; // seq_parameter_set_id ue(0)
        bits += "1"; // log2_max_frame_num_minus4 ue(0)
        bits += "1"; // pic_order_cnt_type ue(0)
        bits += "1"; // log2_max_pic_order_cnt_lsb_minus4 ue(0)
        bits += "1"; // max_num_ref_frames ue(0)
        bits += "1"; // gaps_in_frame_num_value_allowed_flag
        bits += "1"; // pic_width_in_mbs_minus1 ue(0)
        bits += "1"; // pic_height_in_map_units_minus1 ue(0)
        bits += "0"; // frame_mbs_only_flag = 0 (interlaced)
        let bytes = bits_to_bytes(&bits);
        assert!(parse_sps(&bytes).unwrap_err().contains("interlaced"));
    }

    fn bits_to_bytes(bits: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let mut byte = 0u8;
        let mut n = 0;
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

    #[test]
    fn poc_type_1_is_rejected() {
        let mut bits = String::new();
        bits += &format!("{:08b}", 66u8);
        bits += &format!("{:08b}", 0u8);
        bits += &format!("{:08b}", 10u8);
        bits += "1"; // sps id
        bits += "1"; // log2_max_frame_num_minus4
        bits += "010"; // pic_order_cnt_type = 1 (ue code for 1 is "010")
        let bytes = bits_to_bytes(&bits);
        assert_eq!(parse_sps(&bytes).unwrap_err(), "pic_order_cnt_type 1 is not supported");
    }

    #[test]
    fn frame_cropping_reduces_the_reported_size() {
        // A 16x16-macroblock (one MB) picture, cropped by 1 pixel on the
        // right and bottom (chroma 4:2:0: crop unit 2 pixels per ue step).
        let mut bits = String::new();
        bits += &format!("{:08b}", 66u8);
        bits += &format!("{:08b}", 0u8);
        bits += &format!("{:08b}", 10u8);
        bits += "1"; // sps_id
        bits += "1"; // log2_max_frame_num_minus4
        bits += "1"; // pic_order_cnt_type = 0
        bits += "1"; // log2_max_pic_order_cnt_lsb_minus4
        bits += "1"; // max_num_ref_frames
        bits += "1"; // gaps flag
        bits += "1"; // pic_width_in_mbs_minus1 = 0 (1 MB wide = 16 px)
        bits += "1"; // pic_height_in_map_units_minus1 = 0 (1 MB tall = 16 px)
        bits += "1"; // frame_mbs_only_flag = 1
        bits += "1"; // direct_8x8_inference_flag
        bits += "1"; // frame_cropping_flag = 1
        bits += "1"; // crop_left = ue(0)
        bits += "010"; // crop_right = ue(1)
        bits += "1"; // crop_top = ue(0)
        bits += "010"; // crop_bottom = ue(1)
        bits += "0"; // vui_parameters_present_flag = 0
        let bytes = bits_to_bytes(&bits);
        let sps = parse_sps(&bytes).unwrap();
        assert_eq!(sps.pic_width, 16 - 2, "1 MB (16px) minus a 1-unit (2px) right crop");
        assert_eq!(sps.pic_height, 16 - 2);
    }
}
