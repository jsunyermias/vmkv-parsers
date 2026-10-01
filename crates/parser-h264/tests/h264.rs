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
    let (code, out, err) = run(&[], &path);
    assert_eq!(code, 0, "{err}");
    assert_eq!(outcome(&out), Outcome::Success);
    let golden = root().join("testdata/golden/h264/h264_sample.vtj");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden, &out).unwrap();
    }
    assert_eq!(out, std::fs::read_to_string(&golden).unwrap());
    assert_eq!(run(&[], &path).1, out, "rule 8: same source, same output byte for byte");
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
    let (code, out, err) = run(&[], &path);
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
        "1",    // first_mb_in_slice ue(0)
        "1",    // slice_type ue(0)
        "1",    // pic_parameter_set_id ue(0)
        "0000", // frame_num (4 bits, log2_max_frame_num = 4)
        "1",    // idr_pic_id ue(0) (this is an IDR slice)
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
