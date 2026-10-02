//! Robustness: the parser binary, run on thousands of damaged variants of
//! the fixtures, never crashes, hangs or breaks the output contract.

use std::path::{Path, PathBuf};

#[test]
fn damaged_inputs_never_break_the_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/media");
    let jobs = std::thread::available_parallelism().map_or(2, |n| n.get());
    let bin = Path::new(env!("CARGO_BIN_EXE_vmkv-parser-hevc"));
    let suite = vtj_stress::ci_suite();
    let (mut problems, mut runs) = vtj_stress::check(bin, &[&root.join("hevc_open_gop.hevc")], &suite, jobs);
    let (p, r) = vtj_stress::check_with_args(
        bin,
        &["--frame-rate", "24000/1001"],
        &[],
        &[&root.join("hevc_main10_closed.hevc")],
        &suite,
        jobs,
    );
    problems.extend(p);
    runs += r;
    assert!(runs > 1000, "only {runs} runs");
    assert!(problems.is_empty(), "{} problems in {runs} runs:\n{}", problems.len(), problems.join("\n"));
}
