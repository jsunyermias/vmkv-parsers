//! Every parameter of vmkv-parser-mp3: its effect, its record in
//! `header.params` and its conflicts.

use std::path::{Path, PathBuf};

use vmkv_parser_mp3::{crc16, Mp3};
use vtj::cli;
use vtj::validate::{validate, Options, Outcome};

fn media(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/media").join(name)
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-mp3-params-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

struct Out {
    code: i32,
    text: String,
    err: String,
}

impl Out {
    fn units(&self) -> Vec<&str> {
        self.text.lines().filter(|l| l.starts_with(r#"{"type":"unit""#)).collect()
    }

    fn field(line: &str, key: &str) -> Option<i64> {
        let i = line.find(&format!("\"{key}\":"))? + key.len() + 3;
        let end = line[i..].find(|c: char| c != '-' && !c.is_ascii_digit()).map_or(line.len(), |e| i + e);
        line[i..end].parse().ok()
    }

    fn codec_delay(&self) -> Option<i64> {
        self.text.lines().find(|l| l.contains(r#""type":"track""#)).and_then(|l| Self::field(l, "codec_delay_ns"))
    }

    /// End of the audible part: the minimum of `pts + duration − discard`
    /// over trimmed units, else the end of the last unit.
    fn audible_end(&self) -> i64 {
        let u = self.units();
        let trimmed: Vec<i64> = u
            .iter()
            .filter_map(|l| {
                Self::field(l, "discard_padding_ns")
                    .map(|d| Self::field(l, "pts_ns").unwrap() + Self::field(l, "duration_ns").unwrap() - d)
            })
            .collect();
        trimmed.into_iter().min().unwrap_or_else(|| {
            let l = u[u.len() - 1];
            Self::field(l, "pts_ns").unwrap() + Self::field(l, "duration_ns").unwrap()
        })
    }
}

fn run(args: &[&str], input: &Path) -> Out {
    let mut a: Vec<std::ffi::OsString> = args.iter().map(std::ffi::OsString::from).collect();
    a.push(input.as_os_str().to_os_string());
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cli::run(&Mp3, &a, &mut out, &mut err);
    let text = String::from_utf8(out).unwrap();
    if code != cli::EXIT_USAGE {
        let r = validate(text.as_bytes(), &Options { codec_aware: true, max_problems: 0 });
        assert_ne!(r.outcome, Outcome::Invalid, "{args:?}: {:?}", r.problems);
    }
    Out { code, text, err: String::from_utf8(err).unwrap() }
}

fn ns(samples: i64, rate: i64) -> i64 {
    vtj::ticks_to_ns(samples as i128, vtj::Rational::new(rate, 1)).unwrap()
}

const LAME: &str = "mp3_cbr_lame.mp3";
const PLAIN: &str = "mp3_plain.mp3";
const MPEG2_MONO: &str = "mp3_vbr_mono_mpeg2.mp3";

#[test]
fn gapless_off_ignores_the_lame_tag() {
    let o = run(&["--gapless", "off"], &media(LAME));
    assert_eq!(o.code, 0);
    assert_eq!(o.codec_delay(), None);
    assert!(!o.text.contains("discard_padding_ns"));
    assert!(o.units()[0].contains(r#""pts_ns":0,"#));
    assert!(o.text.contains(r#""params":{"gapless":"off"}"#));
}

#[test]
fn encoder_delay_and_padding_overrides() {
    let o = run(&["--encoder-delay", "576"], &media(PLAIN));
    assert_eq!(o.codec_delay(), Some(ns(576 + 529, 48000)));
    assert!(!o.text.contains("discard_padding_ns"), "no padding given");
    assert!(o.text.contains(r#""params":{"encoder_delay":576}"#));

    let o = run(&["--encoder-padding", &(529 + 1152 + 10).to_string()], &media(LAME));
    assert_eq!(o.code, 0);
    let trimmed = o.units().iter().filter(|u| u.contains("discard_padding_ns")).count();
    assert_eq!(trimmed, 2, "1162 samples of padding span two frames");
    assert_eq!(o.audible_end(), ns(40 * 1152 - 1105 - 1162, 44100));

    let o = run(&["--decoder-delay", "528"], &media(LAME));
    assert_eq!(o.codec_delay(), Some(ns(576 + 528, 44100)));
}

/// `--encoder-padding` is documented up to 65535 samples, far more than the
/// 4095 a LAME tag can carry on its own, so a large override must be able to
/// spread its discard over as many trailing frames as it needs, not just a
/// fixed lookback window sized for the tag case. Covers both 1152 and 576
/// samples/frame, and confirms padding that truly exceeds the track still
/// fails (rather than silently accepting an impossible override).
#[test]
fn encoder_padding_beyond_a_handful_of_frames_still_fits_the_track() {
    // 40 frames * 1152 samples = 46080 total; 20000 samples of discard span
    // about 17 frames.
    let o = run(&["--encoder-padding", "20529"], &media(LAME));
    assert_eq!(o.code, 0, "{}", o.err);
    let trimmed = o.units().iter().filter(|u| u.contains("discard_padding_ns")).count();
    assert!(trimmed > 8, "expected more than 8 trimmed frames, got {trimmed}");
    assert_eq!(o.audible_end(), ns(40 * 1152 - 1105 - 20000, 44100));

    // 41 frames * 576 samples = 23616 total; 15000 samples span about 26 frames.
    let o = run(&["--encoder-padding", "15529"], &media(MPEG2_MONO));
    assert_eq!(o.code, 0, "{}", o.err);
    let trimmed = o.units().iter().filter(|u| u.contains("discard_padding_ns")).count();
    assert!(trimmed > 8, "expected more than 8 trimmed frames, got {trimmed}");

    // Padding that truly exceeds the whole track must still fail, not be
    // silently accepted now that the lookback window is no longer fixed.
    let o = run(&["--encoder-padding", "65535"], &media(LAME));
    assert_eq!(o.code, cli::EXIT_PARSE_ERROR);
    assert!(
        o.text.contains(
            r#""code":"UNREPRESENTABLE_IN_VMKV","message":"an encoder padding of 65535 samples exceeds the stream""#
        ),
        "{}",
        o.text
    );
}

fn with_broken_lame_crc() -> PathBuf {
    let mut b = std::fs::read(media(LAME)).unwrap();
    let crc_at = 61 + 156 + 34;
    b[crc_at] ^= 0xff;
    assert_ne!(crc16(&b[61..crc_at]), u16::from_be_bytes([b[crc_at], b[crc_at + 1]]));
    temp("badcrc.mp3", &b)
}

#[test]
fn lame_crc_policy() {
    let f = with_broken_lame_crc();
    assert_eq!(run(&[], &f).codec_delay(), None, "a tag with a wrong CRC is not trusted by default");
    let o = run(&["--lame-crc", "ignore"], &f);
    assert_eq!(o.codec_delay(), Some(ns(1105, 44100)));
    assert_eq!(o.audible_end(), 1_000_000_000);
}

#[test]
fn xing_count_mismatch_policies() {
    let full = std::fs::read(media(LAME)).unwrap();
    let f = temp("short.mp3", &full[..8419]);
    let d = run(&[], &f);
    assert_eq!((d.codec_delay(), d.text.contains("discard_padding_ns")), (Some(ns(1105, 44100)), false));
    let u = run(&["--xing-count-mismatch", "use-padding"], &f);
    assert_eq!((u.codec_delay(), u.text.contains("discard_padding_ns")), (Some(ns(1105, 44100)), true));
    let i = run(&["--xing-count-mismatch", "ignore-tag"], &f);
    assert_eq!((i.codec_delay(), i.text.contains("discard_padding_ns")), (None, false));
}

#[test]
fn info_frame_keep_adds_it_to_the_delay() {
    let o = run(&["--info-frame", "keep"], &media(LAME));
    assert_eq!(o.code, 0);
    let u = o.units();
    assert_eq!(u.len(), 41);
    assert!(u[0].contains(r#"["src",0,61,208]"#), "{}", u[0]);
    assert_eq!(o.codec_delay(), Some(ns(1105 + 1152, 44100)));
    assert_eq!(o.audible_end(), 1_000_000_000, "the audible part does not move");
}

#[test]
fn junk_resync() {
    let mut b = std::fs::read(media(PLAIN)).unwrap();
    b.splice(384..384, [0x12u8, 0xff, 0xfb, 0x34]);
    b.extend(b"TRAILING-GARBAGE");
    let f = temp("junk.mp3", &b);
    assert_eq!(run(&[], &f).code, cli::EXIT_PARSE_ERROR);
    let o = run(&["--junk", "resync"], &f);
    assert_eq!(o.code, 0, "{}", o.text);
    assert_eq!(o.units().len(), 43, "every frame kept, junk skipped");
    assert!(o.units()[1].contains(r#"["src",0,388,384]"#), "{}", o.units()[1]);
    assert!(o.text.contains(r#""params":{"junk":"resync"}"#));
}

#[test]
fn resync_does_not_hide_a_real_parameter_change() {
    let mut b = std::fs::read(media(PLAIN)).unwrap();
    let other = std::fs::read(media("mp3_vbr_mono_mpeg2.mp3")).unwrap();
    b.extend(&other[227..]);
    let o = run(&["--junk", "resync"], &temp("mixed.mp3", &b));
    assert!(o.text.contains(r#""code":"INCONSISTENT_TRACK_PARAMETERS""#), "{}", o.text);
}

#[test]
fn zero_padding_and_incomplete_end_policies() {
    let plain = std::fs::read(media(PLAIN)).unwrap();
    let mut z = vec![0u8; 100];
    z.extend(&plain);
    let f = temp("zeros.mp3", &z);
    assert_eq!(run(&[], &f).code, 0);
    let o = run(&["--zero-padding", "error"], &f);
    assert!(o.text.contains(r#""message":"no frame sync at byte 0""#), "{}", o.text);

    let cut = temp("cut.mp3", &plain[..plain.len() - 100]);
    assert_eq!(run(&[], &cut).code, cli::EXIT_PARSE_ERROR);
    let o = run(&["--incomplete-end", "drop"], &cut);
    assert_eq!(o.code, 0);
    assert_eq!(o.units().len(), 42);
}

#[test]
fn byte_range() {
    let o = run(&["--byte-range", "384:"], &media(PLAIN));
    assert_eq!(o.units().len(), 42);
    assert!(o.units()[0].contains(r#"["src",0,384,384]"#));
    assert_eq!(run(&["--byte-range", "0:768"], &media(PLAIN)).units().len(), 2);
    let o = run(&["--byte-range", "0:99999999"], &media(PLAIN));
    assert!(
        o.text.contains(r#""code":"TRUNCATED_BITSTREAM","message":"--byte-range end 99999999 is past the end"#),
        "{}",
        o.text
    );
    assert!(o.text.contains(r#""params":{"byte_range":"0:99999999"}"#));
}

#[test]
fn usage_errors_and_conflicts() {
    for (args, needle) in [
        (&["--gapless", "maybe"][..], "not one of auto, off"),
        (&["--gapless", "off", "--encoder-delay", "5"], "--gapless off conflicts with --encoder-delay"),
        (&["--gapless", "off", "--lame-crc", "ignore"], "conflicts with --lame-crc"),
        (&["--encoder-delay", "70000"], "outside 0..=65535"),
        (&["--byte-range", "5:3"], "end must be greater than start"),
        (&["--byte-range", "x"], "is not A:B or A:"),
        (&["--byte-range", "0:", "--zero-padding", "error"], "--byte-range conflicts with --zero-padding"),
    ] {
        let o = run(args, &media(PLAIN));
        assert_eq!(o.code, cli::EXIT_USAGE, "{args:?}");
        assert!(o.err.contains(needle), "{args:?}: {}", o.err);
        assert!(o.text.is_empty());
    }
}

#[test]
fn help_documents_every_parameter() {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(cli::run(&Mp3, &["--help".into()], &mut out, &mut err), 0);
    let help = String::from_utf8(out).unwrap();
    for opt in [
        "--gapless <auto|off>",
        "--lame-crc <verify|ignore>",
        "--encoder-delay <N>",
        "--encoder-padding <N>",
        "--decoder-delay <N>",
        "--xing-count-mismatch <keep-delay|use-padding|ignore-tag>",
        "--info-frame <skip|keep>",
        "--junk <error|resync>",
        "--zero-padding <skip|error>",
        "--incomplete-end <error|drop>",
        "--byte-range <VALUE>",
    ] {
        assert!(help.contains(opt), "{opt} missing:\n{help}");
    }
    assert!(help.contains("(default: 529)"));
}
