//! Robustness: the parser binary, run on thousands of damaged variants of
//! the fixtures (the hybrid pair's .wv and .wvc in turn), never crashes,
//! hangs or breaks the output contract.

use std::path::{Path, PathBuf};

#[test]
fn damaged_inputs_never_break_the_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/media");
    let jobs = std::thread::available_parallelism().map_or(2, |n| n.get());
    let bin = Path::new(env!("CARGO_BIN_EXE_vmkv-parser-wavpack"));
    let suite = vtj_stress::ci_suite();
    let (wv, wvc) = (root.join("wavpack_hybrid.wv"), root.join("wavpack_hybrid.wvc"));
    let singles: Vec<PathBuf> = ["wavpack_stereo.wv", "wavpack_51.wv"].iter().map(|f| root.join(f)).collect();
    let refs: Vec<&Path> = singles.iter().map(PathBuf::as_path).collect();
    let (mut problems, mut runs) = vtj_stress::check(bin, &refs, &suite, jobs);
    let (p, r) = vtj_stress::check_with_args(bin, &[], &[wvc.to_str().unwrap()], &[&wv], &suite, jobs);
    problems.extend(p);
    runs += r;
    let (p, r) = vtj_stress::check_with_args(bin, &[wv.to_str().unwrap()], &[], &[&wvc], &suite, jobs);
    problems.extend(p);
    runs += r;
    assert!(runs > 1000, "only {runs} runs");
    assert!(problems.is_empty(), "{} problems in {runs} runs:\n{}", problems.len(), problems.join("\n"));
}
