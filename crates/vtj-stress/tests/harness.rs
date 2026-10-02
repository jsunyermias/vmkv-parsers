//! The harness must catch every kind of contract breach. Fake parsers are
//! shell scripts, so these tests run on Unix only.
//!
//! The tests run one at a time: a script written by one test while another
//! test forks could be inherited open for writing by the child, and running
//! it would then fail with ETXTBSY ("text file busy").
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use vtj_stress::{run_all, run_one, Exec, Mutation, Problem, RunConfig, Verdict};

const OK: &str = r#"{"type":"header","format":"vmkv-parser-output","version":1,"parser":{"name":"p","version":"1"},"sources":[{"id":0,"size":1}]}
{"type":"track","track_type":"audio","codec_id":"A_MPEG/L3","audio":{"sampling_frequency":[44100,1],"channels":2}}
{"type":"end","unit_count":0}"#;
const FAIL: &str = r#"{"type":"error","code":"TRUNCATED_BITSTREAM","message":"x"}"#;

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vtj-stress-test-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn script(name: &str, body: &str) -> RunConfig {
    let p = dir("bin").join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    RunConfig { bin: p, extra_args: vec![], trailing_args: vec![], timeout: Duration::from_secs(20), repeat: false }
}

fn verdict(cfg: &RunConfig) -> Verdict {
    run_one(cfg, b"x", "bin", &dir(&cfg.bin.file_name().unwrap().to_string_lossy()))
}

#[test]
fn accepts_contract_compliant_runs() {
    let _serial = serial();
    assert_eq!(verdict(&script("ok", &format!("cat <<'E'\n{OK}\nE\nexit 0"))), Verdict::Success { units: 0 });
    assert_eq!(
        verdict(&script("fail", &format!("cat <<'E'\n{FAIL}\nE\nexit 1"))),
        Verdict::Failure { code: "TRUNCATED_BITSTREAM".into() }
    );
}

/// Predicate on the problem the harness must report.
type Check = fn(&Problem) -> bool;

#[test]
fn catches_every_breach() {
    let _serial = serial();
    let cases: Vec<(&str, String, Check)> = vec![
        ("panic", "echo 'thread main panicked at x' >&2; exit 101".into(), |p| matches!(p, Problem::Crash { .. })),
        ("abort", "kill -ABRT $$".into(), |p| matches!(p, Problem::Crash { status, .. } if status.starts_with("signal"))),
        ("hang", "sleep 5".into(), |p| *p == Problem::Hang),
        ("usage", "echo bad >&2; exit 2".into(), |p| matches!(p, Problem::UnexpectedExit { code: 2, .. })),
        ("garbage", "echo not-json; exit 0".into(), |p| matches!(p, Problem::InvalidOutput { exit: 0, .. })),
        ("mismatch0", format!("cat <<'E'\n{FAIL}\nE\nexit 0"), |p| matches!(p, Problem::ExitMismatch { exit: 0, .. })),
        ("mismatch1", format!("cat <<'E'\n{OK}\nE\nexit 1"), |p| matches!(p, Problem::ExitMismatch { exit: 1, .. })),
        ("internal", "echo '{\"type\":\"error\",\"code\":\"UNREPRESENTABLE_IN_VMKV\",\"message\":\"internal: unit 3: payload[0]: offset 9 + length 9 exceeds size 1 of source 0\"}'; exit 1".into(), |p| matches!(p, Problem::Internal { .. })),
        ("partial", "echo '{\"type\":\"header\",\"format\":\"vmkv-parser-output\",\"version\":1,\"parser\":{\"name\":\"p\",\"version\":\"1\"},\"sources\":[{\"id\":0,\"size\":1}]}'; exit 1".into(), |p| matches!(p, Problem::InvalidOutput { exit: 1, .. })),
    ];
    for (name, body, check) in cases {
        let mut cfg = script(name, &body);
        if name == "hang" {
            cfg.timeout = Duration::from_millis(500);
        }
        match verdict(&cfg) {
            Verdict::Problem(p) => assert!(check(&p), "{name}: wrong problem {p:?}"),
            v => panic!("{name}: not caught, got {v:?}"),
        }
    }
}

#[test]
fn repeat_catches_non_determinism() {
    let _serial = serial();
    let counter = dir("nd").join("n");
    let _ = std::fs::remove_file(&counter);
    let body = format!(
        "if [ -f {c} ]; then ch=1; else ch=2; touch {c}; fi\ncat <<E\n{}\nE\nexit 0",
        OK.replace("\"channels\":2", "\"channels\":$ch"),
        c = counter.display()
    );
    let cfg = RunConfig { repeat: true, ..script("nd", &body) };
    assert_eq!(verdict(&cfg), Verdict::Problem(Problem::NonDeterministic));

    let stable = RunConfig { repeat: true, ..script("stable", &format!("cat <<'E'\n{OK}\nE\nexit 0")) };
    assert_eq!(verdict(&stable), Verdict::Success { units: 0 });
}

#[test]
fn run_all_is_parallel_and_fail_fast_stops() {
    let _serial = serial();
    let cfg = script("mixed", &format!("if grep -q X \"$1\"; then exit 101; fi\ncat <<'E'\n{OK}\nE\nexit 0"));
    let muts: Vec<Mutation> =
        (0..40).map(|i| if i == 5 { "set@0:0x58".parse().unwrap() } else { "truncate@1".parse().unwrap() }).collect();
    let all = run_all(&cfg, b"a", "bin", &muts, &Exec { jobs: 4, fail_fast: false, scratch: dir("all") }, &|_, _| {});
    let problems: Vec<_> = all.iter().filter(|v| matches!(v, Some(Verdict::Problem(_)))).collect();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(all.iter().all(Option::is_some));
    let ff = run_all(&cfg, b"a", "bin", &muts, &Exec { jobs: 1, fail_fast: true, scratch: dir("ff") }, &|_, _| {});
    assert!(ff.iter().filter(|v| v.is_none()).count() > 0, "fail-fast skips the rest");
}
