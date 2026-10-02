//! Video, sequence and picture parameter sets (ITU-T H.265 §7.3.2.1 to
//! §7.3.2.3): the fields the access unit assembly, picture order count,
//! frame rate and `hvcC` need, every one checked against its normative
//! range before use. Everything after the last needed field is never read.

use crate::bits::BitReader;

/// `ue(v)` checked against an inclusive maximum.
pub fn ue_max(r: &mut BitReader, max: u64, what: &str) -> Result<u32, String> {
    let v = r.ue()?;
    if v > max {
        return Err(format!("{what} {v} is out of range (at most {max})"));
    }
    Ok(v as u32)
}

fn skip(r: &mut BitReader, bits: u32) -> Result<(), String> {
    let mut left = bits;
    while left > 0 {
        let n = left.min(32);
        r.u(n)?;
        left -= n;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vps {
    pub vps_id: u32,
    pub max_sub_layers: u32,
}

pub fn parse_vps(rbsp: &[u8]) -> Result<Vps, String> {
    let mut r = BitReader::new(rbsp);
    let vps_id = r.u(4)? as u32;
    r.u(2)?; // vps_base_layer_internal_flag, vps_base_layer_available_flag
    r.u(6)?; // vps_max_layers_minus1
    let max_sub_layers = r.u(3)? as u32 + 1;
    if max_sub_layers > 7 {
        return Err("vps_max_sub_layers_minus1 7 is reserved".into());
    }
    Ok(Vps { vps_id, max_sub_layers })
}

/// `profile_tier_level(1, max_sub_layers_minus1)`: the 12 general bytes are
/// returned as they are (they go into `hvcC` verbatim); the sub-layer part
/// is skipped.
fn profile_tier_level(r: &mut BitReader, max_sub_layers_minus1: u32) -> Result<[u8; 12], String> {
    let mut general = [0u8; 12];
    for b in general.iter_mut() {
        *b = r.u(8)? as u8;
    }
    let mut present = Vec::new();
    for _ in 0..max_sub_layers_minus1 {
        present.push((r.u1()?, r.u1()?));
    }
    if max_sub_layers_minus1 > 0 {
        for _ in max_sub_layers_minus1..8 {
            r.u(2)?; // reserved_zero_2bits
        }
    }
    for (profile, level) in present {
        if profile {
            skip(r, 88)?;
        }
        if level {
            r.u(8)?;
        }
    }
    Ok(general)
}

fn scaling_list_data(r: &mut BitReader) -> Result<(), String> {
    for size_id in 0..4 {
        let mut matrix_id = 0;
        while matrix_id < 6 {
            if !r.u1()? {
                // scaling_list_pred_mode_flag = 0
                ue_max(
                    r,
                    if size_id == 3 { matrix_id as u64 / 3 } else { matrix_id as u64 },
                    "scaling_list_pred_matrix_id_delta",
                )?;
            } else {
                let coefs = 64.min(1 << (4 + (size_id << 1)));
                if size_id > 1 {
                    let dc = r.se()?;
                    if !(-7..=247).contains(&dc) {
                        return Err(format!("scaling_list_dc_coef_minus8 {dc} is out of range"));
                    }
                }
                for _ in 0..coefs {
                    let d = r.se()?;
                    if !(-128..=127).contains(&d) {
                        return Err(format!("scaling_list_delta_coef {d} is out of range"));
                    }
                }
            }
            matrix_id += if size_id == 3 { 3 } else { 1 };
        }
    }
    Ok(())
}

/// Skips `st_ref_pic_set(idx)` and returns its `NumDeltaPocs`; `counts`
/// holds those of the sets before it.
fn st_ref_pic_set(r: &mut BitReader, idx: usize, counts: &[u32]) -> Result<u32, String> {
    if idx != 0 && r.u1()? {
        // inter_ref_pic_set_prediction_flag: in the SPS the reference set is
        // always the previous one.
        r.u1()?; // delta_rps_sign
        ue_max(r, (1 << 15) - 1, "abs_delta_rps_minus1")?;
        let mut n = 0;
        for _ in 0..=counts[idx - 1] {
            let used = r.u1()?;
            let use_delta = if used { true } else { r.u1()? };
            if used || use_delta {
                n += 1;
            }
        }
        if n > 16 {
            return Err(format!("short-term reference picture set {idx} has {n} pictures"));
        }
        return Ok(n);
    }
    let negative = ue_max(r, 16, "num_negative_pics")?;
    let positive = ue_max(r, 16 - negative as u64, "num_positive_pics")?;
    for _ in 0..negative + positive {
        ue_max(r, (1 << 15) - 1, "delta_poc_minus1")?;
        r.u1()?; // used_by_curr_pic
    }
    Ok(negative + positive)
}

/// What VUI timing says about the picture rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VuiTiming {
    pub num_units_in_tick: u32,
    pub time_scale: u32,
    /// `elemental_duration_in_tc_minus1 + 1` of the highest sub-layer when
    /// HRD parameters set `fixed_pic_rate_general_flag` for it: only then is
    /// every picture as long (decision 74).
    pub fixed_ticks: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sps {
    pub vps_id: u32,
    pub sps_id: u32,
    pub max_sub_layers: u32,
    pub temporal_id_nesting: bool,
    pub profile_tier_level: [u8; 12],
    pub chroma_format_idc: u32,
    pub separate_colour_plane: bool,
    pub bit_depth_luma_minus8: u32,
    pub bit_depth_chroma_minus8: u32,
    pub log2_max_poc_lsb: u32,
    /// Luma samples after the conformance window.
    pub width: u64,
    pub height: u64,
    pub vui_timing: Option<VuiTiming>,
    pub min_spatial_segmentation_idc: u32,
}

fn hrd_parameters(r: &mut BitReader, max_sub_layers_minus1: u32) -> Result<Option<u32>, String> {
    let nal = r.u1()?;
    let vcl = r.u1()?;
    let mut sub_pic = false;
    if nal || vcl {
        sub_pic = r.u1()?;
        if sub_pic {
            skip(r, 8 + 5 + 1 + 5)?;
        }
        skip(r, 4 + 4)?; // bit_rate_scale, cpb_size_scale
        if sub_pic {
            skip(r, 4)?;
        }
        skip(r, 5 + 5 + 5)?;
    }
    let mut fixed = None;
    for _ in 0..=max_sub_layers_minus1 {
        let general = r.u1()?;
        let within_cvs = if general { true } else { r.u1()? };
        let mut low_delay = false;
        let elemental = if within_cvs {
            Some(ue_max(r, 2047, "elemental_duration_in_tc_minus1")? + 1)
        } else {
            low_delay = r.u1()?;
            None
        };
        let cpb_cnt = if low_delay { 0 } else { ue_max(r, 31, "cpb_cnt_minus1")? };
        for present in [nal, vcl] {
            if present {
                for _ in 0..=cpb_cnt {
                    r.ue()?; // bit_rate_value_minus1
                    r.ue()?; // cpb_size_value_minus1
                    if sub_pic {
                        r.ue()?;
                        r.ue()?;
                    }
                    r.u1()?; // cbr_flag
                }
            }
        }
        // The highest sub-layer's values are the ones that apply to the
        // whole stream; each iteration overwrites the previous one.
        fixed = if general { elemental } else { None };
    }
    Ok(fixed)
}

pub fn parse_sps(rbsp: &[u8]) -> Result<Sps, String> {
    let mut r = BitReader::new(rbsp);
    let vps_id = r.u(4)? as u32;
    let max_sub_layers_minus1 = r.u(3)? as u32;
    if max_sub_layers_minus1 > 6 {
        return Err("sps_max_sub_layers_minus1 7 is reserved".into());
    }
    let temporal_id_nesting = r.u1()?;
    let profile_tier_level = profile_tier_level(&mut r, max_sub_layers_minus1)?;
    let sps_id = ue_max(&mut r, 15, "sps_seq_parameter_set_id")?;
    let chroma_format_idc = ue_max(&mut r, 3, "chroma_format_idc")?;
    let separate_colour_plane = chroma_format_idc == 3 && r.u1()?;
    // Bounded far beyond any level's limit, so that the size arithmetic
    // below cannot overflow.
    let coded_width = ue_max(&mut r, 1 << 16, "pic_width_in_luma_samples")? as u64;
    let coded_height = ue_max(&mut r, 1 << 16, "pic_height_in_luma_samples")? as u64;
    if coded_width == 0 || coded_height == 0 {
        return Err("picture size of 0".into());
    }
    let (mut left, mut right, mut top, mut bottom) = (0u64, 0u64, 0u64, 0u64);
    if r.u1()? {
        // conformance_window_flag; `ue` is below 2^33, so the sums fit.
        left = r.ue()?;
        right = r.ue()?;
        top = r.ue()?;
        bottom = r.ue()?;
    }
    let (sub_w, sub_h) = match (chroma_format_idc, separate_colour_plane) {
        (1, false) => (2, 2),
        (2, false) => (2, 1),
        _ => (1, 1),
    };
    let (crop_x, crop_y) = ((left + right) * sub_w, (top + bottom) * sub_h);
    if crop_x >= coded_width || crop_y >= coded_height {
        return Err(format!(
            "conformance window of {crop_x}x{crop_y} samples leaves nothing of the {coded_width}x{coded_height} coded picture"
        ));
    }
    let bit_depth_luma_minus8 = ue_max(&mut r, 8, "bit_depth_luma_minus8")?;
    let bit_depth_chroma_minus8 = ue_max(&mut r, 8, "bit_depth_chroma_minus8")?;
    let log2_max_poc_lsb = ue_max(&mut r, 12, "log2_max_pic_order_cnt_lsb_minus4")? + 4;
    let ordering_all = r.u1()?;
    for _ in (if ordering_all { 0 } else { max_sub_layers_minus1 })..=max_sub_layers_minus1 {
        r.ue()?; // sps_max_dec_pic_buffering_minus1
        r.ue()?; // sps_max_num_reorder_pics
        r.ue()?; // sps_max_latency_increase_plus1
    }
    for what in [
        "log2_min_luma_coding_block_size_minus3",
        "log2_diff_max_min_luma_coding_block_size",
        "log2_min_luma_transform_block_size_minus2",
        "log2_diff_max_min_luma_transform_block_size",
        "max_transform_hierarchy_depth_inter",
        "max_transform_hierarchy_depth_intra",
    ] {
        ue_max(&mut r, 32, what)?;
    }
    if r.u1()? && r.u1()? {
        // scaling_list_enabled_flag, sps_scaling_list_data_present_flag
        scaling_list_data(&mut r)?;
    }
    r.u1()?; // amp_enabled_flag
    r.u1()?; // sample_adaptive_offset_enabled_flag
    if r.u1()? {
        // pcm_enabled_flag
        skip(&mut r, 4 + 4)?;
        r.ue()?;
        r.ue()?;
        r.u1()?;
    }
    let num_short_term = ue_max(&mut r, 64, "num_short_term_ref_pic_sets")? as usize;
    let mut counts = Vec::with_capacity(num_short_term);
    for i in 0..num_short_term {
        counts.push(st_ref_pic_set(&mut r, i, &counts)?);
    }
    if r.u1()? {
        // long_term_ref_pics_present_flag
        let n = ue_max(&mut r, 32, "num_long_term_ref_pics_sps")?;
        for _ in 0..n {
            r.u(log2_max_poc_lsb)?;
            r.u1()?;
        }
    }
    r.u1()?; // sps_temporal_mvp_enabled_flag
    r.u1()?; // strong_intra_smoothing_enabled_flag
    let mut vui_timing = None;
    let mut min_spatial_segmentation_idc = 0;
    if r.u1()? {
        // vui_parameters_present_flag
        if r.u1()? {
            // aspect_ratio_info_present_flag
            if r.u(8)? == 255 {
                skip(&mut r, 32)?;
            }
        }
        if r.u1()? {
            r.u1()?; // overscan_appropriate_flag
        }
        if r.u1()? {
            // video_signal_type_present_flag
            skip(&mut r, 3 + 1)?;
            if r.u1()? {
                skip(&mut r, 24)?;
            }
        }
        if r.u1()? {
            r.ue()?;
            r.ue()?;
        }
        r.u1()?; // neutral_chroma_indication_flag
        if r.u1()? {
            return Err("field-coded video (field_seq_flag = 1) is not supported".into());
        }
        r.u1()?; // frame_field_info_present_flag
        if r.u1()? {
            // default_display_window_flag
            for _ in 0..4 {
                r.ue()?;
            }
        }
        if r.u1()? {
            // vui_timing_info_present_flag
            let num_units_in_tick = r.u(32)? as u32;
            let time_scale = r.u(32)? as u32;
            if r.u1()? {
                // vui_poc_proportional_to_timing_flag
                r.ue()?;
            }
            let fixed_ticks = if r.u1()? { hrd_parameters(&mut r, max_sub_layers_minus1)? } else { None };
            if num_units_in_tick > 0 && time_scale > 0 {
                vui_timing = Some(VuiTiming { num_units_in_tick, time_scale, fixed_ticks });
            }
        }
        if r.u1()? {
            // bitstream_restriction_flag
            skip(&mut r, 3)?;
            min_spatial_segmentation_idc = ue_max(&mut r, 4095, "min_spatial_segmentation_idc")?;
            for _ in 0..4 {
                r.ue()?;
            }
        }
    }
    Ok(Sps {
        vps_id,
        sps_id,
        max_sub_layers: max_sub_layers_minus1 + 1,
        temporal_id_nesting,
        profile_tier_level,
        chroma_format_idc,
        separate_colour_plane,
        bit_depth_luma_minus8,
        bit_depth_chroma_minus8,
        log2_max_poc_lsb,
        width: coded_width - crop_x,
        height: coded_height - crop_y,
        vui_timing,
        min_spatial_segmentation_idc,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pps {
    pub pps_id: u32,
    pub sps_id: u32,
    pub dependent_slice_segments_enabled: bool,
    pub output_flag_present: bool,
    pub num_extra_slice_header_bits: u32,
    pub tiles_enabled: bool,
    pub entropy_coding_sync_enabled: bool,
}

pub fn parse_pps(rbsp: &[u8]) -> Result<Pps, String> {
    let mut r = BitReader::new(rbsp);
    let pps_id = ue_max(&mut r, 63, "pps_pic_parameter_set_id")?;
    let sps_id = ue_max(&mut r, 15, "pps_seq_parameter_set_id")?;
    let dependent_slice_segments_enabled = r.u1()?;
    let output_flag_present = r.u1()?;
    let num_extra_slice_header_bits = r.u(3)? as u32;
    r.u1()?; // sign_data_hiding_enabled_flag
    r.u1()?; // cabac_init_present_flag
    ue_max(&mut r, 14, "num_ref_idx_l0_default_active_minus1")?;
    ue_max(&mut r, 14, "num_ref_idx_l1_default_active_minus1")?;
    r.se()?; // init_qp_minus26
    r.u1()?; // constrained_intra_pred_flag
    r.u1()?; // transform_skip_enabled_flag
    if r.u1()? {
        r.ue()?; // diff_cu_qp_delta_depth
    }
    r.se()?; // pps_cb_qp_offset
    r.se()?; // pps_cr_qp_offset
    r.u1()?; // pps_slice_chroma_qp_offsets_present_flag
    r.u1()?; // weighted_pred_flag
    r.u1()?; // weighted_bipred_flag
    r.u1()?; // transquant_bypass_enabled_flag
    let tiles_enabled = r.u1()?;
    let entropy_coding_sync_enabled = r.u1()?;
    Ok(Pps {
        pps_id,
        sps_id,
        dependent_slice_segments_enabled,
        output_flag_present,
        num_extra_slice_header_bits,
        tiles_enabled,
        entropy_coding_sync_enabled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bits::remove_emulation_prevention;

    fn fixture_nal(index: usize) -> Vec<u8> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/media/hevc_open_gop.hevc");
        let d = std::fs::read(path).unwrap();
        let starts: Vec<usize> = (0..d.len() - 2).filter(|&i| d[i..i + 3] == [0, 0, 1]).map(|i| i + 3).collect();
        let (s, e) = (starts[index], starts[index + 1] - 3);
        let e = (s..e).rev().find(|&i| d[i] != 0).unwrap() + 1;
        remove_emulation_prevention(&d[s + 2..e])
    }

    #[test]
    fn fixture_parameter_sets() {
        let vps = parse_vps(&fixture_nal(0)).unwrap();
        assert_eq!(vps, Vps { vps_id: 0, max_sub_layers: 1 });
        let sps = parse_sps(&fixture_nal(1)).unwrap();
        assert_eq!((sps.width, sps.height), (192, 108), "conformance window crops 112 to 108");
        assert_eq!((sps.chroma_format_idc, sps.bit_depth_luma_minus8, sps.log2_max_poc_lsb), (1, 0, 8));
        assert_eq!(sps.vui_timing, Some(VuiTiming { num_units_in_tick: 1, time_scale: 25, fixed_ticks: Some(1) }));
        let pps = parse_pps(&fixture_nal(2)).unwrap();
        assert_eq!((pps.pps_id, pps.sps_id, pps.output_flag_present), (0, 0, false));
    }

    #[test]
    fn impossible_conformance_window() {
        // Main profile PTL, 16x16, conformance window cropping 9 chroma
        // units (18 luma samples) on the right.
        let mut bits = String::from("0000"); // vps id
        bits += "000"; // max_sub_layers_minus1
        bits += "1"; // temporal_id_nesting
        bits += &"0".repeat(96); // profile_tier_level general part
        bits += "1"; // sps id ue(0)
        bits += "010"; // chroma_format_idc ue(1)
        bits += "000010001"; // pic_width ue(16)
        bits += "000010001"; // pic_height ue(16)
        bits += "1"; // conformance_window_flag
        bits += "1"; // left ue(0)
        bits += "0001010"; // right ue(9)
        bits += "11"; // top, bottom ue(0)
        let mut bytes = vec![0u8; bits.len().div_ceil(8) + 2];
        for (i, c) in bits.chars().enumerate() {
            if c == '1' {
                bytes[i / 8] |= 0x80 >> (i % 8);
            }
        }
        assert!(parse_sps(&bytes)
            .unwrap_err()
            .contains("conformance window of 18x0 samples leaves nothing of the 16x16 coded picture"));
    }
}
