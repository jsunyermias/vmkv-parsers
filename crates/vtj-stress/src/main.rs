//! `vtj-stress`: runs a VMKV parser on damaged variants of its inputs and
//! checks that it never crashes, hangs or breaks the output contract.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use vtj_stress::mutation::{parse_num, KINDS};
use vtj_stress::{run_all, Exec, Mutation, Offset, Positions, RunConfig, Selection, Verdict};

const USAGE: &str = "Usage: vtj-stress [OPTIONS] <INPUT>...

Runs a VMKV parser binary on mutated copies of each INPUT and checks the
contract: no crash, no hang, exit 0 with a valid success output or exit 1
with a valid failure output, and (with --repeat) identical double runs.

Parser:
  --parser <CODEC>         run vmkv-parser-CODEC (found next to vtj-stress or
                           in PATH); default from the extension
                           (.mp3 mp3, .aac/.adts aac, .opus/.ogg opus)
  --bin <PATH>             run this parser binary instead
  --parser-arg <ARG>       pass ARG to the parser before the input (repeatable)

Which variants (for every INPUT):
  --kinds <LIST>           comma list of: truncate, flip, set, zero, delete,
                           insert, dup, random (default: truncate)
  --all-kinds              every kind
  --from <OFFSET>          start of the range (default 0)
  --to <OFFSET>            end of the range, exclusive (default -0 = end)
                           OFFSET: N, 0xN, -N (from the end) or P%
  --step <N>               every Nth offset in the range (default 1)
  --positions <N>          N offsets spread evenly over the range
  --random-positions <N>   N distinct pseudo-random offsets (see --seed)
  --len <N>                bytes per zero/delete/insert/dup (default 1)
  --masks <LIST>           XOR masks for flip, e.g. 0x01,0x80 (default 0xff)
  --bit-flips              the eight single-bit masks
  --value <V|rand>         byte for set/insert, or random bytes for insert
  --random-count <N>       variants of kind random (default 100)
  --random-edits <K>       edits per random variant (default 3)
  --seed <S>               seed for random positions and variants (default 0)
  --variant <SPEC>         run exactly this variant (repeatable; replaces the
                           selection), e.g. truncate@100, flip@7:0x80,
                           delete@10+4, insert@0+8:rand, random@42x5
  --max-variants <N>       keep at most N variants per input, evenly spread

Execution:
  -j, --jobs <N>           parallel runs (default: available cores)
  --timeout-ms <MS>        per run (default 10000)
  --repeat                 run each variant twice and compare (rule 8)
  --fail-fast              stop an input at its first problem
  --keep <DIR>             save each problematic variant to DIR
  --list                   print the selected variants and exit
  --json                   report as JSON lines
  -q, --quiet              print only problems and the summary
  -h, --help               print help

Exit codes: 0 no problems, 1 problems found, 2 usage error.";

struct Args {
    inputs: Vec<PathBuf>,
    parser: Option<String>,
    bin: Option<PathBuf>,
    parser_args: Vec<String>,
    sel: Selection,
    variants: Vec<Mutation>,
    jobs: usize,
    timeout: Duration,
    repeat: bool,
    fail_fast: bool,
    keep: Option<PathBuf>,
    list: bool,
    json: bool,
    quiet: bool,
}

