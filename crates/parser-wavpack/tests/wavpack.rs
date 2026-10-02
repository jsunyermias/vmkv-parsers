//! Golden files for output of the reference WavPack 5.9.0 encoder (lossless
//! stereo and 5.1, a custom sample rate, a hybrid pair with its .wvc), and
//! failure cases derived from them. Set `UPDATE_GOLDEN=1` to rewrite the
//! golden files after an intended change.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use vmkv_parser_wavpack::WavPack;
use vtj::cli;
use vtj::validate::{validate, Options, Outcome};

fn media(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/media").join(name)
}

fn run(inputs: &[&Path]) -> (i32, String) {
    let argv: Vec<OsString> = inputs.iter().map(|p| p.as_os_str().to_os_string()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cli::run(&WavPack, &argv, &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-wavpack-{}", std::process::id()));
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

#[test]
fn golden_outputs() {
    let cases: [(&str, Vec<PathBuf>); 4] = [
        ("wavpack_stereo", vec![media("wavpack_stereo.wv")]),
        ("wavpack_51", vec![media("wavpack_51.wv")]),
        ("wavpack_50k", vec![media("wavpack_50k.wv")]),
        ("wavpack_hybrid", vec![media("wavpack_hybrid.wv"), media("wavpack_hybrid.wvc")]),
    ];
    for (name, inputs) in &cases {
        let refs: Vec<&Path> = inputs.iter().map(PathBuf::as_path).collect();
        let (code, out) = run(&refs);
        assert_eq!(code, 0, "{out}");
        assert_eq!(outcome(&out), Outcome::Success);
        let golden =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../testdata/golden/wavpack/{name}.vtj"));
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &out).unwrap();
        }
        assert_eq!(out, std::fs::read_to_string(&golden).unwrap(), "{name}");
        assert_eq!(run(&refs).1, out, "rule 8");
        assert!(units(&out).iter().all(|u| u.contains(r#""flags":["random_access"]"#)));
        assert!(out.contains(r#""codec_id":"A_WAVPACK4","codec_private":[["inline","EAQ="]]"#), "version 0x410: {out}");
    }

    // Stereo: one block per frame, the reduced header and the data are one
    // contiguous span of the source.
    let (_, out) = run(&[&media("wavpack_stereo.wv")]);
    assert!(units(&out)[0].contains(r#""payload":[["src",0,20,5176]]"#), "{}", units(&out)[0]);
    // 5.1: four blocks per frame (L/R, C, LFE, Ls/Rs), each with its data
    // size inline.
    let (_, out) = run(&[&media("wavpack_51.wv")]);
    assert_eq!(units(&out)[0].matches(r#"["inline","#).count(), 4, "{}", units(&out)[0]);
    assert!(out.contains(r#""audio":{"sampling_frequency":[48000,1],"channels":6,"bit_depth":24}"#), "{out}");
    let (_, out) = run(&[&media("wavpack_50k.wv")]);
    assert!(out.contains(r#""sampling_frequency":[50000,1]"#), "custom rate sub-block: {out}");
    // Hybrid with its correction file: block addition 1 and its mapping.
    let (_, out) = run(&[&media("wavpack_hybrid.wv"), &media("wavpack_hybrid.wvc")]);
    assert!(units(&out)[0].contains(r#""block_additions":[{"id":1,"data":[["src",1,28,"#), "{}", units(&out)[0]);
    assert!(out.contains(r#""block_addition_mappings":[{"type":1}]"#), "{out}");
}

#[test]
fn hybrid_without_correction_is_the_lossy_part() {
    let (code, out) = run(&[&media("wavpack_hybrid.wv")]);
    assert_eq!(code, 0, "{out}");
    assert!(!out.contains("block_addition"), "{out}");
    let (_, with) = run(&[&media("wavpack_hybrid.wv"), &media("wavpack_hybrid.wvc")]);
    let strip = |l: &str| l.split(r#","block_additions""#).next().unwrap().trim_end_matches('}').to_string();
    let a: Vec<_> = units(&out).iter().map(|l| strip(l)).collect();
    let b: Vec<_> = units(&with).iter().map(|l| strip(l)).collect();
    assert_eq!(a, b, "same units, the correction only adds to them");
}

#[test]
fn mismatched_inputs() {
    let (_, out) = run(&[&media("wavpack_stereo.wv"), &media("wavpack_hybrid.wvc")]);
    assert!(
        error(&out).contains(r#""message":"a .wvc was given but frame 0 is not hybrid""#)
            || error(&out).contains("does not match frame 0"),
        "{out}"
    );

    let (_, out) = run(&[&media("wavpack_hybrid.wv"), &media("wavpack_51.wv")]);
    assert!(error(&out).contains("correction block at byte 0 does not match frame 0"), "{out}");

    let c = std::fs::read(media("wavpack_hybrid.wvc")).unwrap();
    let mut extra = c.clone();
    extra.extend(&c[..c.len().min(1000)]);
    let (_, out) = run(&[&media("wavpack_hybrid.wv"), &temp("extra.wvc", &extra)]);
    assert!(error(&out).contains("does not match frame") || error(&out).contains("no frame uses"), "{out}");
}

#[test]
fn damaged_streams_and_tags() {
    let b = std::fs::read(media("wavpack_stereo.wv")).unwrap();
    let (code, out) = run(&[&temp("cut.wv", &b[..b.len() - 7])]);
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(error(&out).contains(r#""code":"TRUNCATED_BITSTREAM""#), "{out}");

    let mut j = b.clone();
    j[0] = b'x';
    let (_, out) = run(&[&temp("nosync.wv", &j)]);
    assert!(error(&out).contains(r#""message":"frame 0 block at byte 0: no wvpk block header""#), "{out}");

    let mut v = b.clone();
    v[8] = 0x01;
    let (_, out) = run(&[&temp("v401.wv", &v)]);
    assert!(error(&out).contains(r#""code":"UNSUPPORTED_CODEC_VARIANT""#), "{out}");

    // A trailing ID3v1 tag is skipped.
    let mut t = b.clone();
    let mut tag = b"TAG".to_vec();
    tag.resize(128, b' ');
    t.extend(tag);
    let (code, out) = run(&[&temp("tag.wv", &t)]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(units(&out).len(), 4);
}
