//! Golden files for libx265 output (an open GOP with CRA pictures and HRD
//! fixed picture rate, a closed-GOP Main 10 stream), cross-checked against
//! FFmpeg's decoder output order and `hvcC` (decision 74), and failure
//! cases. Set `UPDATE_GOLDEN=1` to rewrite the golden files after an
//! intended change.

use std::path::{Path, PathBuf};

use vmkv_parser_hevc::Hevc;
use vtj::cli;
use vtj::validate::{validate, Options, Outcome};

fn media(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/media").join(name)
}

fn run(args: &[&str], input: &Path) -> (i32, String) {
    let mut a: Vec<std::ffi::OsString> = args.iter().map(std::ffi::OsString::from).collect();
    a.push(input.into());
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cli::run(&Hevc, &a, &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-hevc-{}", std::process::id()));
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

fn error(out: &str) -> &str {
    out.lines().last().unwrap()
}

/// `(offset, length)` of every NAL unit, start codes and stuffing excluded.
fn nals(d: &[u8]) -> Vec<(usize, usize)> {
    let starts: Vec<usize> =
        (0..d.len().saturating_sub(2)).filter(|&i| d[i..i + 3] == [0, 0, 1]).map(|i| i + 3).collect();
    starts
        .iter()
        .enumerate()
        .map(|(k, &s)| {
            let mut e = if k + 1 < starts.len() { starts[k + 1] - 3 } else { d.len() };
            while e > s && d[e - 1] == 0 {
                e -= 1;
            }
            (s, e - s)
        })
        .collect()
}

#[test]
fn golden_outputs() {
    for (name, args) in [("hevc_open_gop", vec![]), ("hevc_main10_closed", vec!["--frame-rate", "24000/1001"])] {
        let input = media(&format!("{name}.hevc"));
        let (code, out) = run(&args, &input);
        assert_eq!(code, 0, "{out}");
        assert_eq!(outcome(&out), Outcome::Success);
        let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../testdata/golden/hevc/{name}.vtj"));
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &out).unwrap();
        }
        assert_eq!(out, std::fs::read_to_string(&golden).unwrap(), "{name}");
        assert_eq!(run(&args, &input).1, out, "rule 8");
        assert_eq!(units(&out).len(), 36);
    }
    // HRD fixed_pic_rate_general_flag: 25 fps from the VUI, no parameter.
    let (_, out) = run(&[], &media("hevc_open_gop.hevc"));
    assert!(!out.lines().next().unwrap().contains("params"));
    let u = units(&out);
    assert!(
        u[0].starts_with(r#"{"type":"unit","pts_ns":0,"duration_ns":40000000,"flags":["random_access"]"#),
        "{}",
        u[0]
    );
    // The CRA pictures are random access points too, and do not restart POC.
    assert_eq!(u.iter().filter(|l| l.contains("random_access")).count(), 3);
    assert!(
        out.contains(r#""codec_id":"V_MPEGH/ISO/HEVC""#)
            && out.contains(r#""video":{"pixel_width":192,"pixel_height":108}"#)
    );

    let (code, out) = run(&[], &media("hevc_main10_closed.hevc"));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert!(
        error(&out).contains(r#""code":"TIMING_REQUIRED","message":"the VUI timing does not fix the picture rate (no fixed_pic_rate_general_flag); pass --frame-rate""#),
        "{out}"
    );
}

#[test]
fn parameter_sets_go_to_codec_private_only() {
    let (_, out) = run(&[], &media("hevc_open_gop.hevc"));
    let d = std::fs::read(media("hevc_open_gop.hevc")).unwrap();
    let ps: Vec<(usize, usize)> =
        nals(&d).into_iter().filter(|&(s, _)| (32..=34).contains(&((d[s] >> 1) & 0x3f))).collect();
    for (s, _) in &ps {
        let r = format!(r#"["src",0,{s},"#);
        assert!(!units(&out).iter().any(|u| u.contains(&r)), "a parameter set at {s} is in a unit");
    }
    let track = out.lines().find(|l| l.contains(r#""type":"track""#)).unwrap();
    assert_eq!(track.matches(r#"["src",0,"#).count(), 3, "one VPS, SPS and PPS: {track}");
}

#[test]
fn rejected_streams() {
    let d = std::fs::read(media("hevc_open_gop.hevc")).unwrap();
    let all = nals(&d);
    let vcl: Vec<(usize, usize)> = all.iter().copied().filter(|&(s, _)| (d[s] >> 1) & 0x3f < 32).collect();

    // Starting at the second picture: no random access picture first.
    let cut = vcl[1].0 - 4;
    let mut m = d[..vcl[0].0 - 4].to_vec();
    m.extend(&d[cut..]);
    let (_, out) = run(&[], &temp("noirap.hevc", &m));
    assert!(error(&out).contains("does not start with a random access (IRAP) picture"), "{out}");

    // A NAL unit in layer 1.
    let mut l = d.clone();
    l[vcl[0].0] |= 1;
    let (_, out) = run(&[], &temp("layer.hevc", &l));
    assert!(error(&out).contains(r#""code":"UNSUPPORTED_FEATURE""#) && error(&out).contains("in layer 32"), "{out}");

    // Dolby Vision RPU (type 62) after the first picture.
    let mut dv = d.clone();
    let at = vcl[1].0 - 4;
    dv.splice(at..at, [0, 0, 0, 1, 62 << 1, 1, 0x10]);
    let (_, out) = run(&[], &temp("dv.hevc", &dv));
    assert!(error(&out).contains("NAL unit type 62 at byte"), "{out}");

    // forbidden_zero_bit.
    let mut f = d.clone();
    f[all[0].0] |= 0x80;
    let (_, out) = run(&[], &temp("forbidden.hevc", &f));
    assert!(error(&out).contains("has forbidden_zero_bit set"), "{out}");
}