fn parse_args(argv: &[String]) -> Result<Option<Args>, String> {
    let mut a = Args {
        inputs: Vec::new(),
        parser: None,
        bin: None,
        parser_args: Vec::new(),
        sel: Selection::default(),
        variants: Vec::new(),
        jobs: std::thread::available_parallelism().map_or(1, |n| n.get()),
        timeout: Duration::from_millis(10_000),
        repeat: false,
        fail_fast: false,
        keep: None,
        list: false,
        json: false,
        quiet: false,
    };
    let mut it = argv.iter();
    let num = |v: Option<&String>, name: &str| -> Result<u64, String> {
        parse_num(v.ok_or_else(|| format!("{name} requires a value"))?).map_err(|e| format!("{name}: {e}"))
    };
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if arg.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let mut value = || inline.clone().or_else(|| it.next().cloned());
        match flag.as_str() {
            "-h" | "--help" => return Ok(None),
            "--parser" => a.parser = Some(value().ok_or("--parser requires a codec")?),
            "--bin" => a.bin = Some(PathBuf::from(value().ok_or("--bin requires a path")?)),
            "--parser-arg" => a.parser_args.push(value().ok_or("--parser-arg requires a value")?),
            "--kinds" => {
                a.sel.kinds =
                    value().ok_or("--kinds requires a list")?.split(',').map(|k| k.trim().to_string()).collect()
            }
            "--all-kinds" => a.sel.kinds = KINDS.iter().map(|k| k.to_string()).collect(),
            "--from" => a.sel.from = Offset::parse(&value().ok_or("--from requires an offset")?)?,
            "--to" => a.sel.to = Offset::parse(&value().ok_or("--to requires an offset")?)?,
            "--step" => a.sel.positions = Positions::Step(num(value().as_ref(), "--step")?),
            "--positions" => a.sel.positions = Positions::Even(num(value().as_ref(), "--positions")?),
            "--random-positions" => a.sel.positions = Positions::Random(num(value().as_ref(), "--random-positions")?),
            "--len" => a.sel.len = num(value().as_ref(), "--len")?,
            "--masks" => {
                a.sel.masks = value()
                    .ok_or("--masks requires a list")?
                    .split(',')
                    .map(|m| parse_num(m.trim()).and_then(|v| u8::try_from(v).map_err(|_| format!("mask {v} > 0xff"))))
                    .collect::<Result<_, _>>()?
            }
            "--bit-flips" => a.sel.masks = (0..8).map(|b| 1u8 << b).collect(),
            "--value" => {
                let v = value().ok_or("--value requires a byte or rand")?;
                if v == "rand" {
                    a.sel.insert_random = true;
                } else {
                    a.sel.value = u8::try_from(parse_num(&v)?).map_err(|_| "--value must be a byte")?;
                }
            }
            "--random-count" => a.sel.random_count = num(value().as_ref(), "--random-count")?,
            "--random-edits" => {
                a.sel.random_edits =
                    u32::try_from(num(value().as_ref(), "--random-edits")?).map_err(|_| "--random-edits too large")?
            }
            "--seed" => a.sel.seed = num(value().as_ref(), "--seed")?,
            "--variant" => a.variants.push(value().ok_or("--variant requires a spec")?.parse()?),
            "--max-variants" => a.sel.max = Some(num(value().as_ref(), "--max-variants")? as usize),
            "-j" | "--jobs" => a.jobs = num(value().as_ref(), "--jobs")?.max(1) as usize,
            "--timeout-ms" => a.timeout = Duration::from_millis(num(value().as_ref(), "--timeout-ms")?),
            "--repeat" => a.repeat = true,
            "--fail-fast" => a.fail_fast = true,
            "--keep" => a.keep = Some(PathBuf::from(value().ok_or("--keep requires a directory")?)),
            "--list" => a.list = true,
            "--json" => a.json = true,
            "-q" | "--quiet" => a.quiet = true,
            f if f.starts_with('-') => return Err(format!("unknown option {f}")),
            _ => a.inputs.push(PathBuf::from(arg)),
        }
    }
    if a.inputs.is_empty() {
        return Err("no input files".into());
    }
    a.sel.validate()?;
    Ok(Some(a))
}

fn codec_for(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "mp3" => Some("mp3"),
        "aac" | "adts" => Some("aac"),
        "opus" | "ogg" => Some("opus"),
        _ => None,
    }
}

fn find_parser(codec: &str) -> Option<PathBuf> {
    let file = format!("vmkv-parser-{codec}{}", std::env::consts::EXE_SUFFIX);
    let mut dirs: Vec<PathBuf> =
        std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).into_iter().collect();
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.into_iter().map(|d| d.join(&file)).find(|p| p.is_file())
}

