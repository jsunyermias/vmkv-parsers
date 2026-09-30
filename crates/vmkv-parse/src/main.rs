//! `vmkv-parse <codec> [ARGS...]` runs the parser binary `vmkv-parser-<codec>`
//! with the remaining arguments, in the style of `git` subcommands.
//!
//! The binary is looked up first next to this launcher, then in `PATH`. On
//! Unix the launcher replaces itself with the parser (`exec`), so stdout,
//! stderr and the exit code are the parser's own. Launcher errors (unknown
//! codec, missing binary) exit with 2 and write nothing to stdout, like any
//! usage error of the parser contract.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const PREFIX: &str = "vmkv-parser-";

const USAGE: &str = "Usage: vmkv-parse <CODEC> [PARSER OPTIONS] <INPUT>...
       vmkv-parse --list

Runs the VMKV parser vmkv-parser-<CODEC>, found next to vmkv-parse or in PATH.
Run `vmkv-parse <CODEC> --help` for the options of a parser.

Options:
  --list         list the parsers found and their versions
  -h, --help     print help
  -V, --version  print version";

fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) =
        env::current_exe().ok().and_then(|p| p.canonicalize().ok()).and_then(|p| p.parent().map(Path::to_path_buf))
    {
        dirs.push(dir);
    }
    if let Some(path) = env::var_os("PATH") {
        for d in env::split_paths(&path) {
            if !d.as_os_str().is_empty() && !dirs.contains(&d) {
                dirs.push(d);
            }
        }
    }
    dirs
}

fn is_executable(p: &Path) -> bool {
    let Ok(meta) = p.metadata() else { return false };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn valid_codec(c: &str) -> bool {
    !c.is_empty() && c.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !c.starts_with('-')
}

fn find(codec: &str) -> Option<PathBuf> {
    let file = format!("{PREFIX}{codec}{}", env::consts::EXE_SUFFIX);
    search_dirs().into_iter().map(|d| d.join(&file)).find(|p| is_executable(p))
}

/// Parsers found, as `(codec, path)`, first occurrence wins, sorted by codec.
fn discover() -> Vec<(String, PathBuf)> {
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for dir in search_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(codec) = name.strip_prefix(PREFIX).and_then(|c| c.strip_suffix(env::consts::EXE_SUFFIX)) else {
                continue;
            };
            if valid_codec(codec) && is_executable(&e.path()) && !found.iter().any(|(c, _)| c == codec) {
                found.push((codec.to_string(), e.path()));
            }
        }
    }
    found.sort();
    found
}

fn list() -> ExitCode {
    let found = discover();
    if found.is_empty() {
        eprintln!("vmkv-parse: no {PREFIX}* binaries found next to vmkv-parse or in PATH");
        return ExitCode::from(2);
    }
    for (codec, path) in found {
        let version = Command::new(&path)
            .arg("--version")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|| "(no version)".into());
        println!("{codec}\t{version}\t{}", path.display());
    }
    ExitCode::SUCCESS
}

fn run(path: &Path, args: Vec<OsString>) -> ExitCode {
    let mut cmd = Command::new(path);
    cmd.args(args);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = cmd.exec();
        eprintln!("vmkv-parse: cannot run {}: {err}", path.display());
        ExitCode::from(2)
    }
    #[cfg(not(unix))]
    {
        match cmd.status() {
            Ok(s) => ExitCode::from(s.code().unwrap_or(1).clamp(0, 255) as u8),
            Err(err) => {
                eprintln!("vmkv-parse: cannot run {}: {err}", path.display());
                ExitCode::from(2)
            }
        }
    }
}

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    let Some(first) = args.next() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match first.to_str() {
        Some("-h" | "--help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("-V" | "--version") => {
            println!("vmkv-parse {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--list") => list(),
        Some(codec) if valid_codec(codec) => match find(codec) {
            Some(path) => run(&path, args.collect()),
            None => {
                let known: Vec<String> = discover().into_iter().map(|(c, _)| c).collect();
                eprintln!(
                    "vmkv-parse: unknown codec \"{codec}\": no {PREFIX}{codec} next to vmkv-parse or in PATH (found: {})",
                    if known.is_empty() { "none".to_string() } else { known.join(", ") }
                );
                ExitCode::from(2)
            }
        },
        _ => {
            eprintln!("vmkv-parse: invalid codec name {first:?}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
