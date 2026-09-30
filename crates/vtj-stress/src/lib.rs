//! Robustness harness for VMKV parsers.
//!
//! [`mutation`] describes how an input is damaged, [`select`] which damages
//! are generated for a file, and [`run`] executes the parser binary on each
//! variant and judges the result against the parser contract. The
//! `vtj-stress` binary exposes all of it on the command line; parser crates
//! use the library in their robustness tests.

pub mod mutation;
pub mod run;
pub mod select;

pub use mutation::{Fill, Mutation};
pub use run::{run_all, run_one, Exec, Problem, RunConfig, Verdict};
pub use select::{Offset, Positions, Selection};

use std::path::Path;
use std::time::Duration;

/// The selections run by each parser's robustness test in CI: dense
/// truncation near both ends (headers, tags, last frames), sparse truncation
/// elsewhere, single-bit flips, small structural edits and random edit
/// combinations. A few thousand runs per input.
pub fn ci_suite() -> Vec<Selection> {
    let base = Selection::default();
    let kinds = |k: &[&str]| k.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    vec![
        Selection { to: Offset::Start(512), ..base.clone() },
        Selection { from: Offset::End(512), ..base.clone() },
        Selection { positions: Positions::Step(7), ..base.clone() },
        Selection {
            kinds: kinds(&["flip"]),
            positions: Positions::Random(300),
            masks: (0..8).map(|b| 1u8 << b).collect(),
            seed: 1,
            ..base.clone()
        },
        Selection {
            kinds: kinds(&["zero", "delete", "insert", "dup"]),
            positions: Positions::Random(200),
            len: 3,
            seed: 2,
            ..base.clone()
        },
        Selection { kinds: kinds(&["random"]), random_count: 500, random_edits: 5, seed: 3, ..base },
    ]
}

/// Runs `selections` on each input with the parser binary `bin` and returns
/// every problem as `input variant: problem` lines, plus the number of runs.
pub fn check(bin: &Path, inputs: &[&Path], selections: &[Selection], jobs: usize) -> (Vec<String>, usize) {
    let cfg = RunConfig { bin: bin.to_path_buf(), extra_args: vec![], timeout: Duration::from_secs(10), repeat: false };
    let scratch = std::env::temp_dir().join(format!("vtj-stress-check-{}", std::process::id()));
    let (mut problems, mut runs) = (Vec::new(), 0);
    for input in inputs {
        let data = std::fs::read(input).unwrap_or_else(|e| panic!("{}: {e}", input.display()));
        let ext = input.extension().and_then(|e| e.to_str()).unwrap_or("bin");
        let mut muts: Vec<Mutation> = selections.iter().flat_map(|s| s.generate(data.len() as u64)).collect();
        muts.sort_by_key(|m| m.to_string());
        muts.dedup();
        runs += muts.len();
        let exec = Exec { jobs, fail_fast: false, scratch: scratch.clone() };
        let results = run_all(&cfg, &data, ext, &muts, &exec, &|_, _| {});
        for (m, r) in muts.iter().zip(results) {
            if let Some(Verdict::Problem(p)) = r {
                problems.push(format!("{} {m}: {p}", input.display()));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    (problems, runs)
}
