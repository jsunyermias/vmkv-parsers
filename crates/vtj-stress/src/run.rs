//! Runs a parser binary on mutated inputs and judges each run against the
//! parser contract.
//!
//! A run is acceptable when the process ends before the timeout and either
//! exits 0 with a valid success output or exits 1 with a valid, well-formed
//! failure output. Anything else is a problem: a panic (exit 101), a signal,
//! a hang, another exit code, an output the validator rejects, an output
//! whose kind does not match the exit code, or two runs that differ.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use vtj::validate::{validate, Options, Outcome};

use crate::mutation::Mutation;

#[derive(Debug, Clone)]
pub struct RunConfig {
    pub bin: PathBuf,
    /// Arguments passed before the input, such as `--frame-rate 25`.
    pub extra_args: Vec<String>,
    pub timeout: Duration,
    /// Run each variant twice and require identical output (rule 8).
    pub repeat: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Exit 0 with a valid success output.
    Success {
        units: u64,
    },
    /// Exit 1 with a valid failure output ending in this error code.
    Failure {
        code: String,
    },
    Problem(Problem),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// Exit 101 (Rust panic) or death by a signal.
    Crash {
        status: String,
        stderr: String,
    },
    Hang,
    /// An exit code other than 0 or 1.
    UnexpectedExit {
        code: i32,
        stderr: String,
    },
    /// The validator rejects the output.
    InvalidOutput {
        exit: i32,
        problems: Vec<String>,
    },
    /// Exit 0 with a failure output, or exit 1 without one.
    ExitMismatch {
        exit: i32,
        outcome: String,
    },
    /// A failure whose message starts with `internal:`: the writer caught
    /// the parser emitting a record that breaks the format (a parser bug).
    Internal {
        message: String,
    },
    NonDeterministic,
    /// The harness itself could not run the variant.
    Harness(String),
}

impl Verdict {
    pub fn is_problem(&self) -> bool {
        matches!(self, Verdict::Problem(_))
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::Crash { status, stderr } => write!(f, "CRASH ({status}): {stderr}"),
            Problem::Hang => write!(f, "HANG (timeout)"),
            Problem::UnexpectedExit { code, stderr } => write!(f, "UNEXPECTED EXIT {code}: {stderr}"),
            Problem::InvalidOutput { exit, problems } => {
                write!(f, "INVALID OUTPUT (exit {exit}): {}", problems.join("; "))
            }
            Problem::ExitMismatch { exit, outcome } => write!(f, "EXIT MISMATCH: exit {exit} with {outcome} output"),
            Problem::Internal { message } => write!(f, "INTERNAL (parser bug caught by the writer): {message}"),
            Problem::NonDeterministic => write!(f, "NON-DETERMINISTIC output"),
            Problem::Harness(m) => write!(f, "HARNESS ERROR: {m}"),
        }
    }
}

fn tail(s: &[u8]) -> String {
    let s = String::from_utf8_lossy(s);
    let s = s.trim();
    let start = s.char_indices().rev().nth(300).map_or(0, |(i, _)| i);
    s[start..].replace('\n', " | ")
}

struct Raw {
    status: Option<i32>,
    signal: Option<String>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

fn execute(cfg: &RunConfig, input: &Path, dir: &Path) -> Result<Raw, String> {
    let out_path = dir.join("out.vtj");
    let err_path = dir.join("err.txt");
    let out = File::create(&out_path).map_err(|e| e.to_string())?;
    let err = File::create(&err_path).map_err(|e| e.to_string())?;
    let mut child = Command::new(&cfg.bin)
        .args(&cfg.extra_args)
        .arg(input)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", cfg.bin.display()))?;
    let start = Instant::now();
    let mut pause = Duration::from_micros(200);
    let (status, timed_out) = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break (s, false);
        }
        if start.elapsed() >= cfg.timeout {
            let _ = child.kill();
            break (child.wait().map_err(|e| e.to_string())?, true);
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(20));
    };
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(|s| format!("signal {s}"))
    };
    #[cfg(not(unix))]
    let signal = None;
    Ok(Raw {
        status: status.code(),
        signal,
        stdout: std::fs::read(&out_path).map_err(|e| e.to_string())?,
        stderr: std::fs::read(&err_path).map_err(|e| e.to_string())?,
        timed_out,
    })
}

