//! Golden files for real encoder output and failure cases derived from it.
//! Set `UPDATE_GOLDEN=1` to rewrite the golden files after an intended change.

use std::path::{Path, PathBuf};

use vmkv_parser_dts::Dts;
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
    let code = cli::run(&Dts, &[input.as_os_str().to_os_string()], &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-dts-{}", std::process::id()));
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
    for name in ["dts_51", "dts_stereo_44k"] {
        let input = root().join(format!("testdata/media/{name}.dts"));
        let (code, out) = run(&input);
        assert_eq!(code, 0, "{out}");
        assert_eq!(outcome(&out), Outcome::Success);
        let golden = root().join(format!("testdata/golden/dts/{name}.vtj"));
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &out).unwrap();
        }
        assert_eq!(out, std::fs::read_to_string(&golden).unwrap(), "{name}");
        assert_eq!(run(&input).1, out, "rule 8");
    }
    let (_, out) = run(&root().join("testdata/media/dts_stereo_44k.dts"));
    // 512 / 44100 s, exact rather than accumulated (rule 2).
    assert!(units(&out)[1].starts_with(r#"{"type":"unit","pts_ns":11609977,"duration_ns":11609978,"#), "{out}");
    assert!(out.contains(r#""codec_id":"A_DTS","audio":{"sampling_frequency":[44100,1],"channels":2}"#), "{out}");
}

#[test]
fn truncated_and_junk() {
    let b = media("dts_51.dts");
    let (code, out) = run(&temp("cut.dts", &b[..b.len() - 5]));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert_eq!(units(&out).len(), 93);
    assert!(
        error(&out)
            .contains(&format!(r#""code":"TRUNCATED_BITSTREAM","message":"frame 93 cut at byte {}""#, b.len() - 5)),
        "{out}"
    );

    let mut j = b.clone();
    j.insert(1884, 0);
    let (_, out) = run(&temp("junk.dts", &j));
    assert!(error(&out).contains(r#""code":"INVALID_BITSTREAM","message":"no DTS sync at byte 1884""#), "{out}");

    let (_, out) = run(&temp("empty.dts", b""));
    assert!(error(&out).contains(r#""message":"no DTS frames""#), "{out}");
}

#[test]
fn unsupported_forms() {
    // The same stream with each 16-bit word byte-swapped: little-endian.
    let mut le = media("dts_51.dts");
    for w in le.chunks_exact_mut(2) {
        w.swap(0, 1);
    }
    let (_, out) = run(&temp("le.dts", &le));
    assert!(
        error(&out).contains(
            r#""code":"UNSUPPORTED_CODEC_VARIANT","message":"frame 0 at byte 0 uses the 16-bit little-endian packing""#
        ),
        "{out}"
    );

    // A DTS-HD substream after the first core frame.
    let mut hd = media("dts_51.dts");
    hd.splice(1884..1884, [0x64, 0x58, 0x20, 0x25, 0, 0, 0, 0, 0, 0, 0]);
    let (_, out) = run(&temp("hd.dts", &hd));
    assert_eq!(units(&out).len(), 1);
    assert!(
        error(&out).contains(
            r#""code":"UNSUPPORTED_FEATURE","message":"frame 1 at byte 1884 is a DTS-HD extension substream""#
        ),
        "{out}"
    );
}

#[test]
fn layout_change_is_inconsistent() {
    let mut b = media("dts_51.dts");
    b.extend(media("dts_stereo_44k.dts"));
    let (code, out) = run(&temp("mix.dts", &b));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(units(&out).len(), 94);
    assert!(
        error(&out).contains(r#""code":"INCONSISTENT_TRACK_PARAMETERS","message":"frame 94 at byte 177096 changes sample rate/channel mode/LFE from (48000, 9, true) to (44100, 2, false)""#),
        "{out}"
    );
}
