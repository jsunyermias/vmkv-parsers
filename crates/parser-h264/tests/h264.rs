//! Golden file for real encoder output, cross-checked against independent
//! ground truth gathered with `ffprobe`/`ffmpeg` when the fixture was
//! generated, and behavior tests for malformed/unsupported input.
//! Set `UPDATE_GOLDEN=1` to rewrite the golden file after an intended change.

use std::path::{Path, PathBuf};

use vmkv_parser_h264::H264;
use vtj::cli;
use vtj::validate::{validate, Options, Outcome};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(args: &[&str], input: &Path) -> (i32, String, String) {
    let mut a: Vec<std::ffi::OsString> = args.iter().map(std::ffi::OsString::from).collect();
    a.push(input.into());
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cli::run(&H264, &a, &mut out, &mut err);
    (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-h264-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn outcome(out: &str) -> Outcome {
    let r = validate(out.as_bytes(), &Options { codec_aware: true, max_problems: 0 });
    assert_ne!(r.outcome, Outcome::Invalid, "{:?}", r.problems);
    r.outcome
}

fn units(out: &str) -> Vec<&str> {
    out.lines().filter(|l| l.starts_with(r#"{"type":"unit""#)).collect()
}

#[test]
fn golden_output() {
    let path = root().join("testdata/media/h264_sample.h264");
    // libx264 through FFmpeg leaves fixed_frame_rate_flag at 0: the VUI
    // clock does not fix the frame rate (decision 71).
    let (code, out, err) = run(&["--frame-rate", "25"], &path);
    assert_eq!(code, 0, "{err}");
    assert_eq!(outcome(&out), Outcome::Success);
    let golden = root().join("testdata/golden/h264/h264_sample.vtj");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden, &out).unwrap();
    }
    assert_eq!(out, std::fs::read_to_string(&golden).unwrap());
    assert_eq!(run(&["--frame-rate", "25"], &path).1, out, "rule 8: same source, same output byte for byte");
    let (code, no_rate, _) = run(&[], &path);
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert!(no_rate.ends_with("\"code\":\"TIMING_REQUIRED\",\"message\":\"the VUI timing does not fix the frame rate (fixed_frame_rate_flag = 0); pass --frame-rate\"}\n"), "{no_rate}");
    assert_eq!(units(&out).len(), 20, "ffprobe's own frame count for this fixture");
}

/// The `codec_private` this parser builds, compared byte for byte against
/// `ffmpeg`'s own `avcC` box for this exact fixture — obtained once, this
/// session, by remuxing `h264_sample.h264` to MP4 with `-c:v copy` and
/// reading the `stsd/avc1/avcC` box's bytes directly (not reproduced here,
/// only its result). Independent confirmation that `codec_private`'s
/// layout and byte ranges are correct, not just internally consistent.
#[test]
fn codec_private_matches_ffmpegs_own_avcc_box() {
    let path = root().join("testdata/media/h264_sample.h264");
    let (code, out, err) = run(&["--frame-rate", "25"], &path);
    assert_eq!(code, 0, "{err}");
    let track = out.lines().find(|l| l.contains(r#""type":"track""#)).unwrap();
    let src = std::fs::read(&path).unwrap();

    // Walk the `codec_private` chain's `["inline","..."]`/`["src",0,off,len]`
    // entries in document order and concatenate what they resolve to.
    let chain_start = track.find("\"codec_private\":").unwrap() + "\"codec_private\":".len();
    let chain = &track[chain_start..];
    let mut rebuilt: Vec<u8> = Vec::new();
    let mut rest = chain;
    loop {
        let inline_at = rest.find("[\"inline\",\"");
        let src_at = rest.find("[\"src\",0,");
        let use_inline = match (inline_at, src_at) {
            (Some(i), Some(s)) => i < s,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if use_inline {
            let after = &rest[inline_at.unwrap() + "[\"inline\",\"".len()..];
            let end = after.find('"').unwrap();
            rebuilt.extend(vtj::base64::decode(&after[..end]).unwrap());
            rest = &after[end..];
        } else {
            let after = &rest[src_at.unwrap() + "[\"src\",0,".len()..];
            let end = after.find(']').unwrap();
            let mut it = after[..end].split(',');
            let off: usize = it.next().unwrap().parse().unwrap();
            let len: usize = it.next().unwrap().parse().unwrap();
            rebuilt.extend(&src[off..off + len]);
            rest = &after[end..];
        }
        if rest.starts_with("],\"video\"") {
            break;
        }
    }
    let ffmpeg_avcc = "014d400bffe10017674d400beca162760220000003002000000641e28532c001000468ce0fc8";
    let rebuilt_hex: String = rebuilt.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(rebuilt_hex, ffmpeg_avcc);
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

/// A minimal valid Annex B stream with no VUI at all (so no frame rate can
/// be detected): Baseline SPS (type-2 POC, so the slice needs no POC
/// bits), a matching PPS, and one IDR slice.
fn minimal_stream_without_vui() -> Vec<u8> {
    let sps_bits = concat!(
        "1",   // seq_parameter_set_id ue(0)
        "1",   // log2_max_frame_num_minus4 ue(0) -> 4 bits
        "011", // pic_order_cnt_type ue(2)
        "1",   // max_num_ref_frames ue(0)
        "0",   // gaps_in_frame_num_value_allowed_flag
        "1",   // pic_width_in_mbs_minus1 ue(0) -> 1 MB (16 px)
        "1",   // pic_height_in_map_units_minus1 ue(0) -> 1 MB (16 px)
        "1",   // frame_mbs_only_flag
        "1",   // direct_8x8_inference_flag
        "0",   // frame_cropping_flag
        "0",   // vui_parameters_present_flag
    );
    let mut sps = vec![0x42u8, 0x00, 0x0A]; // profile_idc Baseline, constraints, level_idc
    sps.extend(bits_to_bytes(sps_bits));

    let pps_bits = concat!(
        "1",  // pic_parameter_set_id ue(0)
        "1",  // seq_parameter_set_id ue(0)
        "0",  // entropy_coding_mode_flag
        "0",  // bottom_field_pic_order_in_frame_present_flag
        "1",  // num_slice_groups_minus1 ue(0)
        "1",  // num_ref_idx_l0_default_active_minus1 ue(0)
        "1",  // num_ref_idx_l1_default_active_minus1 ue(0)
        "0",  // weighted_pred_flag
        "00", // weighted_bipred_idc
        "1",  // pic_init_qp_minus26 se(0)
        "1",  // pic_init_qs_minus26 se(0)
        "1",  // chroma_qp_index_offset se(0)
        "0",  // deblocking_filter_control_present_flag
        "0",  // constrained_intra_pred_flag
        "0",  // redundant_pic_cnt_present_flag
    );
    let pps = bits_to_bytes(pps_bits);

    let slice_bits = concat!(
        "1",       // first_mb_in_slice ue(0)
        "0001000", // slice_type ue(7): I
        "1",       // pic_parameter_set_id ue(0)
        "0000",    // frame_num (4 bits, log2_max_frame_num = 4)
        "1",       // idr_pic_id ue(0) (this is an IDR slice)
        "00",      // no_output_of_prior_pics_flag, long_term_reference_flag
    );
    let slice = bits_to_bytes(slice_bits);

    let mut out = Vec::new();
    out.extend([0, 0, 0, 1, 0x67]);
    out.extend(&sps);
    out.extend([0, 0, 0, 1, 0x68]);
    out.extend(&pps);
    out.extend([0, 0, 0, 1, 0x65]);
    out.extend(&slice);
    out
}

#[test]
fn no_vui_and_no_frame_rate_param_is_timing_required() {
    let f = temp("no_vui.h264", &minimal_stream_without_vui());
    let (code, out, _) = run(&[], &f);
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(
        out.ends_with(
            "\"code\":\"TIMING_REQUIRED\",\"message\":\"the stream carries no timing; pass --frame-rate\"}\n"
        ),
        "{out}"
    );
}

#[test]
fn explicit_frame_rate_overrides_missing_vui() {
    let f = temp("no_vui_with_rate.h264", &minimal_stream_without_vui());
    let (code, out, err) = run(&["--frame-rate", "25"], &f);
    assert_eq!(code, 0, "{err}");
    assert_eq!(outcome(&out), Outcome::Success);
    assert_eq!(units(&out).len(), 1);
    assert!(units(&out)[0].contains(r#""flags":["random_access"]"#), "the only unit is the IDR: {}", units(&out)[0]);
}

#[test]
fn truncated_sps_is_rejected_not_misparsed() {
    let full = std::fs::read(root().join("testdata/media/h264_sample.h264")).unwrap();
    // Cut the file inside the SPS (offset 4..27), well before the PPS.
    let cut = temp("cut_sps.h264", &full[..10]);
    let (code, out, _) = run(&[], &cut);
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(out.contains(r#""code":"INVALID_BITSTREAM""#), "{out}");
}

#[test]
fn unknown_nal_unit_type_is_rejected() {
    let full = std::fs::read(root().join("testdata/media/h264_sample.h264")).unwrap();
    let mut modified = full.clone();
    // The SEI NAL's header byte (offset 38): set its type to 20 (slice
    // extension, out of this parser's scope) while keeping nal_ref_idc.
    modified[38] = (modified[38] & 0xE0) | 20;
    let f = temp("unknown_nal.h264", &modified);
    let (code, out, _) = run(&[], &f);
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(out.contains(r#""code":"UNSUPPORTED_FEATURE","message":"NAL unit type 20"#), "{out}");
}

fn error_line(out: &str) -> &str {
    out.lines().last().unwrap()
}

/// An Annex B stream from `(NAL header byte, RBSP bits)` pairs.
fn annexb(nals: &[(u8, &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (header, bits) in nals {
        out.extend([0, 0, 0, 1, *header]);
        out.extend(bits_to_bytes(bits));
    }
    out
}

/// Baseline, type-2 POC, one 16x16 macroblock, no VUI; `crop` is the
/// frame cropping part (flag, then its four offsets when set).
fn sps_bits(crop: &str) -> String {
    let mut s = String::from("01000010"); // profile_idc 66
    s += "00000000"; // constraint flags
    s += "00001010"; // level_idc 10
    s += "1"; // seq_parameter_set_id ue(0)
    s += "1"; // log2_max_frame_num_minus4 ue(0)
    s += "011"; // pic_order_cnt_type ue(2)
    s += "1"; // max_num_ref_frames ue(0)
    s += "0"; // gaps_in_frame_num_value_allowed_flag
    s += "1"; // pic_width_in_mbs_minus1 ue(0)
    s += "1"; // pic_height_in_map_units_minus1 ue(0)
    s += "1"; // frame_mbs_only_flag
    s += "1"; // direct_8x8_inference_flag
    s += crop;
    s += "0"; // vui_parameters_present_flag
    s
}

/// pps id 0, sps id 0, CAVLC, no bottom field POC, one slice group, one
/// default reference each way, no weighted prediction, QP offsets 0, no
/// deblocking control, no constrained intra, no redundant_pic_cnt.
const PPS_BITS: &str = "1100111000111000";
/// The same with bottom_field_pic_order_in_frame_present_flag set.
const PPS_BOTTOM_FIELD_BITS: &str = "1101111000111000";
/// IDR I slice: first_mb 0, slice_type ue(7), pps 0, frame_num 0 (4 bits),
/// idr_pic_id 0, no_output_of_prior_pics 0, long_term_reference 0.
const IDR_BITS: &str = "1000100010000100";

#[test]
fn high_profile_fixture_with_three_gops() {
    let path = root().join("testdata/media/h264_high_2gop.h264");
    let (code, out, err) = run(&[], &path);
    assert_eq!(code, 0, "{err}");
    assert_eq!(outcome(&out), Outcome::Success);
    let golden = root().join("testdata/golden/h264/h264_high_2gop.vtj");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden, &out).unwrap();
    }
    assert_eq!(out, std::fs::read_to_string(&golden).unwrap());
    let u = units(&out);
    assert_eq!(u.len(), 36);
    // fixed_frame_rate_flag = 1: 24000/1001 from the VUI, no parameter.
    assert!(!out.lines().next().unwrap().contains("params"));
    // Each IDR starts a new POC period: the second IDR (decode index 12)
    // is presented at frame 12, after everything before it.
    assert!(u[12].starts_with(r#"{"type":"unit","pts_ns":500500000,"#), "{}", u[12]);
    // High profile: the avcC record ends with chroma format 1 and 8-bit
    // depths (fc f8 f8) and no SPS extension, as FFmpeg's own avcC.
    assert!(out.contains(r#"["inline","/fj4AA=="]]"#), "{out}");
}

#[test]
fn a_second_gop_is_presented_after_the_first() {
    // Codex's reproduction: the fixture twice in a row.
    let one = std::fs::read(root().join("testdata/media/h264_sample.h264")).unwrap();
    let (_, single, _) = run(&["--frame-rate", "25"], &temp("one.h264", &one));
    let twice = [one.clone(), one].concat();
    let (code, out, err) = run(&["--frame-rate", "25"], &temp("twice.h264", &twice));
    assert_eq!(code, 0, "{err}");
    let u = units(&out);
    assert_eq!(u.len(), 40);
    assert!(
        u[20].starts_with(r#"{"type":"unit","pts_ns":800000000,"duration_ns":40000000,"flags":["random_access"]"#),
        "{}",
        u[20]
    );
    // The first GOP keeps exactly the times it has on its own.
    let pts = |l: &str| l.split(',').nth(1).unwrap().to_string();
    let first: Vec<_> = u[..20].iter().map(|l| pts(l)).collect();
    let alone: Vec<_> = units(&single).iter().map(|l| pts(l)).collect();
    assert_eq!(first, alone);
}

#[test]
fn impossible_cropping_fails_cleanly_in_the_binary() {
    // 16x16 picture cropped by 18 horizontal pixels (crop_right = 9 units
    // of 2): the binary must write an error line, not panic.
    // Crop flag, then left ue(0), right ue(9), top ue(0), bottom ue(0).
    let crop = sps_bits("11000101011");
    let stream = annexb(&[(0x67, &crop), (0x68, PPS_BITS), (0x65, IDR_BITS)]);
    let f = temp("crop.h264", &stream);
    let bin = env!("CARGO_BIN_EXE_vmkv-parser-h264");
    let o = std::process::Command::new(bin).args(["--frame-rate", "25"]).arg(&f).output().unwrap();
    let out = String::from_utf8(o.stdout).unwrap();
    assert_eq!(o.status.code(), Some(cli::EXIT_PARSE_ERROR), "{out}");
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(
        error_line(&out).contains("frame cropping of 18x0 pixels leaves nothing of the 16x16 coded picture"),
        "{out}"
    );
}

#[test]
fn an_sei_between_pictures_belongs_to_the_next_one() {
    let b = std::fs::read(root().join("testdata/media/h264_sample.h264")).unwrap();
    let (_, plain, _) = run(&["--frame-rate", "25"], &temp("plain.h264", &b));
    let second = units(&plain)[1];
    let src_at = second.find(r#"["src",0,"#).unwrap() + 9;
    let slice_at: usize = second[src_at..].split(',').next().unwrap().parse().unwrap();
    // Insert a copy of the fixture's SEI (offset 38, before the IDR) right
    // before the second picture's slice.
    let sei_end = b[38..].windows(3).position(|w| w == [0, 0, 1]).unwrap() + 38;
    let sei = b[38..sei_end].to_vec();
    let mut m = b[..slice_at - 4].to_vec();
    m.extend([0, 0, 0, 1]);
    m.extend(&sei);
    m.extend(&b[slice_at - 4..]);
    let (code, out, err) = run(&["--frame-rate", "25"], &temp("sei.h264", &m));
    assert_eq!(code, 0, "{err}");
    let u = units(&out);
    assert_eq!(u.len(), 20);
    let sei_at = slice_at as u64;
    assert!(u[1].contains(&format!(r#"["src",0,{sei_at},"#)), "the SEI opens picture 1: {}", u[1]);
    assert!(!u[0].contains(&format!(r#"["src",0,{sei_at},"#)), "and is not appended to picture 0: {}", u[0]);
}

#[test]
fn unmodelled_poc_features_are_rejected() {
    let idr = IDR_BITS;
    let (_, out, _) = run(
        &["--frame-rate", "25"],
        &temp("bottom.h264", &annexb(&[(0x67, &sps_bits("0")), (0x68, PPS_BOTTOM_FIELD_BITS), (0x65, idr)])),
    );
    assert!(
        error_line(&out).contains(r#""code":"UNSUPPORTED_FEATURE","message":"bottom_field_pic_order_in_frame_present_flag = 1 is not supported""#),
        "{out}"
    );

    // A P slice that is a reference picture with memory management
    // operation 5: first_mb 0, P, PPS 0, frame_num 1, no override, no list
    // modification, adaptive marking, MMCO 5.
    let mmco5 = "111000100100110";
    let (_, out, _) = run(
        &["--frame-rate", "25"],
        &temp("mmco5.h264", &annexb(&[(0x67, &sps_bits("0")), (0x68, PPS_BITS), (0x65, idr), (0x41, mmco5)])),
    );
    assert!(
        error_line(&out)
            .contains(r#""code":"UNSUPPORTED_FEATURE","message":"memory management operation 5 is not supported""#),
        "{out}"
    );
}

#[test]
fn malformed_nal_headers_and_prefixes() {
    let idr = IDR_BITS;
    let ok = annexb(&[(0x67, &sps_bits("0")), (0x68, PPS_BITS), (0x65, idr)]);
    let (code, _, err) = run(&["--frame-rate", "25"], &temp("ok.h264", &ok));
    assert_eq!(code, 0, "the synthetic stream itself is valid: {err}");

    let mut junk = b"X".to_vec();
    junk.extend(&ok);
    let (_, out, _) = run(&["--frame-rate", "25"], &temp("junk.h264", &junk));
    assert!(error_line(&out).contains(r#""message":"byte 0 before the first start code is not zero""#), "{out}");

    let forbidden = annexb(&[(0x67, &sps_bits("0")), (0x68, PPS_BITS), (0xe5, idr)]);
    let (_, out, _) = run(&["--frame-rate", "25"], &temp("forbidden.h264", &forbidden));
    assert!(error_line(&out).contains("has forbidden_zero_bit set"), "{out}");

    let unreferenced_idr = annexb(&[(0x67, &sps_bits("0")), (0x68, PPS_BITS), (0x05, idr)]);
    let (_, out, _) = run(&["--frame-rate", "25"], &temp("idr0.h264", &unreferenced_idr));
    assert!(error_line(&out).contains("NAL unit of type 5 at byte"), "{out}");
}
