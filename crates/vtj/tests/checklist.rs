//! One negative case per item of the specification's validation checklist.

use vtj::validate::{validate, Options, Outcome};

const H: &str = r#"{"type":"header","format":"vmkv-parser-output","version":1,"parser":{"name":"p","version":"1"},"sources":[{"id":0,"size":100}]}"#;
const U: &str = r#"{"type":"unit","pts_ns":0,"duration_ns":1000,"flags":["random_access"],"payload":[["src",0,0,10]]}"#;
const T: &str = r#"{"type":"track","track_type":"audio","codec_id":"A_MPEG/L3","audio":{"sampling_frequency":[44100,1],"channels":2}}"#;
const E1: &str = r#"{"type":"end","unit_count":1}"#;

fn file(lines: &[&str]) -> Vec<u8> {
    let mut s = lines.join("\n");
    s.push('\n');
    s.into_bytes()
}

fn problems(bytes: &[u8], codec_aware: bool) -> Vec<String> {
    let r = validate(bytes, &Options { codec_aware, max_problems: 0 });
    if r.problems.is_empty() {
        assert_ne!(r.outcome, Outcome::Invalid);
    } else {
        assert_eq!(r.outcome, Outcome::Invalid);
    }
    r.problems.iter().map(ToString::to_string).collect()
}

#[track_caller]
fn invalid(lines: &[&str], needle: &str) {
    invalid_bytes(&file(lines), needle, false);
}

#[track_caller]
fn invalid_bytes(bytes: &[u8], needle: &str, codec_aware: bool) {
    let p = problems(bytes, codec_aware);
    assert!(p.iter().any(|m| m.contains(needle)), "expected a problem containing {needle:?}, got {p:#?}");
}

fn with(base: &str, from: &str, to: &str) -> String {
    assert!(base.contains(from), "{from} not in {base}");
    base.replacen(from, to, 1)
}

