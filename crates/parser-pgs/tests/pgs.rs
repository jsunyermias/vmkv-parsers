//! Golden file for a synthetic `.sup` (decodable by FFmpeg) and failure
//! cases derived from it.
//! Set `UPDATE_GOLDEN=1` to rewrite the golden files after an intended change.

use std::path::{Path, PathBuf};

use vmkv_parser_pgs::Pgs;
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
    let code = cli::run(&Pgs, &[input.as_os_str().to_os_string()], &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-pgs-{}", std::process::id()));
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
fn golden_output() {
    let input = root().join("testdata/media/pgs_sample.sup");
    let (code, out) = run(&input);
    assert_eq!(code, 0, "{out}");
    assert_eq!(outcome(&out), Outcome::Success);
    let golden = root().join("testdata/golden/pgs/pgs_sample.vtj");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
        std::fs::write(&golden, &out).unwrap();
    }
    assert_eq!(out, std::fs::read_to_string(&golden).unwrap());
    assert_eq!(run(&input).1, out, "rule 8");
    let u = units(&out);
    assert_eq!(u.len(), 5);
    // A whole display set, each segment without its PG, PTS and DTS.
    assert!(
        u[0].ends_with(
            r#""payload":[["src",0,10,22],["src",0,42,13],["src",0,65,10],["src",0,85,24],["src",0,119,3]]}"#
        ),
        "{}",
        u[0]
    );
    assert!(u.iter().all(|u| u.contains(r#""duration_ns":-1,"#)), "shown until the next display set");
    // The normal-case update with an object depends on the previous state.
    let flags: Vec<bool> = u.iter().map(|u| u.contains("random_access")).collect();
    assert_eq!(flags, [true, true, true, false, true]);
    assert!(out.contains(r#"{"type":"track","track_type":"subtitle","codec_id":"S_HDMV/PGS"}"#));
}

#[test]
fn broken_structure() {
    let b = media("pgs_sample.sup");
    let (code, out) = run(&temp("cut.sup", &b[..b.len() - 13]));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert_eq!(units(&out).len(), 4);
    assert!(
        error(&out)
            .contains(r#""code":"TRUNCATED_BITSTREAM","message":"display set 4 has no END segment at byte 419""#),
        "{out}"
    );

    let (_, out) = run(&temp("hdr.sup", &b[..b.len() - 1]));
    assert!(error(&out).contains(r#""message":"segment header at byte 419 cut at byte 431""#), "{out}");

    let mut j = b.clone();
    j[122] = b'X';
    let (_, out) = run(&temp("junk.sup", &j));
    assert!(
        error(&out).contains(r#""code":"INVALID_BITSTREAM","message":"no PG segment header at byte 122""#),
        "{out}"
    );

    // The first display set without its PCS: it starts with the WDS.
    let (_, out) = run(&temp("nopcs.sup", &b[32..]));
    assert!(error(&out).contains("display set 0 at byte 0 starts with segment type 0x17 instead of a PCS"), "{out}");

    // END of the first set turned into a PCS.
    let mut p = b.clone();
    p[119 + 10 - 10] = 0x16;
    let (_, out) = run(&temp("pcs2.sup", &p));
    assert!(error(&out).contains("a second PCS at byte 109 before END"), "{out}");

    let mut k = b.clone();
    k[42 + 10 - 10] = 0x99;
    let (_, out) = run(&temp("kind.sup", &k));
    assert!(error(&out).contains("unknown segment type 0x99 at byte 32"), "{out}");

    // No display sets is an empty track (decision 68).
    let (code, out) = run(&temp("empty.sup", b""));
    assert_eq!(code, 0, "{out}");
    assert_eq!(outcome(&out), Outcome::Success);
    assert!(out.ends_with("{\"type\":\"end\",\"unit_count\":0}\n"), "{out}");
}
