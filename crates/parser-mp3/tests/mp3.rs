//! Golden files for real encoder output and failure cases derived from them.
//! Set `UPDATE_GOLDEN=1` to rewrite the golden files after an intended change.

use std::path::{Path, PathBuf};

use vmkv_parser_mp3::Mp3;
use vtj::cli;
use vtj::validate::{validate, Options, Outcome};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn media(name: &str) -> PathBuf {
    root().join("testdata/media").join(name)
}

fn run(input: &Path) -> (i32, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cli::run(&Mp3, &[input.to_string_lossy().into_owned()], &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-mp3-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

fn check_valid(out: &str) -> Outcome {
    let r = validate(out.as_bytes(), &Options { codec_aware: true, max_problems: 0 });
    assert_ne!(r.outcome, Outcome::Invalid, "{:?}", r.problems);
    r.outcome
}

fn units(out: &str) -> Vec<&str> {
    out.lines().filter(|l| l.starts_with(r#"{"type":"unit""#)).collect()
}

fn error_line(out: &str) -> &str {
    out.lines().last().unwrap()
}

#[test]
fn golden_outputs() {
    for name in ["mp3_cbr_lame", "mp3_vbr_mono_mpeg2", "mp3_plain"] {
        let (code, out) = run(&media(&format!("{name}.mp3")));
        assert_eq!(code, 0, "{name}: {out}");
        assert_eq!(check_valid(&out), Outcome::Success);
        let golden = root().join(format!("testdata/golden/mp3/{name}.vtj"));
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::write(&golden, &out).unwrap();
        }
        assert_eq!(out, std::fs::read_to_string(&golden).unwrap(), "{name}");
        assert_eq!(run(&media(&format!("{name}.mp3"))).1, out, "rule 8: {name}");
    }
}

#[test]
fn lame_gapless_timing_ends_exactly_at_one_second() {
    for name in ["mp3_cbr_lame", "mp3_vbr_mono_mpeg2"] {
        let (_, out) = run(&media(&format!("{name}.mp3")));
        let u = units(&out);
        let first: serde_like::Unit = serde_like::parse(u[0]);
        let last: serde_like::Unit = serde_like::parse(u[u.len() - 1]);
        assert!(first.pts < 0, "{name}: timeline starts before 0 by the codec delay");
        assert_eq!(last.pts + last.dur - last.discard.unwrap(), 1_000_000_000, "{name}");
        assert!(out.contains(&format!(r#""codec_delay_ns":{}"#, -first.pts)), "{name}");
    }
}

#[test]
fn plain_stream_has_no_invented_delay() {
    let (_, out) = run(&media("mp3_plain.mp3"));
    assert!(!out.contains("codec_delay_ns") && !out.contains("discard_padding_ns"));
    assert!(units(&out)[0].contains(r#""pts_ns":0,"#));
}

#[test]
fn trailing_id3v1_is_skipped() {
    let mut b = std::fs::read(media("mp3_plain.mp3")).unwrap();
    let mut tag = b"TAG".to_vec();
    tag.resize(128, b' ');
    b.extend(tag);
    let (code, out) = run(&temp("v1.mp3", &b));
    assert_eq!(code, 0);
    assert_eq!(units(&out), units(&run(&media("mp3_plain.mp3")).1));
}

#[test]
fn truncated_last_frame() {
    let b = std::fs::read(media("mp3_plain.mp3")).unwrap();
    let (code, out) = run(&temp("cut.mp3", &b[..b.len() - 100]));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(check_valid(&out), Outcome::Failure);
    assert_eq!(
        error_line(&out),
        format!(
            r#"{{"type":"error","code":"TRUNCATED_BITSTREAM","message":"frame 42 cut at byte {}"}}"#,
            b.len() - 100
        )
    );
    assert!(units(&out).len() <= 42, "units written before the error are allowed but not required");
}

#[test]
fn junk_between_frames() {
    let mut b = std::fs::read(media("mp3_plain.mp3")).unwrap();
    b.splice(384..384, [0u8; 3]);
    let (code, out) = run(&temp("junk.mp3", &b));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(check_valid(&out), Outcome::Failure);
    assert!(error_line(&out).contains(r#""code":"INVALID_BITSTREAM","message":"no frame sync at byte 384""#));
}

#[test]
fn other_layers_are_another_codec() {
    let mut b = std::fs::read(media("mp3_plain.mp3")).unwrap();
    b[1] = (b[1] & !0x06) | 0x04;
    let (code, out) = run(&temp("l2.mp3", &b));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert!(error_line(&out).contains(r#""code":"UNSUPPORTED_CODEC_VARIANT","message":"MPEG audio layer 2 at byte 0""#));
}

#[test]
fn sample_rate_change_is_inconsistent() {
    let mut b = std::fs::read(media("mp3_plain.mp3")).unwrap();
    let other = std::fs::read(media("mp3_vbr_mono_mpeg2.mp3")).unwrap();
    b.extend(&other[227..]);
    let (code, out) = run(&temp("mixed.mp3", &b));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert!(
        error_line(&out).contains(
            r#""code":"INCONSISTENT_TRACK_PARAMETERS","message":"frame 43 changes from 48000 Hz 2 ch to 22050 Hz 1 ch""#
        ),
        "{out}"
    );
}

#[test]
fn only_tags_is_invalid() {
    let b = std::fs::read(media("mp3_cbr_lame.mp3")).unwrap();
    let (code, out) = run(&temp("tags.mp3", &b[..61]));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert!(error_line(&out).contains(r#""message":"no audio frames""#));
}

#[test]
fn frame_count_mismatch_drops_padding_but_keeps_delay() {
    let b = std::fs::read(media("mp3_cbr_lame.mp3")).unwrap();
    let (_, full) = run(&media("mp3_cbr_lame.mp3"));
    let last: serde_like::Unit = serde_like::parse(units(&full)[39]);
    let (code, out) = run(&temp("short.mp3", &b[..last.offset as usize]));
    assert_eq!(code, 0);
    assert_eq!(check_valid(&out), Outcome::Success);
    assert_eq!(units(&out).len(), 39);
    assert!(!out.contains("discard_padding_ns"), "the LAME padding no longer describes this file");
    assert!(out.contains(r#""codec_delay_ns":25056689"#));
}

/// Real LAME encodes often declare more padding than one frame after the
/// 529-sample decoder delay (1684 − 529 = 1155 > 1152): the padding then
/// spans the last two frames.
#[test]
fn padding_longer_than_one_frame_spans_several_units() {
    let mut b = std::fs::read(media("mp3_cbr_lame.mp3")).unwrap();
    let (frame, lame) = (61usize, 61 + 156);
    let padding: u32 = 1684;
    b[lame + 22] = (b[lame + 22] & 0xf0) | (padding >> 8) as u8;
    b[lame + 23] = padding as u8;
    let crc = vmkv_parser_mp3::crc16(&b[frame..lame + 34]);
    b[lame + 34..lame + 36].copy_from_slice(&crc.to_be_bytes());
    let (code, out) = run(&temp("pad.mp3", &b));
    assert_eq!(code, 0, "{out}");
    assert_eq!(check_valid(&out), Outcome::Success);
    let u = units(&out);
    let last: serde_like::Unit = serde_like::parse(u[u.len() - 1]);
    let prev: serde_like::Unit = serde_like::parse(u[u.len() - 2]);
    assert_eq!(last.discard, Some(last.dur), "the last frame is padding entirely");
    let rate = vtj::Rational::new(44100, 1);
    let audible = vtj::ticks_to_ns(40 * 1152 - 1105 - 1155, rate).unwrap();
    assert_eq!(prev.pts + prev.dur - prev.discard.unwrap(), audible, "3 samples of the previous frame");
    assert!(serde_like::parse(u[u.len() - 3]).discard.is_none());
}

/// Just enough field extraction for these tests.
mod serde_like {
    pub struct Unit {
        pub pts: i64,
        pub dur: i64,
        pub discard: Option<i64>,
        pub offset: u64,
    }

    fn field(line: &str, key: &str) -> Option<i64> {
        let i = line.find(&format!("\"{key}\":"))? + key.len() + 3;
        let end = line[i..].find(|c: char| c != '-' && !c.is_ascii_digit()).map_or(line.len(), |e| i + e);
        line[i..end].parse().ok()
    }

    pub fn parse(line: &str) -> Unit {
        let off = line.find(r#"["src",0,"#).unwrap() + 9;
        let offset = line[off..].split(',').next().unwrap().parse().unwrap();
        Unit {
            pts: field(line, "pts_ns").unwrap(),
            dur: field(line, "duration_ns").unwrap(),
            discard: field(line, "discard_padding_ns"),
            offset,
        }
    }
}
