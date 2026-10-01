//! Golden files for real encoder output and failure cases derived from it.
//! Set `UPDATE_GOLDEN=1` to rewrite the golden files after an intended change.

use std::path::{Path, PathBuf};

use vmkv_parser_mp2::Mp2;
use vtj::cli;
use vtj::validate::{validate, Options, Outcome};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn media(name: &str) -> Vec<u8> {
    std::fs::read(root().join("testdata/media").join(name)).unwrap()
}

fn run(input: &Path) -> (i32, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cli::run(&Mp2, &[input.as_os_str().to_os_string()], &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-mp2-{}", std::process::id()));
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
    for name in ["mp2_stereo", "mp2_mono_lsf"] {
        let input = root().join(format!("testdata/media/{name}.mp2"));
        let (code, out) = run(&input);
        assert_eq!(code, 0, "{out}");
        assert_eq!(outcome(&out), Outcome::Success);
        let golden = root().join(format!("testdata/golden/mp2/{name}.vtj"));
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &out).unwrap();
        }
        assert_eq!(out, std::fs::read_to_string(&golden).unwrap(), "{name}");
        assert_eq!(run(&input).1, out, "rule 8");
    }
    let (_, out) = run(&root().join("testdata/media/mp2_mono_lsf.mp2"));
    // 1152 / 22050 s per frame.
    assert!(units(&out)[1].starts_with(r#"{"type":"unit","pts_ns":52244898,"duration_ns":52244898,"#), "{out}");
    assert!(out.contains(r#""codec_id":"A_MPEG/L2","audio":{"sampling_frequency":[22050,1],"channels":1}"#), "{out}");
}

#[test]
fn tags_truncation_and_junk() {
    let b = media("mp2_stereo.mp2");
    let mut t = vec![b'I', b'D', b'3', 4, 0, 0, 0, 0, 0, 5, 1, 2, 3, 4, 5];
    t.extend(&b);
    let (code, out) = run(&temp("id3.mp2", &t));
    assert_eq!(code, 0, "{out}");
    assert!(units(&out)[0].contains(r#""payload":[["src",0,15,576]]"#), "{out}");

    let (code, out) = run(&temp("cut.mp2", &b[..b.len() - 5]));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(
        error(&out)
            .contains(&format!(r#""code":"TRUNCATED_BITSTREAM","message":"frame 41 cut at byte {}""#, b.len() - 5)),
        "{out}"
    );

    let mut j = b.clone();
    j.insert(576, 0);
    let (_, out) = run(&temp("junk.mp2", &j));
    assert!(error(&out).contains(r#""code":"INVALID_BITSTREAM","message":"no MPEG audio sync at byte 576""#), "{out}");
}

#[test]
fn other_layers_and_changes() {
    let (_, out) = run(&root().join("testdata/media/mp3_plain.mp3"));
    assert!(
        error(&out).contains(r#""code":"UNSUPPORTED_CODEC_VARIANT""#)
            && error(&out).contains("is Layer III; use vmkv-parser-mp3"),
        "{out}"
    );

    let mut b = media("mp2_stereo.mp2");
    b.extend(media("mp2_mono_lsf.mp2"));
    let (_, out) = run(&temp("mix.mp2", &b));
    assert!(
        error(&out).contains(r#""code":"INCONSISTENT_TRACK_PARAMETERS","message":"frame 42 at byte 24192 changes version/sample rate/channels from (Mpeg1, 48000, 2) to (Mpeg2, 22050, 1)""#),
        "{out}"
    );
}
