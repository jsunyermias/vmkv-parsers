//! Golden files for the Matroska mapping example and for real converter
//! output, plus synthetic files for the rest.
//! Set `UPDATE_GOLDEN=1` to rewrite the golden files after an intended change.

use std::path::{Path, PathBuf};

use vmkv_parser_webvtt::WebVtt;
use vtj::cli;
use vtj::validate::{validate, Options, Outcome};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(input: &Path) -> (i32, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cli::run(&WebVtt, &[input.as_os_str().to_os_string()], &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-webvtt-{}", std::process::id()));
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
    for name in ["webvtt_features", "webvtt_sample"] {
        let input = root().join(format!("testdata/media/{name}.vtt"));
        let (code, out) = run(&input);
        assert_eq!(code, 0, "{out}");
        assert_eq!(outcome(&out), Outcome::Success);
        let golden = root().join(format!("testdata/golden/webvtt/{name}.vtj"));
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &out).unwrap();
        }
        assert_eq!(out, std::fs::read_to_string(&golden).unwrap(), "{name}");
        assert_eq!(run(&input).1, out, "rule 8");
        assert!(units(&out).iter().all(|u| u.contains(r#""flags":["random_access","duration_required"]"#)));
    }
    let (_, out) = run(&root().join("testdata/media/webvtt_features.vtt"));
    let u = units(&out);
    // The identifier, with an empty settings line before it.
    assert!(
        u[0].ends_with(r#""block_additions":[{"id":1,"data":[["inline","Cg=="],["src",0,115,5],["inline","Cg=="]]}]}"#),
        "{}",
        u[0]
    );
    // A NOTE between cues travels with the next cue.
    assert!(u[1].contains(r#"[["inline","Cgo="],["src",0,189,51],["inline","Cg=="]]"#), "{}", u[1]);
    // The inner timestamp 00:03:15.000 becomes 5 s after the cue's start.
    assert!(u[3].contains(r#"["inline","MDA6MDA6MDUuMDAw"]"#) && !u[3].contains("block_additions"), "{}", u[3]);
    assert!(out.contains(r#""codec_private":[["src",0,0,113]]"#), "global blocks up to the first cue: {out}");
    let (_, out) = run(&root().join("testdata/media/webvtt_sample.vtt"));
    assert!(units(&out)[2].contains(r#""duration_ns":0,"#), "a cue may last zero ns");
}

#[test]
fn bom_and_crlf() {
    let v = b"\xef\xbb\xbfWEBVTT\r\n\r\n1\r\n00:01.000 --> 00:02.000 align:start\r\nA\r\nB\r\n\r\n";
    let (code, out) = run(&temp("crlf.vtt", v));
    assert_eq!(code, 0, "{out}");
    let u = units(&out);
    assert!(u[0].contains(r#""payload":[["src",0,53,4]]"#), "line endings kept, last one dropped: {}", u[0]);
    assert!(
        u[0].contains(r#""data":[["src",0,40,11],["inline","Cg=="],["src",0,13,1],["inline","Cg=="]]"#),
        "{}",
        u[0]
    );
    assert!(out.contains(r#""codec_private":[["src",0,3,6]]"#), "no BOM: {out}");
}

#[test]
fn cue_without_text_and_trailing_note() {
    let v = b"WEBVTT\n\n00:01.000 --> 00:02.000\n\nNOTE trailing\n";
    let (code, out) = run(&temp("empty.vtt", v));
    assert_eq!(code, 0, "{out}");
    assert!(units(&out)[0].contains(r#""payload":[]"#), "{out}");
}

#[test]
fn rejected() {
    let cases: [(&str, &[u8], &str); 7] = [
        (
            "nosig.vtt",
            b"WEBVTX\n\n00:01.000 --> 00:02.000\nA\n",
            r#""code":"MISSING_INITIALIZATION_DATA","message":"no WEBVTT signature""#,
        ),
        ("latin1.vtt", b"WEBVTT\n\n00:01.000 --> 00:02.000\n\xe9\n", r#""message":"not UTF-8 at byte 32""#),
        (
            "style.vtt",
            b"WEBVTT\n\n00:01.000 --> 00:02.000\nA\n\nSTYLE\n::cue {}\n",
            r#""message":"STYLE block at byte 35 after the first cue""#,
        ),
        ("back.vtt", b"WEBVTT\n\n00:02.000 --> 00:01.000\nA\n", r#""message":"cue 0 at byte 8 ends before it starts""#),
        (
            "bad.vtt",
            b"WEBVTT\n\n00:01,000 --> 00:02.000\nA\n",
            r#""message":"cue 0 timing line at byte 8: invalid start timestamp""#,
        ),
        (
            "inner.vtt",
            b"WEBVTT\n\n00:01.000 --> 00:02.000\nA<00:03.000>B\n",
            r#""message":"cue 0 at byte 8: inner timestamp 00:00:03.000 is outside the cue""#,
        ),
        ("junk.vtt", b"WEBVTT\n\nhello there\n", r#""message":"block at byte 8 is not a cue, NOTE, STYLE or REGION""#),
    ];
    for (name, bytes, msg) in cases {
        let (code, out) = run(&temp(name, bytes));
        assert_eq!(code, cli::EXIT_PARSE_ERROR, "{name}: {out}");
        assert_eq!(outcome(&out), Outcome::Failure);
        assert!(error(&out).contains(msg), "{name}: {out}");
    }
}
