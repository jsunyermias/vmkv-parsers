//! The common parser contract, exercised in process with a test-only parser
//! that cuts its input into fixed-size frames.

use vtj::cli::{self, ParamKind, ParamSpec, FRAME_RATE};
use vtj::validate::{validate, Options, Outcome};
use vtj::*;

struct FixedFrames;

const FRAME_SIZE: ParamSpec = ParamSpec { name: "frame_size", kind: ParamKind::Int, help: "bytes per frame" };

impl Parser for FixedFrames {
    fn name(&self) -> &'static str {
        "fixed-frames"
    }
    fn version(&self) -> &'static str {
        "0.0.1"
    }
    fn params(&self) -> &'static [ParamSpec] {
        &[FRAME_RATE, FRAME_SIZE]
    }
    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let rate = ctx.param_rational("frame_rate").ok_or_else(|| {
            ParseError::new(ErrorCode::TimingRequired, "the stream carries no timing; pass --frame-rate")
        })?;
        let size = ctx.param_int("frame_size").unwrap_or(4) as u64;
        let total = ctx.source(0).size();
        let mut tl = Timeline::new(rate, 0)?;
        let mut off = 0;
        while off < total {
            if total - off < size {
                return Err(ParseError::truncated(format!("frame {} cut at byte {total}", ctx.units_emitted())));
            }
            let (pts, dur) = tl.advance(1)?;
            ctx.emit(&Unit::new(pts, dur, Flags::NONE.with(Flag::RandomAccess), vec![Chunk::src(0, off, size)]))?;
            off += size;
        }
        let mut t = Track::new(TrackType::Video, "V_FFV1");
        t.video = Some(Video::new(16, 16));
        Ok(t)
    }
}

struct Buggy;

impl Parser for Buggy {
    fn name(&self) -> &'static str {
        "buggy"
    }
    fn version(&self) -> &'static str {
        "0"
    }
    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let size = ctx.source(0).size();
        ctx.emit(&Unit::new(0, -1, Flags::NONE, vec![Chunk::src(0, 0, size + 1)]))?;
        unreachable!("the writer rejects the unit")
    }
}

struct Run {
    code: i32,
    out: Vec<u8>,
    err: String,
}

fn run(p: &dyn Parser, args: &[&str]) -> Run {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cli::run(p, &args, &mut out, &mut err);
    Run { code, out, err: String::from_utf8(err).unwrap() }
}

