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

/// An extension substream with static fields and one asset descriptor
/// (`rate` is the 4-bit sample rate code), padded with `body` zero bytes.
fn exss(rate: u32, channels: u32, bits: u32, body: usize) -> Vec<u8> {
    let fields = |hdr: u64, size: u64| -> Vec<(u64, usize)> {
        vec![
            (0x6458_2025, 32),
            (0, 8),
            (0, 2),
            (0, 1),
            (hdr - 1, 8),
            (size - 1, 16),
            (1, 1),
            (0, 2),
            (0, 3),
            (0, 1),
            (0, 3),
            (0, 3),
            (1, 1),
            (1, 8),
            (0, 1),
            (body as u64 - 1, 16),
            (12, 9),
            (0, 3),
            (0, 1),
            (0, 1),
            (0, 1),
            (bits as u64 - 1, 5),
            (rate as u64, 4),
            (channels as u64 - 1, 8),
        ]
    };
    let nbits: usize = fields(1, 1).iter().map(|f| f.1).sum();
    let hdr = nbits.div_ceil(8) as u64;
    let mut out = vec![0u8; hdr as usize];
    let mut pos = 0;
    for (v, n) in fields(hdr, hdr + body as u64) {
        for i in (0..n).rev() {
            if v >> i & 1 == 1 {
                out[pos / 8] |= 0x80 >> (pos % 8);
            }
            pos += 1;
        }
    }
    out.resize(hdr as usize + body, 0);
    out
}

/// The core frames of the 5.1 fixture, each followed by `ext(i)`.
fn with_extension(ext: impl Fn(usize) -> Vec<u8>) -> Vec<u8> {
    let core = media("dts_51.dts");
    let mut out = Vec::new();
    for (i, f) in core.chunks(1884).enumerate() {
        out.extend(f);
        out.extend(ext(i));
    }
    out
}

#[test]
fn dts_hd_units_carry_core_and_extension() {
    // 7.1 at 48 kHz (code 12), 24 bit, as in a real MA 7.1 track.
    let x = exss(12, 8, 24, 40);
    let b = with_extension(|_| x.clone());
    let (code, out) = run(&temp("hd.dts", &b));
    assert_eq!(code, 0, "{out}");
    assert_eq!(outcome(&out), Outcome::Success);
    let u = units(&out);
    assert_eq!(u.len(), 94);
    let unit = 1884 + x.len() as u64;
    assert!(u[1].contains(&format!(r#""payload":[["src",0,{unit},{unit}]]"#)), "{}", u[1]);
    // Channels and rate from the asset descriptor, not the 5.1 core; times
    // still from the core's 512 samples per frame.
    assert!(out.contains(r#""codec_id":"A_DTS","audio":{"sampling_frequency":[48000,1],"channels":8}"#), "{out}");
    assert!(u[1].starts_with(r#"{"type":"unit","pts_ns":10666667,"#), "{}", u[1]);
}

#[test]
fn dts_hd_inconsistencies() {
    let x = exss(12, 8, 24, 40);
    let b = with_extension(|i| if i == 5 { Vec::new() } else { x.clone() });
    let (_, out) = run(&temp("gap.dts", &b));
    assert!(
        error(&out).contains(r#""code":"INCONSISTENT_TRACK_PARAMETERS""#)
            && error(&out).contains("lacks the DTS-HD extension"),
        "{out}"
    );

    let b = with_extension(|i| if i == 3 { exss(13, 8, 24, 40) } else { x.clone() });
    let (_, out) = run(&temp("rate.dts", &b));
    assert!(error(&out).contains("changes sample rate/channels from (48000, 8) to (96000, 8)"), "{out}");

    // No core at all: DTS Express-like.
    let (_, out) = run(&temp("express.dts", &x.repeat(3)));
    assert!(
        error(&out).contains(r#""code":"UNSUPPORTED_FEATURE","message":"frame 0 at byte 0 is a DTS-HD extension substream without a core frame""#),
        "{out}"
    );

    // A substream cut by the end of the file.
    let mut cut = with_extension(|_| x.clone());
    cut.truncate(cut.len() - 10);
    let (_, out) = run(&temp("cut.dts", &cut));
    assert!(error(&out).contains(r#""code":"TRUNCATED_BITSTREAM","message":"frame 93 substream cut at byte"#), "{out}");
}
