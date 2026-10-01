//! Robustness: the parser binary, run on thousands of damaged variants of
//! the fixture, never crashes, hangs or breaks the output contract.

use std::path::{Path, PathBuf};

#[test]
fn damaged_inputs_never_break_the_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/media");
    let inputs: Vec<PathBuf> = ["srt_sample.srt"].iter().map(|f| root.join(f)).collect();
    let refs: Vec<&Path> = inputs.iter().map(PathBuf::as_path).collect();
    let jobs = std::thread::available_parallelism().map_or(2, |n| n.get());
    let bin = Path::new(env!("CARGO_BIN_EXE_vmkv-parser-srt"));
    let (problems, runs) = vtj_stress::check(bin, &refs, &vtj_stress::ci_suite(), jobs);
    assert!(runs > 1000, "only {runs} runs");
    assert!(problems.is_empty(), "{} problems in {runs} runs:\n{}", problems.len(), problems.join("\n"));
}
