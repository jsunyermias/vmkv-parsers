//! Golden files for a synthetic `.idx` + `.sub` pair (two interleaved
//! streams, a multi-pack subpicture, a delay line, a subpicture without a
//! stop command; decodable by FFmpeg), and failure cases. Real VobSub was
//! verified outside the repository (decision 72). Set `UPDATE_GOLDEN=1` to
//! rewrite the golden files after an intended change.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use vmkv_parser_vobsub::VobSub;
use vtj::cli;
use vtj::validate::{validate, Options, Outcome};

fn media(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/media").join(name)
}

fn run(args: &[&str], inputs: &[&Path]) -> (i32, String) {
    let mut argv: Vec<OsString> = args.iter().map(OsString::from).collect();
    argv.extend(inputs.iter().map(|p| p.as_os_str().to_os_string()));
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cli::run(&VobSub, &argv, &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-vobsub-{}", std::process::id()));
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
    let (idx, sub) = (media("vobsub_sample.idx"), media("vobsub_sample.sub"));
    for stream in ["0", "1"] {
        let (code, out) = run(&["--stream-index", stream], &[&idx, &sub]);
        assert_eq!(code, 0, "{out}");
        assert_eq!(outcome(&out), Outcome::Success);
        let golden = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../testdata/golden/vobsub/vobsub_sample_{stream}.vtj"));
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &out).unwrap();
        }
        assert_eq!(out, std::fs::read_to_string(&golden).unwrap(), "stream {stream}");
        assert_eq!(run(&["--stream-index", stream], &[&idx, &sub]).1, out, "rule 8");
        assert!(
            out.lines().next().unwrap().contains(r#""sources":[{"id":0,"#) && out.contains(r#"{"id":1,"#),
            "two sources"
        );
        assert!(units(&out).iter().all(|u| u.contains(r#""flags":["random_access"]"#)));
    }
    let (_, out) = run(&["--stream-index", "0"], &[&idx, &sub]);
    let u = units(&out);
    // The setting lines of the .idx, one span, in source 0.
    assert!(out.contains(r#""codec_id":"S_VOBSUB","codec_private":[["src",0,65,"#), "{out}");
    // The stop command at date 90: 90 * 1024 / 90000 s.
    assert!(u[0].starts_with(r#"{"type":"unit","pts_ns":1000000000,"duration_ns":1024000000,"#), "{}", u[0]);
    // A subpicture over two packs is two src chunks of source 1.
    assert!(u[1].contains(r#""payload":[["src",1,4125,2019],["src",1,6168,1471]]"#), "{}", u[1]);
    // delay +0.5 s applies to what follows in its stream; no stop command.
    assert!(u[2].starts_with(r#"{"type":"unit","pts_ns":6000000000,"duration_ns":-1,"#), "{}", u[2]);
    // ... and not to the next stream (VSFilter's rule).
    let (_, out) = run(&["--stream-index", "1"], &[&idx, &sub]);
    assert!(units(&out)[0].starts_with(r#"{"type":"unit","pts_ns":1200000000,"#), "{out}");
}

#[test]
fn stream_choice() {
    let (idx, sub) = (media("vobsub_sample.idx"), media("vobsub_sample.sub"));
    let (code, out) = run(&[], &[&idx, &sub]);
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(
        error(&out).contains(r#""code":"UNSUPPORTED_FEATURE","message":"the .idx declares 2 streams (0, 1); choose one with --stream-index""#),
        "{out}"
    );
    let (_, out) = run(&["--stream-index", "5"], &[&idx, &sub]);
    assert!(error(&out).contains(r#""message":"the .idx has no stream with index 5""#), "{out}");

    // One stream left: no choice needed.
    let text = std::fs::read_to_string(&idx).unwrap();
    let only = &text[..text.find("# es").unwrap()];
    let (code, out) = run(&[], &[&temp("one.idx", only.as_bytes()), &sub]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(units(&out).len(), 3);
}

#[test]
fn broken_inputs() {
    let (idx, sub) = (media("vobsub_sample.idx"), media("vobsub_sample.sub"));
    let text = std::fs::read_to_string(&idx).unwrap();

    let v6 = text.replacen("v7", "v6", 1);
    let (_, out) = run(&["--stream-index", "0"], &[&temp("v6.idx", v6.as_bytes()), &sub]);
    assert!(error(&out).contains(r#""code":"UNSUPPORTED_CODEC_VARIANT""#), "{out}");

    let moved = text.replacen("filepos: 000001000", "filepos: 000001004", 1);
    let (_, out) = run(&["--stream-index", "0"], &[&temp("moved.idx", moved.as_bytes()), &sub]);
    assert!(
        error(&out)
            .contains(r#""message":"subpicture 1 at filepos 0x1004: no program stream start code at byte 4100""#),
        "{out}"
    );

    let s = std::fs::read(&sub).unwrap();
    let (code, out) = run(&["--stream-index", "0"], &[&idx, &temp("cut.sub", &s[..5000])]);
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(units(&out).len(), 1, "the subpicture before the cut is described");
    assert!(error(&out).contains("subpicture 1 at filepos 0x1000"), "{out}");

    // Both files are needed: a single input is a usage error.
    let (code, out) = run(&[], &[&idx]);
    assert_eq!(code, cli::EXIT_USAGE);
    assert!(out.is_empty());
}
