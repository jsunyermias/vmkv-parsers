//! Golden files for FFmpeg TrueHD encoder output and failure cases derived
//! from it (real tracks were verified outside the repository, decision 70).
//! Set `UPDATE_GOLDEN=1` to rewrite the golden files after an intended change.

use std::path::{Path, PathBuf};

use vmkv_parser_truehd::TrueHd;
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
    let code = cli::run(&TrueHd, &[input.as_os_str().to_os_string()], &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-truehd-{}", std::process::id()));
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

/// `(offset, length)` of every access unit.
fn access_units(b: &[u8]) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    let mut p = 0;
    while p < b.len() {
        let len = (u16::from_be_bytes([b[p], b[p + 1]]) & 0xfff) as usize * 2;
        v.push((p, len));
        p += len;
    }
    v
}

#[test]
fn golden_outputs() {
    for name in ["truehd_51", "truehd_stereo_96k"] {
        let input = root().join(format!("testdata/media/{name}.thd"));
        let (code, out) = run(&input);
        assert_eq!(code, 0, "{out}");
        assert_eq!(outcome(&out), Outcome::Success);
        let golden = root().join(format!("testdata/golden/truehd/{name}.vtj"));
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &out).unwrap();
        }
        assert_eq!(out, std::fs::read_to_string(&golden).unwrap(), "{name}");
        assert_eq!(run(&input).1, out, "rule 8");
        assert!(!out.contains("codec_private"), "the A_TRUEHD mapping has none");
    }
    let (_, out) = run(&root().join("testdata/media/truehd_51.thd"));
    let u = units(&out);
    assert_eq!(u.len(), 600);
    // 40 samples at 48 kHz per access unit; only major syncs are keyframes.
    assert!(u[1].starts_with(r#"{"type":"unit","pts_ns":833333,"duration_ns":833334,"flags":[],"#), "{}", u[1]);
    assert!(u[0].contains(r#""flags":["random_access"]"#));
    assert!(out.contains(r#""codec_id":"A_TRUEHD","audio":{"sampling_frequency":[48000,1],"channels":6}"#));
    let (_, out) = run(&root().join("testdata/media/truehd_stereo_96k.thd"));
    // 80 samples at 96 kHz: still 1/1200 s.
    assert!(units(&out)[1].starts_with(r#"{"type":"unit","pts_ns":833333,"#), "{out}");
}

#[test]
fn damaged_streams() {
    let b = media("truehd_51.thd");
    let aus = access_units(&b);

    let (code, out) = run(&temp("cut.thd", &b[..b.len() - 3]));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert_eq!(units(&out).len(), 599);
    assert!(error(&out).contains(r#""code":"TRUNCATED_BITSTREAM","message":"access unit 599 cut at byte"#), "{out}");

    // Flip a bit of a substream directory: the check nibble no longer holds.
    let (off, _) = aus[1];
    let mut p = b.clone();
    p[off + 5] ^= 0x10;
    let (_, out) = run(&temp("parity.thd", &p));
    assert!(
        error(&out).contains(&format!(r#""message":"access unit 1 at byte {off}: check nibble mismatch""#)),
        "{out}"
    );

    // Starting after the first access unit: no major sync to start from.
    let (_, out) = run(&temp("nosync.thd", &b[aus[1].0..]));
    assert!(
        error(&out).contains(
            r#""code":"MISSING_INITIALIZATION_DATA","message":"the first access unit carries no major sync""#
        ),
        "{out}"
    );

    // MLP's format sync.
    let mut m = b.clone();
    m[7] = 0xbb;
    let (_, out) = run(&temp("mlp.thd", &m));
    assert!(
        error(&out)
            .contains(r#""code":"UNSUPPORTED_CODEC_VARIANT","message":"access unit 0 at byte 0 is MLP, not TrueHD""#),
        "{out}"
    );

    let (_, out) = run(&temp("empty.thd", b""));
    assert!(error(&out).contains(r#""message":"no TrueHD access units""#), "{out}");
}

#[test]
fn parameter_change_is_inconsistent() {
    let mut b = media("truehd_51.thd");
    let first = b.len();
    b.extend(media("truehd_stereo_96k.thd"));
    let (code, out) = run(&temp("mix.thd", &b));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(units(&out).len(), 600);
    assert!(
        error(&out).contains(&format!(r#""code":"INCONSISTENT_TRACK_PARAMETERS","message":"access unit 600 at byte {first} changes sample rate/channels/substreams"#)),
        "{out}"
    );
}
