//! Golden files for real converter output and hand-written scripts, plus
//! synthetic files for the failure cases.
//! Set `UPDATE_GOLDEN=1` to rewrite the golden files after an intended change.

use std::path::{Path, PathBuf};

use vmkv_parser_ass::Ass;
use vtj::cli;
use vtj::validate::{validate, Options, Outcome};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn run(input: &Path) -> (i32, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = cli::run(&Ass, &[input.as_os_str().to_os_string()], &mut out, &mut err);
    (code, String::from_utf8(out).unwrap())
}

fn temp(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vmkv-ass-{}", std::process::id()));
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
    for name in ["ass_sample.ass", "ass_features.ass", "ssa_sample.ssa"] {
        let input = root().join("testdata/media").join(name);
        let (code, out) = run(&input);
        assert_eq!(code, 0, "{out}");
        assert_eq!(outcome(&out), Outcome::Success);
        let stem = name.split('.').next().unwrap();
        let golden = root().join(format!("testdata/golden/ass/{stem}.vtj"));
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::create_dir_all(golden.parent().unwrap()).unwrap();
            std::fs::write(&golden, &out).unwrap();
        }
        assert_eq!(out, std::fs::read_to_string(&golden).unwrap(), "{name}");
        assert_eq!(run(&input).1, out, "rule 8");
        assert!(units(&out).iter().all(|u| u.contains(r#""flags":["random_access","duration_required"]"#)));
    }

    let (_, out) = run(&root().join("testdata/media/ass_features.ass"));
    assert!(out.contains(r#""codec_id":"S_TEXT/ASS""#));
    let u = units(&out);
    assert_eq!(u.len(), 3, "the Comment event is not a unit");
    // Format order Start, End, Layer, ...: Layer to Text are contiguous in
    // the source, so after ReadOrder the payload is one span.
    assert!(u[1].contains(r#""payload":[["inline","MSw="],["src",0,820,73]]"#), "{}", u[1]);
    // Comment event, [Fonts] and line endings stay in codec_private.
    assert!(out.contains(r#""codec_private":[["src",0,0,716],["src",0,947,42]]"#), "{out}");

    let (_, out) = run(&root().join("testdata/media/ssa_sample.ssa"));
    assert!(out.contains(r#""codec_id":"S_TEXT/SSA""#));
    // SSA has no Layer: it stays empty after ReadOrder.
    assert!(units(&out)[0].contains(r#""payload":[["inline","MCws"],"#), "{out}");
}

#[test]
fn transcoded_sources_are_inline() {
    let mut s = b"[Script Info]\nScriptType: v4.00+\n\n[V4+ Styles]\nFormat: Name\nStyle: D\n\n[Events]\n".to_vec();
    s.extend(b"Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n");
    s.extend(b"Dialogue: 0,0:00:01.00,0:00:02.00,D,,0,0,0,,caf\xe9\n");
    let (code, out) = run(&temp("cp1252.ass", &s));
    assert_eq!(code, 0, "{out}");
    // "0,0,D,,0,0,0,,café" in one inline chunk.
    assert!(units(&out)[0].contains(r#""payload":[["inline","MCwwLEQsLDAsMCwwLCxjYWbDqQ=="]]"#), "{out}");
    assert!(out.contains(r#""codec_private":[["inline","#), "{out}");
}

#[test]
fn rejected() {
    let head = "[Script Info]\n\n[V4+ Styles]\nFormat: Name\nStyle: D\n\n[Events]\n";
    let fmt = "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";
    let cases = [
        (
            "nostyles.ass",
            format!("[Script Info]\n\n[Events]\n{fmt}"),
            r#""message":"no [V4+ Styles] or [V4 Styles] section""#,
        ),
        (
            "noformat.ass",
            format!("{head}Dialogue: 0,0:00:01.00,0:00:02.00,D,,0,0,0,,x\n"),
            r#""message":"Dialogue line at byte 60 before the [Events] Format line""#,
        ),
        (
            "back.ass",
            format!("{head}{fmt}Dialogue: 0,0:00:03.00,0:00:02.00,D,,0,0,0,,x\n"),
            r#""message":"dialogue 0 at byte 140 ends before it starts""#,
        ),
        (
            "time.ass",
            format!("{head}{fmt}Dialogue: 0,0:00:01.000,0:00:02.00,D,,0,0,0,,x\n"),
            r#""message":"dialogue 0 at byte 140 has an invalid start time""#,
        ),
        (
            "short.ass",
            format!("{head}{fmt}Dialogue: 0,0:00:01.00,0:00:02.00,D\n"),
            r#""message":"Dialogue line at byte 140 has fewer than the 10 fields of its format""#,
        ),
        (
            "nostart.ass",
            format!("{head}Format: Layer, End, Text\nDialogue: 0,0:00:02.00,x\n"),
            r#""message":"the [Events] format has no start field""#,
        ),
    ];
    for (name, text, msg) in cases {
        let (code, out) = run(&temp(name, text.as_bytes()));
        assert_eq!(code, cli::EXIT_PARSE_ERROR, "{name}: {out}");
        assert_eq!(outcome(&out), Outcome::Failure);
        assert!(error(&out).contains(msg), "{name}: {out}");
    }
}
