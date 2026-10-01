//! Golden files for real encoder output, plus synthetic streams (verbatim
//! subframes, so every byte is under the test's control) for the cases a
//! real encoder does not produce. Set `UPDATE_GOLDEN=1` to rewrite the
//! golden files after an intended change.

use std::path::{Path, PathBuf};

use vmkv_parser_flac::{crc16, crc8, Flac};
use vtj::cli;
use vtj::validate::{validate, Options, Outcome};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(input: &Path) -> (i32, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cli::run(&Flac, &[input.as_os_str().to_os_string()], &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-flac-{}", std::process::id()));
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

/// UTF-8-like coded number.
fn coded(n: u64) -> Vec<u8> {
    if n < 0x80 {
        return vec![n as u8];
    }
    let extra = (1..=6).find(|&e| n < 1u64 << (6 * e + 6 - e)).unwrap();
    let mut v = vec![(0xff00u16 >> (extra + 1)) as u8 | (n >> (6 * extra)) as u8];
    for i in (0..extra).rev() {
        v.push(0x80 | ((n >> (6 * i)) & 0x3f) as u8);
    }
    v
}

/// A frame of 16-bit verbatim silence with block size `bs` and `ch`
/// independent channels; rate and sample size come from STREAMINFO.
fn frame(variable: bool, number: u64, bs: u32, ch: u8) -> Vec<u8> {
    let mut f = vec![0xff, 0xf8 | variable as u8, 0x70, (ch - 1) << 4];
    f.extend(coded(number));
    f.extend(((bs - 1) as u16).to_be_bytes());
    f.push(crc8(&f));
    for _ in 0..ch {
        f.push(0x02);
        f.extend(std::iter::repeat_n(0, bs as usize * 2));
    }
    f.extend(crc16(&f).to_be_bytes());
    f
}

fn stream(min_bs: u32, max_bs: u32, ch: u8, total: u64, frames: &[Vec<u8>]) -> Vec<u8> {
    let mut s = b"fLaC".to_vec();
    s.extend([0x80, 0, 0, 34]);
    s.extend((min_bs as u16).to_be_bytes());
    s.extend((max_bs as u16).to_be_bytes());
    s.extend([0; 6]);
    let packed: u64 = 8000 << 44 | ((ch as u64 - 1) << 41) | (15 << 36) | total;
    s.extend(packed.to_be_bytes());
    s.extend([0; 16]);
    for f in frames {
        s.extend(f);
    }
    s
}

#[test]
fn golden_outputs() {
    for name in ["flac_stereo", "flac_mono_24"] {
        let input = root().join(format!("testdata/media/{name}.flac"));
        let (code, out) = run(&input);
        assert_eq!(code, 0, "{out}");
        assert_eq!(outcome(&out), Outcome::Success);
        let golden = root().join(format!("testdata/golden/flac/{name}.vtj"));
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &out).unwrap();
        }
        assert_eq!(out, std::fs::read_to_string(&golden).unwrap(), "{name}");
        assert_eq!(run(&input).1, out, "rule 8");
        assert!(out.contains(r#""codec_private":[["src",0,0,8256]]"#), "marker and every metadata block");
    }
}

#[test]
fn synthetic_fixed_stream() {
    let frames: Vec<_> = (0..3).map(|n| frame(false, n, 4000, 2)).chain([frame(false, 3, 1000, 2)]).collect();
    let (code, out) = run(&temp("fixed.flac", &stream(4000, 4000, 2, 13000, &frames)));
    assert_eq!(code, 0, "{out}");
    assert_eq!(outcome(&out), Outcome::Success);
    let u = units(&out);
    assert_eq!(u.len(), 4);
    assert!(u[1].starts_with(r#"{"type":"unit","pts_ns":500000000,"duration_ns":500000000,"#), "{}", u[1]);
    assert!(u[3].contains(r#""duration_ns":125000000,"#), "short last frame: {}", u[3]);
    let len = frames[0].len() as u64;
    assert!(u[1].contains(&format!(r#""payload":[["src",0,{},{len}]]"#, 42 + len)), "{}", u[1]);
    assert!(out.contains(r#""audio":{"sampling_frequency":[8000,1],"channels":2,"bit_depth":16}"#));
}

#[test]
fn variable_block_size_uses_sample_numbers() {
    let frames = [frame(true, 0, 100, 1), frame(true, 100, 300, 1), frame(true, 400, 50, 1)];
    let (code, out) = run(&temp("var.flac", &stream(16, 300, 1, 450, &frames)));
    assert_eq!(code, 0, "{out}");
    let u = units(&out);
    assert!(u[1].starts_with(r#"{"type":"unit","pts_ns":12500000,"duration_ns":37500000,"#), "{}", u[1]);
    assert!(u[2].starts_with(r#"{"type":"unit","pts_ns":50000000,"duration_ns":6250000,"#), "{}", u[2]);

    // A gap in sample numbers is not a frame boundary: no header with the
    // expected number ever follows, so the first frame runs past the
    // longest it could be.
    let frames = [frame(true, 0, 100, 1), frame(true, 101, 300, 1)];
    let (code, out) = run(&temp("gap.flac", &stream(16, 300, 1, 0, &frames)));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(
        error(&out)
            .contains(r#""code":"INVALID_BITSTREAM","message":"frame 0 at byte 42: no frame end within 237 bytes""#),
        "{out}"
    );
}

#[test]
fn stream_not_starting_at_zero_keeps_its_time() {
    // A cut stream: frame numbers start at 5 (rule 4, no shift to 0).
    let frames = [frame(false, 5, 800, 1), frame(false, 6, 800, 1)];
    let (code, out) = run(&temp("cut_start.flac", &stream(800, 800, 1, 0, &frames)));
    assert_eq!(code, 0, "{out}");
    assert!(units(&out)[0].starts_with(r#"{"type":"unit","pts_ns":500000000,"#), "{out}");
}

#[test]
fn tags_around_the_stream() {
    let frames = [frame(false, 0, 800, 1), frame(false, 1, 800, 1)];
    let mut b = vec![b'I', b'D', b'3', 4, 0, 0, 0, 0, 0, 5, 1, 2, 3, 4, 5];
    b.extend(stream(800, 800, 1, 1600, &frames));
    let mut v1 = b"TAG".to_vec();
    v1.resize(128, b' ');
    b.extend(&v1);
    let (code, out) = run(&temp("tags.flac", &b));
    assert_eq!(code, 0, "{out}");
    assert!(out.contains(r#""codec_private":[["src",0,15,42]]"#), "{out}");
    let u = units(&out);
    assert_eq!(u.len(), 2);
    assert!(u[1].contains(&format!(r#"["src",0,{},{}]"#, 57 + frames[0].len(), frames[1].len())), "{}", u[1]);
}

#[test]
fn truncated_and_corrupt() {
    let b = std::fs::read(root().join("testdata/media/flac_stereo.flac")).unwrap();
    let (code, out) = run(&temp("cut.flac", &b[..b.len() - 10]));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert_eq!(units(&out).len(), 9, "complete frames before the cut are still described");
    assert!(error(&out).contains(r#""code":"TRUNCATED_BITSTREAM","message":"frame 9 cut at byte 20652""#), "{out}");

    // A flipped byte in the last frame cannot be told from a cut.
    let mut c = b.clone();
    let n = c.len();
    c[n - 100] ^= 0x10;
    let (_, out) = run(&temp("crc.flac", &c));
    assert!(error(&out).contains(r#""code":"TRUNCATED_BITSTREAM","message":"frame 9 cut at byte 20662""#), "{out}");

    // A flipped byte mid-stream: no CRC-valid end before the next header,
    // so the frame runs on past its maximum length or to the end.
    let mut c = b.clone();
    c[9000] ^= 0x10;
    let (code, out) = run(&temp("mid.flac", &c));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(units(&out).len(), 0, "{out}");

    // Fewer samples than STREAMINFO declares.
    let frames = [frame(false, 0, 800, 1)];
    let (_, out) = run(&temp("short.flac", &stream(800, 800, 1, 1600, &frames)));
    assert!(
        error(&out).contains(
            r#""code":"TRUNCATED_BITSTREAM","message":"STREAMINFO declares 1600 samples, the frames carry 800""#
        ),
        "{out}"
    );
}

#[test]
fn header_problems() {
    let (_, out) = run(&temp("nomarker.flac", b"OggS\0\0\0\0"));
    assert!(
        error(&out).contains(r#""code":"MISSING_INITIALIZATION_DATA","message":"no fLaC marker at byte 0""#),
        "{out}"
    );

    let mut s = stream(800, 800, 1, 0, &[frame(false, 0, 800, 1)]);
    s[4] = 0x84; // first block VORBIS_COMMENT
    let (_, out) = run(&temp("first.flac", &s));
    assert!(error(&out).contains("the first metadata block is not STREAMINFO"), "{out}");

    // Two channels in a frame of a mono stream.
    let (_, out) = run(&temp("ch.flac", &stream(800, 800, 1, 0, &[frame(false, 0, 800, 2)])));
    assert!(
        error(&out).contains(
            r#""code":"INCONSISTENT_TRACK_PARAMETERS","message":"frame 0 at byte 42 has 2 channels, STREAMINFO 1""#
        ),
        "{out}"
    );

    // A frame longer than the fixed block size of the first one.
    let frames = [frame(false, 0, 800, 1), frame(false, 1, 900, 1)];
    let (_, out) = run(&temp("grow.flac", &stream(800, 900, 1, 0, &frames)));
    assert!(error(&out).contains("has block size 900 over the fixed 800"), "{out}");

    // A short frame that is not the last.
    let frames = [frame(false, 0, 800, 1), frame(false, 1, 400, 1), frame(false, 2, 800, 1)];
    let (_, out) = run(&temp("short_mid.flac", &stream(400, 800, 1, 0, &frames)));
    assert!(error(&out).contains("follows a frame shorter than the fixed block size"), "{out}");

    let (_, out) = run(&temp("empty.flac", &stream(800, 800, 1, 0, &[])));
    assert!(error(&out).contains(r#""message":"no FLAC frames""#), "{out}");
}