fn input(name: &str, bytes: &[u8]) -> String {
    let dir = std::env::temp_dir().join(format!("vtj-contract-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    p.to_string_lossy().into_owned()
}

fn outcome(bytes: &[u8]) -> Outcome {
    let r = validate(bytes, &Options { codec_aware: true, max_problems: 0 });
    assert!(r.problems.is_empty() || r.outcome == Outcome::Invalid, "{:?}", r.problems);
    if r.outcome == Outcome::Invalid {
        panic!("invalid output: {:?}\n{}", r.problems, String::from_utf8_lossy(bytes));
    }
    r.outcome
}

#[test]
fn deterministic_output_with_params() {
    let src = input("ok.bin", &[7u8; 12]);
    let a = run(&FixedFrames, &["--frame-rate", "24000/1001", &src]);
    let b = run(&FixedFrames, &["--frame-rate=24000/1001", &src]);
    assert_eq!(a.code, 0, "{}", a.err);
    assert_eq!(a.out, b.out, "rule 8: same sources and params give identical bytes");
    assert_eq!(outcome(&a.out), Outcome::Success);
    let text = String::from_utf8(a.out).unwrap();
    let first = text.lines().next().unwrap();
    assert!(first.contains(r#""params":{"frame_rate":[24000,1001]}"#), "{first}");
    assert!(first.contains(r#""sources":[{"id":0,"size":12,"sha256":""#), "{first}");
    assert!(!first.contains("path"), "paths would break determinism: {first}");
    assert_eq!(text.lines().filter(|l| l.contains(r#""type":"unit""#)).count(), 3);
    assert!(text.ends_with("{\"type\":\"end\",\"unit_count\":3}\n"));

    let c = run(&FixedFrames, &["--frame-size", "6", "--frame-rate", "25", &src]);
    let text = String::from_utf8(c.out).unwrap();
    assert!(text.contains(r#""params":{"frame_rate":[25,1],"frame_size":6}"#), "{text}");
}

#[test]
fn timing_required() {
    let src = input("t.bin", &[0u8; 8]);
    let r = run(&FixedFrames, &[&src]);
    assert_eq!(r.code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&r.out), Outcome::Failure);
    let text = String::from_utf8(r.out).unwrap();
    assert!(text.ends_with("{\"type\":\"error\",\"code\":\"TIMING_REQUIRED\",\"message\":\"the stream carries no timing; pass --frame-rate\"}\n"));
}

#[test]
fn truncated_keeps_prior_units() {
    let src = input("trunc.bin", &[0u8; 10]);
    let r = run(&FixedFrames, &["--frame-rate", "25/1", &src]);
    assert_eq!(r.code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&r.out), Outcome::Failure);
    let text = String::from_utf8(r.out).unwrap();
    assert_eq!(text.lines().count(), 4, "header, 2 units, error");
    assert!(text
        .ends_with("{\"type\":\"error\",\"code\":\"TRUNCATED_BITSTREAM\",\"message\":\"frame 2 cut at byte 10\"}\n"));
}

#[test]
fn unreadable_source_gives_headerless_failure() {
    let r = run(&FixedFrames, &["--frame-rate", "25", "/nonexistent/vtj/input.bin"]);
    assert_eq!(r.code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&r.out), Outcome::Failure);
    let text = String::from_utf8(r.out).unwrap();
    assert_eq!(text, "{\"type\":\"error\",\"code\":\"SOURCE_UNREADABLE\",\"message\":\"source 0 cannot be read\"}\n");
    assert!(r.err.contains("/nonexistent/vtj/input.bin"), "details go to stderr only");
}

#[test]
fn contract_violation_becomes_error_line() {
    let src = input("bug.bin", &[0u8; 3]);
    let r = run(&Buggy, &[&src]);
    assert_eq!(r.code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&r.out), Outcome::Failure);
    let text = String::from_utf8(r.out).unwrap();
    assert!(
        text.lines().last().unwrap().contains("internal: format contract") || text.contains("internal: unit 0"),
        "{text}"
    );
}

#[test]
fn usage_errors_write_nothing() {
    let src = input("u.bin", &[0u8; 4]);
    for args in [
        vec![],
        vec!["--bogus", src.as_str()],
        vec!["--frame-rate", "0/1", src.as_str()],
        vec!["--frame-rate", "1/2", src.as_str(), src.as_str()],
        vec!["--frame-rate"],
    ] {
        let r = run(&FixedFrames, &args);
        assert_eq!(r.code, cli::EXIT_USAGE, "{args:?}");
        assert!(r.out.is_empty(), "{args:?}");
        assert!(r.err.contains("Usage:"), "{args:?}");
    }
    let dup = run(&FixedFrames, &["--frame-rate", "1", "--frame-rate", "2", &src]);
    assert_eq!(dup.code, cli::EXIT_USAGE);
}

#[test]
fn help_version_and_output_file() {
    let h = run(&FixedFrames, &["--help"]);
    assert_eq!(h.code, 0);
    let help = String::from_utf8(h.out).unwrap();
    assert!(help.contains("--frame-rate <VALUE>") && help.contains("--frame-size <VALUE>"), "{help}");
    let v = run(&FixedFrames, &["-V"]);
    assert_eq!(String::from_utf8(v.out).unwrap(), "fixed-frames 0.0.1\n");

    let src = input("o.bin", &[1u8; 8]);
    let out = input("o.vtj", b"");
    let r = run(&FixedFrames, &["-o", &out, "--frame-rate", "30", &src]);
    assert_eq!(r.code, 0);
    assert!(r.out.is_empty());
    assert_eq!(outcome(&std::fs::read(&out).unwrap()), Outcome::Success);
}
