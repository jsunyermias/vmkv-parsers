use std::path::PathBuf;
use std::process::Command;

fn golden(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/golden").join(name).to_string_lossy().into_owned()
}

fn run(args: &[&str]) -> (i32, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_vtj-validate")).args(args).output().unwrap();
    (o.status.code().unwrap(), String::from_utf8(o.stdout).unwrap())
}

#[test]
fn golden_success() {
    for f in ["mp3.vtj", "aac_adts.vtj", "h264_annexb.vtj", "ogg_opus.vtj", "srt.vtj"] {
        let (code, out) = run(&["--codec-aware", &golden(f)]);
        assert_eq!(code, 0, "{f}: {out}");
        assert!(out.starts_with("valid: "), "{out}");
    }
}

#[test]
fn failure_outputs_exit_3() {
    assert_eq!(run(&[&golden("failure.vtj")]).0, 3);
    assert_eq!(run(&["-q", &golden("failure_no_header.vtj")]), (3, String::new()));
}

#[test]
fn source_verification() {
    let ok = format!("0={}", golden("srt_source.srt"));
    assert_eq!(run(&["--source", &ok, &golden("srt.vtj")]).0, 0);
    let wrong = format!("0={}", golden("mp3.vtj"));
    let (code, out) = run(&["--source", &wrong, &golden("srt.vtj")]);
    assert_eq!(code, 1);
    assert!(out.contains("size 83 does not match"), "{out}");
    assert!(out.contains("sha256 does not match"), "{out}");
    let (code, out) = run(&["--source", &format!("5={}", golden("srt_source.srt")), &golden("srt.vtj")]);
    assert_eq!(code, 1);
    assert!(out.contains("no source with that id"), "{out}");
}

#[test]
fn invalid_and_usage() {
    let dir = std::env::temp_dir().join(format!("vtj-validate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bad = dir.join("bad.vtj");
    std::fs::write(&bad, "{\"type\":\"end\",\"unit_count\":0}\n").unwrap();
    let (code, out) = run(&[bad.to_str().unwrap()]);
    assert_eq!(code, 1);
    assert!(out.contains("line 1: end must directly follow track"), "{out}");
    assert_eq!(run(&[]).0, 2);
    assert_eq!(run(&["--nope", "x"]).0, 2);
    assert_eq!(run(&["/nonexistent.vtj"]).0, 2);
}
