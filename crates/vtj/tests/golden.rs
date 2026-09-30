//! Golden tests: the specification's examples, built through the typed API and
//! the timing helpers, must serialize byte for byte to the files in
//! `testdata/golden`, and those files must pass the validator.

use std::collections::BTreeMap;
use std::path::PathBuf;

use vtj::validate::{validate, Options, Outcome};
use vtj::*;

fn golden(name: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/golden").join(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn header(parser: &str, size: u64, sha256: Option<&str>) -> Header {
    Header {
        parser: ParserInfo { name: parser.into(), version: "0.1.0".into() },
        sources: vec![Source { id: 0, size, sha256: sha256.map(Into::into), path: None }],
        params: BTreeMap::new(),
    }
}

fn write(h: &Header, units: &[Unit], t: &Track) -> Vec<u8> {
    let mut w = VtjWriter::new(Vec::new());
    w.header(h).unwrap();
    for u in units {
        w.unit(u).unwrap();
    }
    w.finish(t).unwrap();
    w.into_inner()
}

fn ra() -> Flags {
    Flags::NONE.with(Flag::RandomAccess)
}

fn assert_golden(name: &str, bytes: Vec<u8>) {
    let expected = golden(name);
    assert_eq!(String::from_utf8(bytes).unwrap(), String::from_utf8(expected).unwrap(), "{name}");
}

#[test]
fn mp3() {
    let mut tl = Timeline::new(Rational::new(44100, 1), 0).unwrap();
    let frames = [(2048, 418), (2466, 417), (2883, 418)];
    let units: Vec<Unit> = frames
        .iter()
        .map(|&(off, len)| {
            let (pts, dur) = tl.advance(1152).unwrap();
            Unit::new(pts, dur, ra(), vec![Chunk::src(0, off, len)])
        })
        .collect();
    let mut t = Track::new(TrackType::Audio, "A_MPEG/L3");
    t.audio = Some(Audio::new(Rational::new(44100, 1), 2));
    let sha = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    assert_golden("mp3.vtj", write(&header("mp3-parser", 5_234_123, Some(sha)), &units, &t));
}

#[test]
fn aac_adts() {
    let mut tl = Timeline::new(Rational::new(44100, 1), 0).unwrap();
    let units: Vec<Unit> = [(7, 364), (378, 373)]
        .iter()
        .map(|&(off, len)| {
            let (pts, dur) = tl.advance(1024).unwrap();
            Unit::new(pts, dur, ra(), vec![Chunk::src(0, off, len)])
        })
        .collect();
    let mut t = Track::new(TrackType::Audio, "A_AAC");
    t.codec_private = Some(vec![Chunk::inline([0x12, 0x10])]);
    t.audio = Some(Audio::new(Rational::new(44100, 1), 2));
    assert_golden("aac_adts.vtj", write(&header("aac-adts-parser", 751, None), &units, &t));
}

#[test]
fn h264_annexb() {
    let rate = Rational::new(24000, 1001);
    let pts: Vec<i64> = (0..2).map(|n| ticks_to_ns(n, rate).unwrap()).collect();
    let dur = durations_from_pts(&pts, Some(ticks_to_ns(2, rate).unwrap())).unwrap();
    let frames = [(41u64, 4340u32, ra()), (4385, 1200, Flags::NONE)];
    let units: Vec<Unit> = frames
        .iter()
        .enumerate()
        .map(|(i, &(off, len, flags))| {
            Unit::new(pts[i], dur[i], flags, vec![Chunk::inline(len.to_be_bytes()), Chunk::src(0, off, len as u64)])
        })
        .collect();
    let mut t = Track::new(TrackType::Video, "V_MPEG4/ISO/AVC");
    t.codec_private = Some(vec![
        Chunk::inline([0x01, 0x4d, 0x00, 0x28, 0xff, 0xe1, 0x00, 0x19]),
        Chunk::src(0, 4, 25),
        Chunk::inline([0x01, 0x00, 0x04]),
        Chunk::src(0, 33, 4),
    ]);
    let mut v = Video::new(1920, 1080);
    v.interlace = Some(Interlace::Progressive);
    v.nominal_frame_rate = Some(rate);
    t.video = Some(v);
    assert_golden("h264_annexb.vtj", write(&header("h264-parser", 5585, None), &units, &t));
}

#[test]
fn ogg_opus() {
    let rate = Rational::new(48000, 1);
    let pre_skip = 312;
    let mut tl = Timeline::new(rate, -pre_skip).unwrap();
    let units: Vec<Unit> = [(3895, 120), (4015, 118)]
        .iter()
        .map(|&(off, len)| {
            let (pts, dur) = tl.advance(960).unwrap();
            Unit::new(pts, dur, ra(), vec![Chunk::src(0, off, len)])
        })
        .collect();
    let mut t = Track::new(TrackType::Audio, "A_OPUS");
    t.codec_private = Some(vec![Chunk::src(0, 28, 19)]);
    t.codec_delay_ns = Some(ticks_to_ns(pre_skip, rate).unwrap());
    t.seek_preroll_ns = Some(80_000_000);
    t.audio = Some(Audio::new(rate, 2));
    assert_golden("ogg_opus.vtj", write(&header("ogg-opus-parser", 4133, None), &units, &t));
}

#[test]
fn srt() {
    let ms = Rational::new(1000, 1);
    let cues = [(1000, 3500, 32, 5), (5000, 7250, 71, 11)];
    let flags = ra().with(Flag::DurationRequired);
    let units: Vec<Unit> = cues
        .iter()
        .map(|&(start, end, off, len)| {
            let pts = ticks_to_ns(start, ms).unwrap();
            Unit::new(pts, ticks_to_ns(end, ms).unwrap() - pts, flags, vec![Chunk::src(0, off, len)])
        })
        .collect();
    let t = Track::new(TrackType::Subtitle, "S_TEXT/UTF8");
    let sha = "4094a2ba8df504a02193a629d295958a6790c321d175d4e93fb3a4bb734b2b0d";
    let src_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/golden/srt_source.srt");
    assert_eq!(vtj::source::SourceFile::open(0, src_path).unwrap().sha256(), sha);
    let src = golden("srt_source.srt");
    assert_eq!(src.len(), 83);
    assert_eq!(&src[32..37], b"Hello");
    assert_eq!(&src[71..82], b"Hello world");
    assert_golden("srt.vtj", write(&header("srt-parser", 83, Some(sha)), &units, &t));
}

#[test]
fn golden_files_are_valid_and_roundtrip() {
    let opts = Options { codec_aware: true, max_problems: 0 };
    for name in ["mp3.vtj", "aac_adts.vtj", "h264_annexb.vtj", "ogg_opus.vtj", "srt.vtj"] {
        let r = validate(&golden(name), &opts);
        assert_eq!(r.outcome, Outcome::Success, "{name}: {:?}", r.problems);
    }
    for name in ["failure.vtj", "failure_no_header.vtj"] {
        let r = validate(&golden(name), &opts);
        assert_eq!(r.outcome, Outcome::Failure, "{name}: {:?}", r.problems);
    }
}

#[test]
fn spec_rounding_example() {
    assert_eq!(ticks_to_ns(3, Rational::new(24000, 1001)), Ok(125_125_000));
    assert_eq!(3 * 41_708_333, 125_124_999);
}
