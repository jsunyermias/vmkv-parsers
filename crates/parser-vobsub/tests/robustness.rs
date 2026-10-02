//! Robustness: the parser binary, run on thousands of damaged variants of
//! the fixture pair (the `.idx` and the `.sub` in turn, the other intact),
//! never crashes, hangs or breaks the output contract.

use std::path::{Path, PathBuf};

#[test]
fn damaged_inputs_never_break_the_contract() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/media");
    let (idx, sub) = (root.join("vobsub_sample.idx"), root.join("vobsub_sample.sub"));
    let jobs = std::thread::available_parallelism().map_or(2, |n| n.get());
    let bin = Path::new(env!("CARGO_BIN_EXE_vmkv-parser-vobsub"));
    let suite = vtj_stress::ci_suite();
    let idx_s = idx.to_str().unwrap();
    let sub_s = sub.to_str().unwrap();
    let mut all = Vec::new();
    let mut total = 0;
    for stream in ["0", "1"] {
        let (p, runs) = vtj_stress::check_with_args(bin, &["--stream-index", stream], &[sub_s], &[&idx], &suite, jobs);
        all.extend(p);
        total += runs;
        let (p, runs) =
            vtj_stress::check_with_args(bin, &["--stream-index", stream, idx_s], &[], &[&sub], &suite, jobs);
        all.extend(p);
        total += runs;
    }
    assert!(total > 1000, "only {total} runs");
    assert!(all.is_empty(), "{} problems in {total} runs:\n{}", all.len(), all.join("\n"));
}
