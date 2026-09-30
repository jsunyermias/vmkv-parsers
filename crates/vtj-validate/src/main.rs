//! `vtj-validate`: checks a `.vtj` v1 file against the specification checklist.
//!
//! Exit codes: 0 valid success output; 1 invalid; 2 usage or I/O error;
//! 3 well-formed failure output (ends in `error`, must be discarded).

use std::io::{self, Read, Write};
use std::process::ExitCode;

use vtj::source::SourceFile;
use vtj::validate::{validate, Options, Outcome, Problem};

const USAGE: &str = "Usage: vtj-validate [OPTIONS] <FILE|->

Validates a VMKV parser output file (.vtj v1).

Options:
  --codec-aware          also check codec_private, codec_delay_ns, seek_preroll_ns
                         and uncompressed_fourcc against the Matroska mappings
  --source <ID>=<PATH>   check size and sha256 of source ID against PATH (repeatable)
  --max-problems <N>     report at most N problems (default 100, 0 = all)
  -q, --quiet            print nothing, only set the exit code
  -h, --help             print help

Exit codes: 0 valid, 1 invalid, 2 usage or I/O error, 3 valid failure output";

struct Args {
    file: String,
    opts: Options,
    sources: Vec<(u64, String)>,
    quiet: bool,
}

fn parse_args(args: &[String]) -> Result<Option<Args>, String> {
    let mut file = None;
    let mut opts = Options { codec_aware: false, max_problems: 100 };
    let mut sources = Vec::new();
    let mut quiet = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(None),
            "--codec-aware" => opts.codec_aware = true,
            "-q" | "--quiet" => quiet = true,
            "--max-problems" => {
                let v = it.next().ok_or("--max-problems requires a value")?;
                opts.max_problems = v.parse().map_err(|_| format!("invalid --max-problems {v}"))?;
            }
            "--source" => {
                let v = it.next().ok_or("--source requires ID=PATH")?;
                let (id, path) = v.split_once('=').ok_or("--source requires ID=PATH")?;
                let id = id.parse().map_err(|_| format!("invalid source id {id}"))?;
                sources.push((id, path.to_string()));
            }
            s if s.starts_with('-') && s != "-" => return Err(format!("unknown option {s}")),
            s => {
                if file.replace(s.to_string()).is_some() {
                    return Err("exactly one input file is expected".into());
                }
            }
        }
    }
    let file = file.ok_or("missing input file")?;
    Ok(Some(Args { file, opts, sources, quiet }))
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(Some(a)) => a,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("vtj-validate: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let mut bytes = Vec::new();
    let read = if args.file == "-" {
        io::stdin().read_to_end(&mut bytes).map(|_| ())
    } else {
        std::fs::read(&args.file).map(|b| bytes = b)
    };
    if let Err(e) = read {
        eprintln!("vtj-validate: cannot read {}: {e}", args.file);
        return ExitCode::from(2);
    }

    let mut report = validate(&bytes, &args.opts);

    for (id, path) in &args.sources {
        let declared = report.header.as_ref().and_then(|h| h.sources.iter().find(|s| s.id == *id));
        let Some(declared) = declared else {
            report
                .problems
                .push(Problem { line: 1, message: format!("--source {id}: no source with that id in the header") });
            continue;
        };
        match SourceFile::open(*id, path) {
            Err(e) => {
                eprintln!("vtj-validate: cannot read source {path}: {e}");
                return ExitCode::from(2);
            }
            Ok(actual) => {
                if actual.size() != declared.size {
                    report.problems.push(Problem {
                        line: 1,
                        message: format!(
                            "sources id {id}: size {} does not match {path} ({} bytes)",
                            declared.size,
                            actual.size()
                        ),
                    });
                }
                if let Some(sha) = &declared.sha256 {
                    if sha != actual.sha256() {
                        report.problems.push(Problem {
                            line: 1,
                            message: format!("sources id {id}: sha256 does not match {path}"),
                        });
                    }
                }
            }
        }
    }
    if !report.problems.is_empty() {
        report.outcome = Outcome::Invalid;
    }

    if !args.quiet {
        let stdout = io::stdout();
        let mut out = stdout.lock();
        for p in &report.problems {
            let _ = writeln!(out, "{p}");
        }
        let _ = match report.outcome {
            Outcome::Success => writeln!(out, "valid: {} units", report.unit_count),
            Outcome::Failure => writeln!(out, "valid failure output: ends in an error line and must be discarded"),
            Outcome::Invalid => writeln!(out, "invalid: {} problem(s)", report.problems.len()),
        };
    }
    ExitCode::from(match report.outcome {
        Outcome::Success => 0,
        Outcome::Invalid => 1,
        Outcome::Failure => 3,
    })
}
