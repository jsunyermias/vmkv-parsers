//! Golden file for real subtitle content and behavior tests for encoding
//! detection and malformed input. Set `UPDATE_GOLDEN=1` to rewrite the
//! golden file after an intended change.

use std::path::{Path, PathBuf};

use vmkv_parser_srt::Srt;
use vtj::cli;
use vtj::validate::{validate, Options, Outcome};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(input: &Path) -> (i32, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cli::run(&Srt, &[input.as_os_str().to_os_string()], &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-srt-{}", std::process::id()));
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

#[test]
fn golden_output() {
    let (code, out) = run(&root().join("testdata/media/srt_sample.srt"));
    assert_eq!(code, 0, "{out}");
    assert_eq!(outcome(&out), Outcome::Success);
    let golden = root().join("testdata/golden/srt/srt_sample.vtj");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden, &out).unwrap();
    }
    assert_eq!(out, std::fs::read_to_string(&golden).unwrap());
    assert_eq!(run(&root().join("testdata/media/srt_sample.srt")).1, out, "rule 8");
    assert_eq!(units(&out).len(), 4);
    assert!(units(&out)[2].contains(r#""duration_ns":0"#), "a cue may last zero ns: {}", units(&out)[2]);
    assert!(units(&out).iter().all(|u| u.contains(r#""flags":["random_access","duration_required"]"#)));
    assert!(
        units(&out)[1].contains(r#""payload":[["src",0,74,26]]"#),
        "a multi-line cue is one src span: {}",
        units(&out)[1]
    );
}

#[test]
fn multiline_text_keeps_the_internal_newline() {
    let b = b"1\n00:00:01,000 --> 00:00:02,000\nLine one\nLine two\n";
    let (code, out) = run(&temp("multiline.srt", b));
    assert_eq!(code, 0, "{out}");
    assert!(units(&out)[0].contains(r#""payload":[["src",0,32,17]]"#), "{}", units(&out)[0]);
    assert_eq!(&b[32..32 + 17], b"Line one\nLine two");
}

#[test]
fn crlf_line_endings_are_kept_verbatim_in_the_source_span() {
    let b = b"1\r\n00:00:01,000 --> 00:00:02,000\r\nCRLF text\r\n\r\n";
    let (code, out) = run(&temp("crlf.srt", b));
    assert_eq!(code, 0, "{out}");
    assert_eq!(outcome(&out), Outcome::Success);
    assert!(units(&out)[0].contains(r#""payload":[["src",0,34,9]]"#), "{}", units(&out)[0]);
}

#[test]
fn empty_cue_text_is_a_zero_length_span() {
    let b = b"1\n00:00:01,000 --> 00:00:02,000\n\n2\n00:00:03,000 --> 00:00:04,000\nlater\n";
    let (code, out) = run(&temp("empty-cue.srt", b));
    assert_eq!(code, 0, "{out}");
    assert!(
        units(&out)[0].contains(r#""payload":[["src",0,32,0]]"#),
        "a cue with no text is a zero-length span: {}",
        units(&out)[0]
    );
    assert_eq!(units(&out).len(), 2);
}

/// A separator line that is not strictly empty (spaces, tabs, or both) must
/// still close the cue, the same as `skip_blank_lines` treats it before the
/// next index. Before this fix the text-collecting loop only recognized a
/// zero-length line, so the whole next cue (index, timing and text) was
/// swallowed into the first cue's payload.
#[test]
fn a_separator_line_of_spaces_or_tabs_still_ends_the_cue() {
    for (name, b, first_offset) in [
        (
            "lf-spaces",
            &b"1\n00:00:01,000 --> 00:00:02,000\nfirst\n   \n2\n00:00:03,000 --> 00:00:04,000\nsecond\n"[..],
            32,
        ),
        (
            "lf-tabs",
            &b"1\n00:00:01,000 --> 00:00:02,000\nfirst\n\t\t\n2\n00:00:03,000 --> 00:00:04,000\nsecond\n"[..],
            32,
        ),
        (
            "crlf-spaces",
            &b"1\r\n00:00:01,000 --> 00:00:02,000\r\nfirst\r\n  \r\n2\r\n00:00:03,000 --> 00:00:04,000\r\nsecond\r\n"[..],
            34,
        ),
    ] {
        let (code, out) = run(&temp(&format!("{name}.srt"), b));
        assert_eq!(code, 0, "{name}: {out}");
        assert_eq!(outcome(&out), Outcome::Success, "{name}");
        assert_eq!(units(&out).len(), 2, "{name}: {out}");
        assert!(
            units(&out)[0].contains(&format!(r#""payload":[["src",0,{first_offset},5]]"#)),
            "{name}: {}",
            units(&out)[0]
        );
    }
}

#[test]
fn utf8_bom_is_stripped_and_offsets_still_reference_the_source() {
    let mut b = vec![0xef, 0xbb, 0xbf];
    b.extend_from_slice(b"1\n00:00:01,000 --> 00:00:02,000\nBOM\n");
    let (code, out) = run(&temp("bom.srt", &b));
    assert_eq!(code, 0, "{out}");
    assert_eq!(outcome(&out), Outcome::Success);
    // "BOM" starts 3 (BOM) + 1 (index+LF) + 31 (timing+LF) = 35 bytes in.
    assert!(units(&out)[0].contains(r#""payload":[["src",0,35,3]]"#), "{}", units(&out)[0]);
}

#[test]
fn non_utf8_source_is_transcoded_to_inline_windows1252() {
    let mut b = b"1\n00:00:01,000 --> 00:00:02,000\n".to_vec();
    b.extend_from_slice(b"caf\xe9"); // Windows-1252 "café"
    b.push(b'\n');
    let (code, out) = run(&temp("cp1252.srt", &b));
    assert_eq!(code, 0, "{out}");
    assert_eq!(outcome(&out), Outcome::Success);
    assert!(units(&out)[0].contains(r#""payload":[["inline","Y2Fmw6k="]]"#), "{}", units(&out)[0]);
}

#[test]
fn utf16le_bom_is_transcoded_to_inline() {
    let text = "1\r\n00:00:01,000 --> 00:00:02,000\r\nhola\r\n";
    let mut b = vec![0xff, 0xfe];
    for u in text.encode_utf16() {
        b.extend_from_slice(&u.to_le_bytes());
    }
    let (code, out) = run(&temp("utf16le.srt", &b));
    assert_eq!(code, 0, "{out}");
    assert_eq!(outcome(&out), Outcome::Success);
    assert!(units(&out)[0].contains(r#""payload":[["inline","aG9sYQ=="]]"#), "{}", units(&out)[0]);
}

#[test]
fn malformed_and_missing_lines() {
    let (code, out) = run(&temp("no-index.srt", b"00:00:01,000 --> 00:00:02,000\ntext\n"));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert!(out.ends_with("\"code\":\"INVALID_BITSTREAM\",\"message\":\"expected a cue index at byte 0\"}\n"), "{out}");

    let (code, out) = run(&temp("period.srt", b"1\n00:00:01.000 --> 00:00:02.000\ntext\n"));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert!(out.contains(r#""code":"INVALID_BITSTREAM","message":"malformed timing at byte 2""#), "{out}");

    let (code, out) = run(&temp("inverted.srt", b"1\n00:00:05,000 --> 00:00:02,000\ntext\n"));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert!(
        out.ends_with("\"code\":\"INVALID_BITSTREAM\",\"message\":\"cue at byte 0 ends before it starts\"}\n"),
        "{out}"
    );

    let (code, out) = run(&temp("cut.srt", b"1\n"));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert!(
        out.ends_with("\"code\":\"TRUNCATED_BITSTREAM\",\"message\":\"cue at byte 0 is missing its timing line\"}\n"),
        "{out}"
    );

    // No cues at all is an empty track (decision 68), with or without a BOM
    // or blank lines.
    for (name, bytes) in [("empty.srt", &b""[..]), ("blank.srt", &b"\xef\xbb\xbf\r\n\r\n"[..])] {
        let (code, out) = run(&temp(name, bytes));
        assert_eq!(code, 0, "{name}: {out}");
        assert_eq!(outcome(&out), Outcome::Success);
        assert!(out.ends_with("{\"type\":\"end\",\"unit_count\":0}\n"), "{name}: {out}");
    }
}

/// A syntactically valid but astronomically large hour field must not panic
/// the process (a multiply overflow previously aborted it with exit 101);
/// it must fail cleanly with a well-formed `error` line instead. Run as a
/// real subprocess, since an in-process panic would abort the test binary
/// itself rather than demonstrate the process-level exit code.
#[test]
fn overflowing_hour_field_fails_cleanly_not_panics() {
    let input = temp("overflow.srt", b"1\n153722867280912930:00:00,000 --> 153722867280912931:00:00,000\ntext\n");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_vmkv-parser-srt")).arg(&input).output().unwrap();
    assert!(output.status.code() != Some(101), "the process aborted instead of reporting an error");
    assert_eq!(output.status.code(), Some(cli::EXIT_PARSE_ERROR));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "{stderr}");
    let out = String::from_utf8(output.stdout).unwrap();
    assert!(out.ends_with("\"code\":\"INVALID_BITSTREAM\",\"message\":\"malformed timing at byte 2\"}\n"), "{out}");
}

fn run_args(args: &[&str], input: &Path) -> (i32, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut argv: Vec<std::ffi::OsString> = args.iter().map(std::ffi::OsString::from).collect();
    argv.push(input.as_os_str().to_os_string());
    let code = cli::run(&Srt, &argv, &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

/// Reduced from a real track (decision 67): a cue whose text starts with two
/// CRLF blank lines to raise it on screen, as stored in its MKV block and
/// written out as is when extracted to `.srt`, with LF between cues.
const RAISED: &[u8] = b"8\n00:00:54,930 --> 00:00:56,139\nLos pilotos la llaman:\n\n9\n00:04:01,158 --> 00:04:06,162\n\r\n\r\nOCEANO INDICO.\r\nEN LA ACTUALIDAD.\n\n10\n00:04:08,707 --> 00:04:10,249\n- Buenos dias.\n";

#[test]
fn blank_lines_not_followed_by_a_cue_belong_to_the_text() {
    let (code, out) = run(&temp("raised.srt", RAISED));
    assert_eq!(code, 0, "{out}");
    assert_eq!(outcome(&out), Outcome::Success);
    let u = units(&out);
    assert_eq!(u.len(), 3);
    // The blank lines are kept: the payload is the MKV block's own bytes,
    // "\r\n\r\nOCEANO INDICO.\r\nEN LA ACTUALIDAD.", one source span.
    assert!(u[1].contains(r#""payload":[["src",0,88,37]]"#), "{}", u[1]);
    assert!(!out.lines().next().unwrap().contains("params"), "the default is not recorded");
}

#[test]
fn strict_mode_ends_a_cue_at_any_blank_line() {
    let (code, out) = run_args(&["--blank-lines-in-cue", "strict"], &temp("raised-strict.srt", RAISED));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert_eq!(outcome(&out), Outcome::Failure);
    assert!(out.lines().next().unwrap().ends_with(r#""params":{"blank_lines_in_cue":"strict"}}"#), "{out}");
    assert!(out.lines().last().unwrap().contains(r#""message":"expected a cue index at byte 92""#), "{out}");

    // Explicit keep is recorded like any parameter passed.
    let (code, out) = run_args(&["--blank-lines-in-cue", "keep"], &temp("raised-keep.srt", RAISED));
    assert_eq!(code, 0, "{out}");
    assert!(out.lines().next().unwrap().ends_with(r#""params":{"blank_lines_in_cue":"keep"}}"#), "{out}");
}

#[test]
fn a_timing_line_without_index_after_blank_lines_is_still_an_error() {
    // The second cue lost its index: tolerating blank lines must not merge
    // it into the first cue's text.
    let b = b"1\n00:00:01,000 --> 00:00:02,000\nfirst\n\n00:00:03,000 --> 00:00:04,000\nsecond\n";
    let (code, out) = run(&temp("no-index.srt", b));
    assert_eq!(code, cli::EXIT_PARSE_ERROR);
    assert!(out.lines().last().unwrap().contains(r#""message":"timing line without a cue index at byte 39""#), "{out}");
}

#[test]
fn trailing_blank_lines_and_cues_after_them_still_split_normally() {
    let b = b"1\n00:00:01,000 --> 00:00:02,000\nfirst\n\n\n\n2\n00:00:03,000 --> 00:00:04,000\nsecond\n\n\n";
    let (code, out) = run(&temp("trailing.srt", b));
    assert_eq!(code, 0, "{out}");
    let u = units(&out);
    assert_eq!(u.len(), 2);
    assert!(u[0].contains(r#""payload":[["src",0,32,5]]"#), "{}", u[0]);
    assert!(u[1].contains(r#""payload":[["src",0,73,6]]"#), "{}", u[1]);
}