#[test]
fn baseline_is_valid() {
    assert!(problems(&file(&[H, U, T, E1]), true).is_empty());
    assert!(problems(&file(&[H, T, r#"{"type":"end","unit_count":0}"#]), true).is_empty());
}

#[test]
fn json_and_known_type() {
    invalid(&[H, "{not json", T, E1], "invalid JSON");
    invalid(&[H, r#"{"type":"frame"}"#, T, E1], "unknown line type");
    invalid(&[H, r#"["unit"]"#, T, E1], "expected object");
    invalid(&[H, "", U, T, E1], "empty line");
}

#[test]
fn line_order() {
    invalid(&[U, H, T, E1], "header must be the first line");
    invalid(&[H, T, U, E1], "unit lines must come before track");
    invalid(&[H, U, T], "missing end line");
    invalid(&[H, U, E1], "end must directly follow track");
    invalid(&[H, U, T, T, E1], "only one track line");
    invalid(&[H, U, T, E1, U], "no lines are allowed after end");
    invalid(&[H, U, T, r#"{"type":"error","code":"INVALID_BITSTREAM","message":"x"}"#], "error cannot follow track");
    invalid(&[U, T, E1], "missing header");
    let err = r#"{"type":"error","code":"INVALID_BITSTREAM","message":"x"}"#;
    assert!(problems(&file(&[H, U, err]), false).is_empty());
    let inline_unit = with(U, r#"["src",0,0,10]"#, r#"["inline","AA=="]"#);
    assert!(problems(&file(&[&inline_unit, err]), false).is_empty());
    invalid(&[U, err], "unknown source id 0");
    assert!(problems(&file(&[err]), false).is_empty());
    invalid(&[H, U, err, U], "no lines are allowed after end or error");
}

#[test]
fn canonical_serialization() {
    invalid(&[&with(H, r#""version":1,"#, r#""version": 1,"#), U, T, E1], "not in canonical serialization");
    invalid(&[H, &with(U, r#""pts_ns":0,"duration_ns":1000"#, r#""duration_ns":1000,"pts_ns":0"#), T, E1], "canonical");
    invalid(&[H, U, &with(T, "A_MPEG/L3", r"A_MPEG\/L3"), E1], "canonical");
    invalid(&[H, U, &with(T, "A_MPEG", "\\u0041_MPEG"), E1], "canonical");
    invalid(&[H, U, &with(T, "A_MPEG", "A\\u005fMPEG"), E1], "canonical");
    invalid(&[H, U, &with(T, r#""channels":2}"#, r#""channels":2},"requires_lacing":false"#), E1], "canonical");
    let crlf = file(&[H, U, T, E1])
        .iter()
        .flat_map(|&b| if b == b'\n' { vec![b'\r', b'\n'] } else { vec![b] })
        .collect::<Vec<_>>();
    invalid_bytes(&crlf, "CR is not allowed", false);
    let mut no_lf = file(&[H, U, T, E1]);
    no_lf.pop();
    invalid_bytes(&no_lf, "last line must end with LF", false);
    let mut bom = vec![0xef, 0xbb, 0xbf];
    bom.extend(file(&[H, U, T, E1]));
    invalid_bytes(&bom, "byte order mark", false);
    let mut bad_utf8 = file(&[H, U, T]);
    bad_utf8.extend_from_slice(b"{\"type\":\"end\",\"unit_count\":1,\"x\":\"\xff\"}\n");
    invalid_bytes(&bad_utf8, "invalid UTF-8", false);
}

#[test]
fn no_null_and_no_unknown_fields() {
    invalid(&[H, &with(U, r#""payload""#, r#""codec_state":null,"payload""#), T, E1], "null is not allowed");
    invalid(&[H, &with(U, r#""payload""#, r#""extra":1,"payload""#), T, E1], "unknown field");
    invalid(&[H, U, &with(T, r#""channels":2"#, r#""channels":2,"bit_depth":null"#), E1], "null is not allowed");
    invalid(
        &[&with(H, r#","sources""#, r#","sources""#).replace(r#""size":100"#, r#""size":100,"path":null"#), U, T, E1],
        "null",
    );
    invalid(&[H, &with(U, r#","flags""#, r#","x":1,"flags""#), T, E1], "unknown field");
}

#[test]
fn unit_count() {
    invalid(&[H, U, U, T, E1], "unit_count is 1 but the file has 2");
}

#[test]
fn integer_range() {
    invalid(&[H, &with(U, r#""pts_ns":0"#, r#""pts_ns":9007199254740992"#), T, E1], "outside");
    invalid(&[H, &with(U, r#""pts_ns":0"#, r#""pts_ns":-9007199254740992"#), T, E1], "outside");
    invalid(&[H, &with(U, r#""pts_ns":0"#, r#""pts_ns":1.0"#), T, E1], "without fraction");
    invalid(&[H, &with(U, r#""pts_ns":0"#, r#""pts_ns":1e3"#), T, E1], "without fraction");
    let max = with(U, r#""pts_ns":0"#, r#""pts_ns":-9007199254740991"#);
    assert!(problems(&file(&[H, &max, T, E1]), false).is_empty());
}

#[test]
fn source_ids_and_references() {
    let dup = with(H, r#"{"id":0,"size":100}"#, r#"{"id":0,"size":100},{"id":0,"size":5}"#);
    invalid(&[&dup, U, T, E1], "duplicate id");
    let unordered = with(H, r#"{"id":0,"size":100}"#, r#"{"id":1,"size":100},{"id":0,"size":5}"#);
    invalid(&[&unordered, U, T, E1], "increasing order");
    invalid(
        &[&with(H, r#"[{"id":0,"size":100}]"#, "[]"), T, r#"{"type":"end","unit_count":0}"#],
        "at least one source",
    );
    invalid(&[H, &with(U, r#"["src",0,0,10]"#, r#"["src",7,0,10]"#), T, E1], "unknown source id 7");
    invalid(
        &[H, U, &with(T, r#""codec_id":"A_MPEG/L3""#, r#""codec_id":"A_MPEG/L3","codec_private":[["src",3,0,1]]"#), E1],
        "unknown source id 3",
    );
    let sha = with(H, r#""size":100"#, r#""size":100,"sha256":"ABCDEF""#);
    invalid(&[&sha, U, T, E1], "64 lowercase hexadecimal");
}

#[test]
fn src_bounds() {
    invalid(&[H, &with(U, r#"["src",0,0,10]"#, r#"["src",0,95,6]"#), T, E1], "exceeds size 100");
    let ok = with(U, r#"["src",0,0,10]"#, r#"["src",0,90,10]"#);
    assert!(problems(&file(&[H, &ok, T, E1]), false).is_empty());
}

#[test]
fn inline_base64_and_chunk_forms() {
    invalid(&[H, &with(U, r#"["src",0,0,10]"#, r#"["inline","EhA"]"#), T, E1], "base64");
    invalid(&[H, &with(U, r#"["src",0,0,10]"#, r#"["inline","EhB="]"#), T, E1], "base64");
    invalid(&[H, &with(U, r#"["src",0,0,10]"#, r#"["xform","zlib"]"#), T, E1], "reserved");
    invalid(&[H, &with(U, r#"["src",0,0,10]"#, r#"["src",0,0]"#), T, E1], "wrong number of elements");
    invalid(&[H, &with(U, r#"["src",0,0,10]"#, r#"["src",0,-1,10]"#), T, E1], "must be ≥ 0");
    let empty = with(U, r#"[["src",0,0,10]]"#, "[]");
    assert!(problems(&file(&[H, &empty, T, E1]), false).is_empty());
}

#[test]
fn durations() {
    invalid(&[H, &with(U, r#""duration_ns":1000"#, r#""duration_ns":-2"#), T, E1], "must be ≥ -1");
    let unknown = with(U, r#""duration_ns":1000"#, r#""duration_ns":-1"#);
    assert!(problems(&file(&[H, &unknown, T, E1]), false).is_empty());
    let req = with(&unknown, r#"["random_access"]"#, r#"["random_access","duration_required"]"#);
    invalid(&[H, &req, T, E1], "duration_required requires");
}

#[test]
fn flags() {
    invalid(&[H, &with(U, r#"["random_access"]"#, r#"["randon_access"]"#), T, E1], "unknown flag \"randon_access\"");
    invalid(&[H, &with(U, r#"["random_access"]"#, r#"["random_access","random_access"]"#), T, E1], "repeated flag");
    invalid(&[H, &with(U, r#"["random_access"]"#, r#"["invisible","random_access"]"#), T, E1], "canonical");
}

#[test]
fn block_additions() {
    let ba = |ids: &str| {
        with(U, r#""payload":[["src",0,0,10]]"#, &format!(r#""payload":[["src",0,0,10]],"block_additions":[{ids}]"#))
    };
    invalid(&[H, &ba(r#"{"id":0,"data":[]}"#), T, E1], "must be ≥ 1");
    invalid(&[H, &ba(r#"{"id":1,"data":[]},{"id":1,"data":[]}"#), T, E1], "duplicate id 1");
    invalid(&[H, &ba(r#"{"id":4,"data":[]}"#), T, E1], "no mapping with id_value 4");
    let mapped = with(T, r#""channels":2}"#, r#""channels":2},"block_addition_mappings":[{"id_value":4,"type":5}]"#);
    assert!(problems(&file(&[H, &ba(r#"{"id":1,"data":[]},{"id":4,"data":[]}"#), &mapped, E1]), false).is_empty());
    let bad_map = with(T, r#""channels":2}"#, r#""channels":2},"block_addition_mappings":[{"id_value":1,"type":0}]"#);
    let p = problems(&file(&[H, U, &bad_map, E1]), false);
    assert!(p.iter().any(|m| m.contains("id_value: must be ≥ 2")), "{p:?}");
    assert!(p.iter().any(|m| m.contains("type: must not be 0")), "{p:?}");
    invalid(
        &[H, &with(U, r#""payload":[["src",0,0,10]]"#, r#""payload":[["src",0,0,10]],"block_additions":[]"#), T, E1],
        "canonical",
    );
}

#[test]
fn codec_id_and_track_type() {
    invalid(&[H, U, &with(T, r#""codec_id":"A_MPEG/L3""#, r#""codec_id":"""#), E1], "codec_id: must not be empty");
    invalid(&[H, U, &with(T, r#""track_type":"audio""#, r#""track_type":"sound""#), E1], "unknown value \"sound\"");
    invalid(&[H, U, &with(T, r#","audio":{"sampling_frequency":[44100,1],"channels":2}"#, ""), E1], "audio: required");
    let vid =
        r#"{"type":"track","track_type":"video","codec_id":"V_VP9","audio":{"sampling_frequency":[1,1],"channels":1}}"#;
    let p = problems(&file(&[H, U, vid, E1]), false);
    assert!(p.iter().any(|m| m.contains("video: required")), "{p:?}");
    assert!(p.iter().any(|m| m.contains("audio: only allowed")), "{p:?}");
}

#[test]
fn audio_and_video_values() {
    invalid(&[H, U, &with(T, r#""channels":2"#, r#""channels":0"#), E1], "audio.channels: must be > 0");
    invalid(&[H, U, &with(T, "[44100,1]", "[44100,0]"), E1], "rational terms must be > 0");
    invalid(&[H, U, &with(T, "[44100,1]", "[-44100,1]"), E1], "rational terms must be > 0");
    let v = |video: &str| format!(r#"{{"type":"track","track_type":"video","codec_id":"V_VP9","video":{video}}}"#);
    invalid(&[H, U, &v(r#"{"pixel_width":0,"pixel_height":1080}"#), E1], "pixel_width: must be > 0");
    invalid(&[H, U, &v(r#"{"pixel_width":1920,"pixel_height":0}"#), E1], "pixel_height: must be > 0");
    invalid(
        &[H, U, &v(r#"{"pixel_width":1920,"pixel_height":1080,"nominal_frame_rate":[0,1]}"#), E1],
        "rational terms",
    );
    invalid(&[H, U, &v(r#"{"pixel_width":1920,"pixel_height":1080,"interlace":"maybe"}"#), E1], "unknown value");
}

#[test]
fn projection_private() {
    let v = |p: &str| {
        format!(
            r#"{{"type":"track","track_type":"video","codec_id":"V_VP9","video":{{"pixel_width":1,"pixel_height":1,"projection":{p}}}}}"#
        )
    };
    invalid(&[H, U, &v(r#"{"type":"rectangular","private":[]}"#), E1], "must be absent with type rectangular");
    invalid(&[H, U, &v(r#"{"type":"equirectangular"}"#), E1], "required with type equirectangular");
    assert!(problems(&file(&[H, U, &v(r#"{"type":"rectangular","yaw":90.5}"#), E1]), false).is_empty());
    assert!(
        problems(&file(&[H, U, &v(r#"{"type":"cubemap","private":[["inline","AAAAAA=="]]}"#), E1]), false).is_empty()
    );
    invalid(&[H, U, &v(r#"{"type":"rectangular","yaw":90.50}"#), E1], "canonical");
}

#[test]
fn colour_mastering_reals() {
    let t = r#"{"type":"track","track_type":"video","codec_id":"V_VP9","video":{"pixel_width":1,"pixel_height":1,"colour":{"matrix_coefficients":9,"range":1,"transfer_characteristics":16,"primaries":9,"max_cll":1000,"max_fall":400,"mastering":{"primary_r_chromaticity_x":0.708,"primary_r_chromaticity_y":0.292,"luminance_max":1000,"luminance_min":0.0001}}}}"#;
    assert!(problems(&file(&[H, U, t, E1]), false).is_empty());
    invalid(&[H, U, &t.replace("1000,\"luminance_min", "1000.0,\"luminance_min"), E1], "canonical");
    invalid(&[H, U, &t.replace("0.0001", "1e-4"), E1], "canonical");
}

#[test]
fn header_fields_and_params() {
    invalid(&[&with(H, "vmkv-parser-output", "other"), U, T, E1], "format: expected");
    invalid(&[&with(H, r#""version":1"#, r#""version":2"#), U, T, E1], "unsupported version 2");
    let params = with(H, r#"}]}"#, r#"}],"params":{"frame_rate":[24000,1001],"mode":"x","n":3}}"#);
    assert!(problems(&file(&[&params, U, T, E1]), false).is_empty());
    let unsorted = with(H, r#"}]}"#, r#"}],"params":{"n":3,"frame_rate":[24000,1001]}}"#);
    invalid(&[&unsorted, U, T, E1], "canonical");
    invalid(&[&with(H, r#"}]}"#, r#"}],"params":{}}"#), U, T, E1], "canonical");
    invalid(&[&with(H, r#"}]}"#, r#"}],"params":{"frame_rate":[0,1]}}"#), U, T, E1], "rational terms");
    invalid(&[&with(H, r#"}]}"#, r#"}],"params":{"x":true}}"#), U, T, E1], "parameter values");
}

#[test]
fn error_codes() {
    invalid(&[H, r#"{"type":"error","code":"OOPS","message":"x"}"#], "unknown value \"OOPS\"");
    invalid(&[H, r#"{"type":"error","code":"INVALID_BITSTREAM"}"#], "missing required field");
}

#[test]
fn codec_aware_level() {
    let aac = r#"{"type":"track","track_type":"audio","codec_id":"A_AAC","audio":{"sampling_frequency":[44100,1],"channels":2}}"#;
    assert!(problems(&file(&[H, U, aac, E1]), false).is_empty(), "structural level ignores mappings");
    invalid_bytes(&file(&[H, U, aac, E1]), "codec_private: required", true);
    let opus = r#"{"type":"track","track_type":"audio","codec_id":"A_OPUS","codec_private":[["src",0,28,19]],"audio":{"sampling_frequency":[48000,1],"channels":2}}"#;
    let p = problems(&file(&[H, U, opus, E1]), true);
    assert!(p.iter().any(|m| m.contains("codec_delay_ns: required")), "{p:?}");
    assert!(p.iter().any(|m| m.contains("seek_preroll_ns: required")), "{p:?}");
    let raw = r#"{"type":"track","track_type":"video","codec_id":"V_UNCOMPRESSED","video":{"pixel_width":1,"pixel_height":1}}"#;
    invalid_bytes(&file(&[H, U, raw, E1]), "uncompressed_fourcc: required", true);
    let mismatch = r#"{"type":"track","track_type":"subtitle","codec_id":"A_MPEG/L3"}"#;
    invalid_bytes(&file(&[H, U, mismatch, E1]), "implies audio", true);
}