fn json_str(s: &str) -> String {
    let mut out = String::new();
    vtj::json::write_str(&mut out, s);
    out
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
            eprintln!("vtj-stress: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    let scratch = std::env::temp_dir().join(format!("vtj-stress-{}", std::process::id()));
    let mut total_problems = 0usize;
    let mut total_runs = 0usize;
    let started = Instant::now();

    for input in &args.inputs {
        let data = match std::fs::read(input) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("vtj-stress: cannot read {}: {e}", input.display());
                return ExitCode::from(2);
            }
        };
        let ext = input.extension().and_then(|e| e.to_str()).unwrap_or("bin").to_string();
        let bin = match (&args.bin, args.parser.as_deref().or_else(|| codec_for(input))) {
            (Some(b), _) => b.clone(),
            (None, Some(codec)) => match find_parser(codec) {
                Some(b) => b,
                None => {
                    eprintln!("vtj-stress: vmkv-parser-{codec} not found next to vtj-stress or in PATH");
                    return ExitCode::from(2);
                }
            },
            (None, None) => {
                eprintln!("vtj-stress: cannot tell the codec of {}; use --parser or --bin", input.display());
                return ExitCode::from(2);
            }
        };
        let variants =
            if args.variants.is_empty() { args.sel.generate(data.len() as u64) } else { args.variants.clone() };

        if args.list {
            for v in &variants {
                println!("{}\t{v}", input.display());
            }
            continue;
        }

        let cfg = RunConfig {
            bin: bin.clone(),
            extra_args: args.parser_args.clone(),
            timeout: args.timeout,
            repeat: args.repeat,
        };
        let done = AtomicUsize::new(0);
        let n = variants.len();
        let show_progress = !args.quiet && !args.json && n >= 200;
        let progress = |_: usize, _: &Verdict| {
            let d = done.fetch_add(1, Ordering::Relaxed) + 1;
            if show_progress && (d.is_multiple_of((n / 20).max(1)) || d == n) {
                eprint!("\r{}: {d}/{n}", input.display());
            }
        };
        let exec = Exec { jobs: args.jobs, fail_fast: args.fail_fast, scratch: scratch.clone() };
        let results = run_all(&cfg, &data, &ext, &variants, &exec, &progress);
        if show_progress {
            eprintln!();
        }

        let (mut ok, mut skipped) = (0usize, 0usize);
        let mut failures: BTreeMap<String, usize> = BTreeMap::new();
        let mut problems = Vec::new();
        for (m, r) in variants.iter().zip(results) {
            match r {
                None => skipped += 1,
                Some(Verdict::Success { .. }) => ok += 1,
                Some(Verdict::Failure { code }) => *failures.entry(code).or_default() += 1,
                Some(Verdict::Problem(p)) => problems.push((*m, p)),
            }
        }
        total_runs += n - skipped;
        total_problems += problems.len();

        if let Some(dir) = &args.keep {
            let _ = std::fs::create_dir_all(dir);
            let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            for (m, _) in &problems {
                let name = format!("{stem}.{}.{ext}", m.to_string().replace([':', '@', '+'], "_"));
                let _ = std::fs::write(dir.join(name), m.apply(&data));
            }
        }

        if args.json {
            for (m, p) in &problems {
                println!(
                    "{{\"input\":{},\"variant\":{},\"problem\":{}}}",
                    json_str(&input.display().to_string()),
                    json_str(&m.to_string()),
                    json_str(&p.to_string())
                );
            }
            let codes: Vec<String> = failures.iter().map(|(c, k)| format!("{}:{k}", json_str(c))).collect();
            println!(
                "{{\"input\":{},\"variants\":{n},\"success\":{ok},\"failure\":{{{}}},\"problems\":{},\"skipped\":{skipped}}}",
                json_str(&input.display().to_string()),
                codes.join(","),
                problems.len()
            );
        } else {
            if !args.quiet || !problems.is_empty() {
                let codes: Vec<String> = failures.iter().map(|(c, k)| format!("{c} {k}")).collect();
                println!(
                    "{} ({} bytes, {n} variants): {ok} success, {} failure [{}], {} problems{}",
                    input.display(),
                    data.len(),
                    failures.values().sum::<usize>(),
                    codes.join(", "),
                    problems.len(),
                    if skipped > 0 { format!(", {skipped} skipped") } else { String::new() }
                );
            }
            for (m, p) in &problems {
                println!("  {m}: {p}");
                println!("    repro: vtj-stress --bin {} --variant {m} {}", bin.display(), input.display());
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    if args.list {
        return ExitCode::SUCCESS;
    }
    if !args.json {
        println!("total: {total_runs} runs, {total_problems} problems, {:.1} s", started.elapsed().as_secs_f64());
    }
    if total_problems > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