/// `(code, message)` of the error line that ends a failure output.
fn error_line(out: &[u8]) -> (String, String) {
    let last = out.split(|&b| b == b'\n').rfind(|l| !l.is_empty()).unwrap_or(b"");
    match vtj::json::parse(&String::from_utf8_lossy(last)).ok().and_then(|v| vtj::decode_line(&v).ok()) {
        Some(vtj::Line::Error(e)) => (e.code.as_str().to_string(), e.message),
        _ => ("?".into(), String::new()),
    }
}

fn judge(raw: &Raw) -> Verdict {
    if raw.timed_out {
        return Verdict::Problem(Problem::Hang);
    }
    let exit = match (raw.status, &raw.signal) {
        (Some(101), _) => {
            return Verdict::Problem(Problem::Crash { status: "exit 101, panic".into(), stderr: tail(&raw.stderr) })
        }
        (None, sig) => {
            let status = sig.clone().unwrap_or_else(|| "killed".into());
            return Verdict::Problem(Problem::Crash { status, stderr: tail(&raw.stderr) });
        }
        (Some(c @ (0 | 1)), _) => c,
        (Some(code), _) => return Verdict::Problem(Problem::UnexpectedExit { code, stderr: tail(&raw.stderr) }),
    };
    let report = validate(&raw.stdout, &Options { codec_aware: true, max_problems: 20 });
    match (exit, &report.outcome) {
        (_, Outcome::Invalid) => Verdict::Problem(Problem::InvalidOutput {
            exit,
            problems: report.problems.iter().map(ToString::to_string).collect(),
        }),
        (0, Outcome::Success) => Verdict::Success { units: report.unit_count },
        (1, Outcome::Failure) => match error_line(&raw.stdout) {
            (_, message) if message.starts_with("internal:") => Verdict::Problem(Problem::Internal { message }),
            (code, _) => Verdict::Failure { code },
        },
        (_, o) => Verdict::Problem(Problem::ExitMismatch { exit, outcome: format!("{o:?}").to_lowercase() }),
    }
}

/// Runs one variant in `dir` (a private scratch directory).
pub fn run_one(cfg: &RunConfig, data: &[u8], ext: &str, dir: &Path) -> Verdict {
    let input = dir.join(format!("input.{ext}"));
    if let Err(e) = std::fs::write(&input, data) {
        return Verdict::Problem(Problem::Harness(format!("cannot write {}: {e}", input.display())));
    }
    let first = match execute(cfg, &input, dir) {
        Ok(r) => r,
        Err(e) => return Verdict::Problem(Problem::Harness(e)),
    };
    let verdict = judge(&first);
    if cfg.repeat && !verdict.is_problem() {
        match execute(cfg, &input, dir) {
            Ok(second) if second.stdout != first.stdout || second.status != first.status => {
                return Verdict::Problem(Problem::NonDeterministic)
            }
            Ok(_) => {}
            Err(e) => return Verdict::Problem(Problem::Harness(e)),
        }
    }
    verdict
}

/// How a batch of variants is executed.
#[derive(Debug, Clone)]
pub struct Exec {
    /// Parallel runs.
    pub jobs: usize,
    /// Skip the remaining variants after the first problem.
    pub fail_fast: bool,
    /// Directory for per-worker scratch files.
    pub scratch: PathBuf,
}

/// Runs every mutation of `data`. `progress` is called after each run with
/// its index and verdict. With `fail_fast`, variants skipped after the first
/// problem come back as `None`.
pub fn run_all(
    cfg: &RunConfig,
    data: &[u8],
    ext: &str,
    mutations: &[Mutation],
    exec: &Exec,
    progress: &(dyn Fn(usize, &Verdict) + Sync),
) -> Vec<Option<Verdict>> {
    let (jobs, fail_fast, scratch) = (exec.jobs, exec.fail_fast, exec.scratch.as_path());
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let results: Mutex<Vec<Option<Verdict>>> = Mutex::new(vec![None; mutations.len()]);
    std::thread::scope(|s| {
        for w in 0..jobs.max(1) {
            let (next, stop, results) = (&next, &stop, &results);
            let dir = scratch.join(format!("worker-{w}"));
            s.spawn(move || {
                let _ = std::fs::create_dir_all(&dir);
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(m) = mutations.get(i) else { break };
                    let v = run_one(cfg, &m.apply(data), ext, &dir);
                    if fail_fast && v.is_problem() {
                        stop.store(true, Ordering::Relaxed);
                    }
                    progress(i, &v);
                    results.lock().expect("no poisoning")[i] = Some(v);
                }
            });
        }
    });
    results.into_inner().expect("no poisoning")
}
