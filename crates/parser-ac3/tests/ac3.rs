//! Golden files for real encoder output and failure cases derived from it.
//! Set `UPDATE_GOLDEN=1` to rewrite the golden files after an intended change.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use vmkv_parser_ac3::{crc16, Ac3};
use vtj::cli;
use vtj::validate::{validate, Options, Outcome};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn media(name: &str) -> Vec<u8> {
    std::fs::read(root().join("testdata/media").join(name)).unwrap()
}

fn run_args(args: &[&str], input: &Path) -> (i32, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut argv: Vec<OsString> = args.iter().map(OsString::from).collect();
    argv.push(input.as_os_str().to_os_string());
    let code = cli::run(&Ac3, &argv, &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

fn run(input: &Path) -> (i32, String) {
    run_args(&[], input)
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-ac3-{}", std::process::id()));
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
    for (name, ext) in [("ac3_stereo", "ac3"), ("ac3_51_44k", "ac3"), ("eac3_51", "eac3")] {
        let input = root().join(format!("testdata/media/{name}.{ext}"));
        let (code, out) = run(&input);
        assert_eq!(code, 0, "{out}");
        assert_eq!(outcome(&out), Outcome::Success);
        let golden = root().join(format!("testdata/golden/ac3/{name}.vtj"));
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &out).unwrap();
        }
        assert_eq!(out, std::fs::read_to_string(&golden).unwrap(), "{name}");
        assert_eq!(run(&input).1, out, "rule 8");
        assert!(!out.contains("codec_private"), "{name}: neither mapping has CodecPrivate");
    }
    let (_, out) = run(&root().join("testdata/media/ac3_51_44k.ac3"));
    assert!(out.contains(r#""codec_id":"A_AC3","audio":{"sampling_frequency":[44100,1],"channels":6}"#), "{out}");
    // 1536 / 44100 s, exact rather than accumulated (rule 2).
    assert!(units(&out)[1].starts_with(r#"{"type":"unit","pts_ns":34829932,"duration_ns":34829932,"#), "{out}");
    let (_, out) = run(&root().join("testdata/media/eac3_51.eac3"));
    assert!(out.contains(r#""codec_id":"A_EAC3","audio":{"sampling_frequency":[48000,1],"channels":6}"#), "{out}");
}

#[test]
fn truncated_and_junk() {
    let b = media("ac3_stereo.ac3");
    let (code, out) = run(&temp("cut.ac3", &b[..b.len() - 5]));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert_eq!(units(&out).len(), 31);
    assert!(error(&out).contains(r#""code":"TRUNCATED_BITSTREAM","message":"frame 31 cut at byte 24571""#), "{out}");

    let mut j = b.clone();
    j.insert(768, 0);
    let (_, out) = run(&temp("junk.ac3", &j));
    assert!(error(&out).contains(r#""code":"INVALID_BITSTREAM","message":"no AC-3 sync at byte 768""#), "{out}");

    let (_, out) = run(&temp("empty.ac3", b""));
    assert!(error(&out).contains(r#""message":"no AC-3 frames""#), "{out}");
}

#[test]
fn crc_policy() {
    let mut b = media("ac3_stereo.ac3");
    b[768 * 2 + 100] ^= 0x01;
    let p = temp("crc.ac3", &b);
    let (code, out) = run(&p);
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert!(
        error(&out).contains(r#""code":"INVALID_BITSTREAM","message":"frame 2 at byte 1536: CRC mismatch""#),
        "{out}"
    );

    let (code, out) = run_args(&["--crc", "ignore"], &p);
    assert_eq!(code, 0, "{out}");
    assert_eq!(outcome(&out), Outcome::Success);
    assert!(out.lines().next().unwrap().ends_with(r#""params":{"crc":"ignore"}}"#), "the policy is recorded: {out}");
    assert_eq!(units(&out).len(), 32);
}

/// Rewrites byte `at` of every E-AC-3 frame header with `f` and fixes the CRC.
fn edit_eac3(b: &mut [u8], at: usize, f: impl Fn(u8) -> u8) {
    let mut pos = 0;
    while pos < b.len() {
        let len = ((((b[pos + 2] & 7) as usize) << 8 | b[pos + 3] as usize) + 1) * 2;
        b[pos + at] = f(b[pos + at]);
        let crc = crc16(&b[pos + 2..pos + len - 2]);
        b[pos + len - 2..pos + len].copy_from_slice(&crc.to_be_bytes());
        pos += len;
    }
}

#[test]
fn unsupported_streams() {
    let mut b = media("eac3_51.eac3");
    edit_eac3(&mut b, 2, |x| (x & 0x3f) | 0x40);
    let (_, out) = run(&temp("dep.eac3", &b));
    assert!(
        error(&out)
            .contains(r#""code":"UNSUPPORTED_FEATURE","message":"frame 0 at byte 0 is an E-AC-3 dependent substream""#),
        "{out}"
    );

    let mut b = media("eac3_51.eac3");
    edit_eac3(&mut b, 2, |x| (x & 0xc7) | 0x08);
    let (_, out) = run(&temp("sub1.eac3", &b));
    assert!(error(&out).contains("is E-AC-3 independent substream 1"), "{out}");

    let mut b = media("ac3_stereo.ac3");
    b[5] = (b[5] & 7) | 9 << 3;
    let (_, out) = run(&temp("bsid9.ac3", &b));
    assert!(error(&out).contains(r#""code":"UNSUPPORTED_CODEC_VARIANT","message":"frame 0 has bsid 9"#), "{out}");
}

#[test]
fn layout_change_is_inconsistent() {
    let mut b = media("ac3_stereo.ac3");
    b.extend(media("eac3_51.eac3"));
    let (code, out) = run(&temp("mix.ac3", &b));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(units(&out).len(), 32);
    assert!(
        error(&out).contains(r#""code":"INCONSISTENT_TRACK_PARAMETERS","message":"frame 32 at byte 24576 changes codec/sample rate/acmod/LFE from (Ac3, 48000, 2, false) to (Eac3, 48000, 7, true)""#),
        "{out}"
    );
}
