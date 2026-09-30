//! Golden file for real encoder output and failure cases derived from it.
//! Set `UPDATE_GOLDEN=1` to rewrite the golden file after an intended change.

use std::path::{Path, PathBuf};

use vmkv_parser_aac::Aac;
use vtj::cli;
use vtj::validate::{validate, Options, Outcome};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn source() -> Vec<u8> {
    std::fs::read(root().join("testdata/media/aac_lc.aac")).unwrap()
}

fn run(input: &Path) -> (i32, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cli::run(&Aac, &[input.to_string_lossy().into_owned()], &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-aac-{}", std::process::id()));
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

/// `(offset, frame_len)` of every ADTS frame.
fn frames(b: &[u8]) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    let mut p = 0;
    while p < b.len() {
        let len = (((b[p + 3] & 3) as usize) << 11) | ((b[p + 4] as usize) << 3) | (b[p + 5] as usize >> 5);
        v.push((p, len));
        p += len;
    }
    v
}

fn set_len(h: &mut [u8], len: usize) {
    h[3] = (h[3] & !3) | (len >> 11) as u8;
    h[4] = (len >> 3) as u8;
    h[5] = (h[5] & 0x1f) | ((len & 7) << 5) as u8;
}

#[test]
fn golden_output() {
    let (code, out) = run(&root().join("testdata/media/aac_lc.aac"));
    assert_eq!(code, 0, "{out}");
    assert_eq!(outcome(&out), Outcome::Success);
    let golden = root().join("testdata/golden/aac/aac_lc.vtj");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden, &out).unwrap();
    }
    assert_eq!(out, std::fs::read_to_string(&golden).unwrap());
    assert_eq!(run(&root().join("testdata/media/aac_lc.aac")).1, out, "rule 8");
    assert!(out.contains(r#""codec_private":[["inline","EhA="]]"#));
    assert!(!out.contains("codec_delay_ns"), "ADTS carries no encoder delay; none is invented");
}

#[test]
fn crc_protected_frames_skip_nine_header_bytes() {
    let b = source();
    let mut out_bytes = Vec::new();
    for (off, len) in frames(&b) {
        let mut h = b[off..off + 7].to_vec();
        h[1] &= !1;
        set_len(&mut h, len + 2);
        out_bytes.extend(&h);
        out_bytes.extend([0xab, 0xcd]);
        out_bytes.extend(&b[off + 7..off + len]);
    }
    let (code, out) = run(&temp("crc.aac", &out_bytes));
    assert_eq!(code, 0, "{out}");
    assert_eq!(outcome(&out), Outcome::Success);
    let first = frames(&b)[0];
    assert!(units(&out)[0].contains(&format!(r#""payload":[["src",0,9,{}]]"#, first.1 - 7)), "{}", units(&out)[0]);
    assert_eq!(units(&out).len(), frames(&b).len());
}

#[test]
fn leading_id3v2_is_skipped() {
    let mut b = vec![b'I', b'D', b'3', 4, 0, 0, 0, 0, 0, 5, 1, 2, 3, 4, 5];
    b.extend(source());
    let (code, out) = run(&temp("id3.aac", &b));
    assert_eq!(code, 0);
    assert!(units(&out)[0].contains(r#"["src",0,22,"#), "{}", units(&out)[0]);
}

#[test]
fn truncated_and_junk() {
    let b = source();
    let (code, out) = run(&temp("cut.aac", &b[..b.len() - 5]));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(
        out.ends_with(&format!(
            "\"code\":\"TRUNCATED_BITSTREAM\",\"message\":\"frame 44 cut at byte {}\"}}\n",
            b.len() - 5
        )),
        "{out}"
    );

    let second = frames(&b)[1].0;
    let mut j = b.clone();
    j.insert(second, 0);
    let (_, out) = run(&temp("junk.aac", &j));
    assert!(
        out.ends_with(&format!("\"code\":\"INVALID_BITSTREAM\",\"message\":\"no ADTS sync at byte {second}\"}}\n")),
        "{out}"
    );
}

#[test]
fn channel_change_is_inconsistent() {
    let mut b = source();
    let (off, _) = frames(&b)[3];
    b[off + 3] = (b[off + 3] & 0x3f) | (1 << 6);
    b[off + 2] &= !1;
    let (code, out) = run(&temp("ch.aac", &b));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert!(out.contains(r#""code":"INCONSISTENT_TRACK_PARAMETERS","message":"frame 3 changes object type/sampling index/channels from (2, 4, 2) to (2, 4, 1)""#), "{out}");
}

#[test]
fn unsupported_layouts() {
    let mut b = source();
    b[6] |= 1;
    let (_, out) = run(&temp("blocks.aac", &b));
    assert!(out.contains(r#""code":"UNSUPPORTED_FEATURE","message":"frame 0 carries 2 raw data blocks"#), "{out}");

    let mut b = source();
    for (off, _) in frames(&b.clone()) {
        b[off + 2] &= !1;
        b[off + 3] &= 0x3f;
    }
    let (_, out) = run(&temp("pce.aac", &b));
    assert!(out.contains(r#""code":"UNSUPPORTED_FEATURE","message":"channel configuration 0"#), "{out}");
}
