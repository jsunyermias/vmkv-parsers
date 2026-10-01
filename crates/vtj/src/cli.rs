//! The common parser contract: command line, `params`, sources, output and
//! exit codes. A parser implements [`Parser`] and calls [`main`].
//!
//! ```text
//! <parser> [OPTIONS] <INPUT>...
//!   -o, --output <PATH>   write to PATH instead of stdout ("-" = stdout)
//!   --<param> <VALUE>     external parameter declared by the parser,
//!                         e.g. --frame-rate 24000/1001
//!   -h, --help            -V, --version
//! ```
//!
//! Inputs become sources 0, 1, … in command-line order. Every parameter given
//! is recorded in `header.params` under its snake_case name.
//!
//! Exit codes: 0 success; 1 parse failure (an `error` line was written);
//! 2 usage error (nothing written); 3 the output could not be written.
//!
//! With `--output`, the output is written to a temporary file next to the
//! target and renamed over it at the end, so the target never holds a
//! partial file; an output that is one of the inputs (same file, also through
//! links) is refused before anything is opened.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::json::MAX_SAFE_INT;
use crate::source::SourceFile;
use crate::time::TimeError;
use crate::types::{ErrorCode, Header, ParamValue, ParserInfo, Rational, Track, Unit};
use crate::writer::{UnitSink, VtjWriter, WriteError};

pub const EXIT_OK: i32 = 0;
pub const EXIT_PARSE_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_OUTPUT_ERROR: i32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// An integer within `[min, max]`.
    Int {
        min: i64,
        max: i64,
    },
    /// `N/D` (or `N`, meaning `N/1`), both terms > 0.
    Rational,
    String,
    /// One of a closed set of names, recorded as a string.
    Choice(&'static [&'static str]),
}

/// An external parameter a parser accepts.
///
/// Parameters override what a parser would detect, choose a policy for
/// deviations from the standard, or select what to describe. A parameter
/// that is not given takes its documented default and is not recorded; a
/// parameter that is given is recorded in `header.params` (rule 8).
#[derive(Debug, Clone, Copy)]
pub struct ParamSpec {
    /// snake_case name used in `header.params`; the option is `--kebab-case`.
    pub name: &'static str,
    pub kind: ParamKind,
    pub help: &'static str,
    /// Behavior when the parameter is not given, for `--help`.
    pub default: Option<&'static str>,
}

impl ParamSpec {
    pub const fn int(name: &'static str, min: i64, max: i64, help: &'static str) -> Self {
        ParamSpec { name, kind: ParamKind::Int { min, max }, help, default: None }
    }

    pub const fn rational(name: &'static str, help: &'static str) -> Self {
        ParamSpec { name, kind: ParamKind::Rational, help, default: None }
    }

    pub const fn string(name: &'static str, help: &'static str) -> Self {
        ParamSpec { name, kind: ParamKind::String, help, default: None }
    }

    pub const fn choice(name: &'static str, choices: &'static [&'static str], help: &'static str) -> Self {
        ParamSpec { name, kind: ParamKind::Choice(choices), help, default: None }
    }

    pub const fn default(mut self, default: &'static str) -> Self {
        self.default = Some(default);
        self
    }

    fn metavar(&self) -> String {
        match self.kind {
            ParamKind::Int { .. } => "<N>".into(),
            ParamKind::Rational => "<N/D>".into(),
            ParamKind::String => "<VALUE>".into(),
            ParamKind::Choice(c) => format!("<{}>", c.join("|")),
        }
    }
}

/// Frame rate for streams that carry no timing (`TIMING_REQUIRED`).
pub const FRAME_RATE: ParamSpec = ParamSpec::rational("frame_rate", "frame rate as N/D, e.g. 24000/1001");

/// A parse failure, written as the terminal `error` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub code: ErrorCode,
    /// Must be deterministic: no paths, OS messages or dates.
    pub message: String,
}

impl ParseError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        ParseError { code, message: message.into() }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidBitstream, message)
    }

    pub fn truncated(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::TruncatedBitstream, message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::UnsupportedFeature, message)
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for ParseError {}

impl From<TimeError> for ParseError {
    fn from(e: TimeError) -> Self {
        ParseError::new(ErrorCode::UnrepresentableInVmkv, e.to_string())
    }
}

impl From<io::Error> for ParseError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            ParseError::truncated("unexpected end of source")
        } else {
            ParseError::new(ErrorCode::SourceUnreadable, "source read failed")
        }
    }
}

/// A codec parser.
pub trait Parser {
    fn name(&self) -> &'static str;
    fn version(&self) -> &'static str;
    fn params(&self) -> &'static [ParamSpec] {
        &[]
    }
    /// Checks combinations of the parameters given; an `Err` is a usage
    /// error (exit 2, nothing written).
    fn check_params(&self, _params: &BTreeMap<String, ParamValue>) -> Result<(), String> {
        Ok(())
    }
    /// Minimum and maximum number of input files.
    fn inputs(&self) -> (usize, usize) {
        (1, 1)
    }
    /// Emits every unit through `ctx.emit` in file order and returns the track.
    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError>;
}

/// What a parser sees while running.
pub struct Context<'w> {
    sources: Vec<SourceFile>,
    params: BTreeMap<String, ParamValue>,
    sink: &'w mut dyn UnitSink,
    output_failed: bool,
}

impl Context<'_> {
    pub fn sources(&mut self) -> &mut [SourceFile] {
        &mut self.sources
    }

    pub fn source(&mut self, id: u64) -> &mut SourceFile {
        &mut self.sources[id as usize]
    }

    pub fn param(&self, name: &str) -> Option<&ParamValue> {
        self.params.get(name)
    }

    pub fn param_rational(&self, name: &str) -> Option<Rational> {
        match self.params.get(name) {
            Some(ParamValue::Rational(r)) => Some(*r),
            _ => None,
        }
    }

    pub fn param_str(&self, name: &str) -> Option<&str> {
        match self.params.get(name) {
            Some(ParamValue::String(s)) => Some(s),
            _ => None,
        }
    }

    pub fn param_int(&self, name: &str) -> Option<i64> {
        match self.params.get(name) {
            Some(ParamValue::Int(i)) => Some(*i),
            _ => None,
        }
    }

    /// Writes one unit. Units must be emitted in file (decode) order.
    pub fn emit(&mut self, u: &Unit) -> Result<(), ParseError> {
        self.sink.unit(u).map_err(|e| self.write_failure(e))
    }

    pub fn units_emitted(&self) -> u64 {
        self.sink.unit_count()
    }

    fn write_failure(&mut self, e: WriteError) -> ParseError {
        match e {
            WriteError::Io(_) => {
                self.output_failed = true;
                ParseError::new(ErrorCode::UnrepresentableInVmkv, "output write failed")
            }
            WriteError::Contract(m) => ParseError::new(ErrorCode::UnrepresentableInVmkv, format!("internal: {m}")),
        }
    }
}

struct Invocation {
    /// Kept as `OsString`, not `String`: a path is not necessarily valid
    /// UTF-8 on Unix, and `File::open` must see the exact bytes given.
    output: Option<OsString>,
    inputs: Vec<OsString>,
    params: BTreeMap<String, ParamValue>,
}

enum Parsed {
    Run(Invocation),
    Help,
    Version,
}

fn kebab(name: &str) -> String {
    name.replace('_', "-")
}

fn parse_param(spec: &ParamSpec, raw: &str) -> Result<ParamValue, String> {
    let opt = kebab(spec.name);
    let int = |s: &str| -> Result<i64, String> {
        let v: i64 = s.parse().map_err(|_| format!("--{opt}: \"{raw}\" is not an integer"))?;
        if v.unsigned_abs() > MAX_SAFE_INT as u64 {
            return Err(format!("--{opt}: {v} is outside ±(2^53-1)"));
        }
        Ok(v)
    };
    match spec.kind {
        ParamKind::Int { min, max } => {
            let v = int(raw)?;
            if v < min || v > max {
                return Err(format!("--{opt}: {v} is outside {min}..={max}"));
            }
            Ok(ParamValue::Int(v))
        }
        ParamKind::String => Ok(ParamValue::String(raw.to_string())),
        ParamKind::Choice(choices) => match choices.iter().find(|c| **c == raw) {
            Some(c) => Ok(ParamValue::String(c.to_string())),
            None => Err(format!("--{opt}: \"{raw}\" is not one of {}", choices.join(", "))),
        },
        ParamKind::Rational => {
            let (n, d) = raw.split_once('/').unwrap_or((raw, "1"));
            let (n, d) = (int(n)?, int(d)?);
            if n <= 0 || d <= 0 {
                return Err(format!("--{opt}: both terms must be > 0"));
            }
            Ok(ParamValue::Rational(Rational::new(n, d)))
        }
    }
}

/// Whether `a` should be read as an option rather than a positional input:
/// starts with `-` and is not exactly `-` (the stdout/stdin sentinel). Only
/// the first byte is inspected, which is sound for any `OsStr` encoding
/// (`OsStr::as_encoded_bytes`'s ASCII bytes always round-trip).
fn looks_like_flag(a: &OsStr) -> bool {
    a.as_encoded_bytes().first() == Some(&b'-') && a != OsStr::new("-")
}

fn parse_args(p: &dyn Parser, args: &[OsString]) -> Result<Parsed, String> {
    let mut inv = Invocation { output: None, inputs: Vec::new(), params: BTreeMap::new() };
    let mut i = 0;
    let mut only_inputs = false;
    while i < args.len() {
        let a = &args[i];
        i += 1;
        if only_inputs || !looks_like_flag(a) {
            inv.inputs.push(a.clone());
            continue;
        }
        // An option name (and, with `--opt=value`, its value) must be valid
        // UTF-8: recognized flags are themselves ASCII, so this rejects
        // nothing a real option could be. A path given as a *separate*
        // argument (`-o <path>`, not `--output=<path>`) never goes through
        // this conversion and keeps its exact bytes; see `value_os` below.
        let a_str = a.to_str().ok_or_else(|| "an option must be valid UTF-8".to_string())?;
        let (flag, inline) = match a_str.split_once('=') {
            Some((f, v)) if a_str.starts_with("--") => (f, Some(v.to_string())),
            _ => (a_str, None),
        };
        let mut value_os = |name: &str| -> Result<OsString, String> {
            if let Some(v) = inline.clone() {
                return Ok(OsString::from(v));
            }
            let v = args.get(i).cloned().ok_or_else(|| format!("{name} requires a value"))?;
            i += 1;
            Ok(v)
        };
        match flag {
            "--" => only_inputs = true,
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            "-o" | "--output" => {
                if inv.output.is_some() {
                    return Err("--output given twice".into());
                }
                inv.output = Some(value_os(flag)?);
            }
            _ => {
                let spec = flag
                    .strip_prefix("--")
                    .and_then(|n| p.params().iter().find(|s| kebab(s.name) == n))
                    .ok_or_else(|| format!("unknown option {flag}"))?;
                let opt = kebab(spec.name);
                let raw = value_os(flag)?;
                let raw = raw.to_str().ok_or_else(|| format!("--{opt}: value must be valid UTF-8"))?;
                let v = parse_param(spec, raw)?;
                if inv.params.insert(spec.name.to_string(), v).is_some() {
                    return Err(format!("{flag} given twice"));
                }
            }
        }
    }
    p.check_params(&inv.params)?;
    let (min, max) = p.inputs();
    if inv.inputs.len() < min || inv.inputs.len() > max {
        return Err(if min == max {
            format!("expected {min} input file(s), got {}", inv.inputs.len())
        } else {
            format!("expected {min} to {max} input files, got {}", inv.inputs.len())
        });
    }
    Ok(Parsed::Run(inv))
}

fn usage(p: &dyn Parser) -> String {
    let mut s = format!(
        "{} {}\n\nUsage: {} [OPTIONS] <INPUT>...\n\nOptions:\n  -o, --output <PATH>  write the .vtj output to PATH (default: stdout)\n",
        p.name(),
        p.version(),
        p.name()
    );
    for spec in p.params() {
        s.push_str(&format!("  --{} {}\n      {}", kebab(spec.name), spec.metavar(), spec.help));
        if let ParamKind::Int { min, max } = spec.kind {
            s.push_str(&format!(" [{min}..={max}]"));
        }
        if let Some(d) = spec.default {
            s.push_str(&format!(" (default: {d})"));
        }
        s.push('\n');
    }
    s.push_str("  -h, --help           print help\n  -V, --version        print version\n");
    s
}

/// Runs `p` with `args` (without the program name). `stdout` receives the
/// output unless `--output` is given; diagnostics go to `stderr`.
pub fn run(p: &dyn Parser, args: &[OsString], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let inv = match parse_args(p, args) {
        Ok(Parsed::Run(inv)) => inv,
        Ok(Parsed::Help) => {
            let _ = stdout.write_all(usage(p).as_bytes());
            return EXIT_OK;
        }
        Ok(Parsed::Version) => {
            let _ = writeln!(stdout, "{} {}", p.name(), p.version());
            return EXIT_OK;
        }
        Err(e) => {
            let _ = writeln!(stderr, "{}: {e}\n\n{}", p.name(), usage(p));
            return EXIT_USAGE;
        }
    };

    let target = inv.output.as_deref().filter(|p| *p != OsStr::new("-")).map(PathBuf::from);
    if let Some(target) = &target {
        if let Some(input) = inv.inputs.iter().find(|i| same_file(target, Path::new(i))) {
            let _ = writeln!(
                stderr,
                "{}: the output {} is the input {}; refusing to overwrite it\n\n{}",
                p.name(),
                target.display(),
                Path::new(input).display(),
                usage(p)
            );
            return EXIT_USAGE;
        }
    }
    let (temp, out): (Option<PathBuf>, Box<dyn Write + '_>) = match &target {
        None => (None, Box::new(BufWriter::new(stdout))),
        Some(t) => match create_temp(t) {
            Ok((tmp, f)) => (Some(tmp), Box::new(BufWriter::new(f))),
            Err(e) => {
                let _ = writeln!(stderr, "{}: cannot create a temporary file next to {}: {e}", p.name(), t.display());
                return EXIT_OUTPUT_ERROR;
            }
        },
    };
    let mut writer = VtjWriter::new(out);
    let result = execute(p, inv, &mut writer, stderr);
    let code = match result {
        Ok(()) => EXIT_OK,
        Err(Some(e)) => {
            let _ = writeln!(stderr, "{}: {e}", p.name());
            match writer.error(e.code, &e.message) {
                Ok(()) => EXIT_PARSE_ERROR,
                Err(_) => EXIT_OUTPUT_ERROR,
            }
        }
        Err(None) => EXIT_OUTPUT_ERROR,
    };
    let mut code = code;
    let flushed = writer.into_inner().flush();
    if code != EXIT_OUTPUT_ERROR && flushed.is_err() {
        code = EXIT_OUTPUT_ERROR;
    }
    if let (Some(tmp), Some(target)) = (&temp, &target) {
        if code == EXIT_OUTPUT_ERROR {
            let _ = std::fs::remove_file(tmp);
        } else if let Err(e) = std::fs::rename(tmp, target) {
            let _ = writeln!(stderr, "{}: cannot rename {} to {}: {e}", p.name(), tmp.display(), target.display());
            let _ = std::fs::remove_file(tmp);
            code = EXIT_OUTPUT_ERROR;
        }
    }
    if code == EXIT_OUTPUT_ERROR {
        let _ = writeln!(stderr, "{}: the output could not be written and must be discarded", p.name());
    }
    code
}

/// Whether `a` and `b` name the same existing file (through hard or symbolic
/// links too).
fn same_file(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ma.dev() == mb.dev() && ma.ino() == mb.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = (ma, mb);
        matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
    }
}

/// How many unpredictable names `create_temp` tries before giving up.
const TEMP_ATTEMPTS: u32 = 8;

/// One candidate temporary file name next to `target` for the given
/// `attempt` and `salt` (decision 45): not just `.<name>.<pid>.tmp`, which
/// another local user could create ahead of time to force every run to
/// fail, or which a reused PID could make look like a leftover from a
/// previous, unrelated run. A pure function of its inputs, so its format is
/// deterministically testable independent of where `salt` comes from.
fn temp_candidate(target: &Path, attempt: u32, salt: u64) -> PathBuf {
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    target.with_file_name(format!(".{name}.{}.{salt:016x}.{attempt}.tmp", std::process::id()))
}

/// No RNG dependency: a `RandomState` keyed from OS randomness makes a
/// best-effort, not a cryptographically guaranteed, unpredictable suffix
/// (`RandomState` carries no API contract for that); collisions, accidental
/// or deliberately guessed, are handled by simply retrying with another one
/// (`create_temp`). See decision 45 for what this does and does not defend
/// against.
fn random_salt() -> u64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    RandomState::new().build_hasher().finish()
}

/// Creates a temporary file next to `target` with an unpredictable name,
/// renamed over `target` once the output is complete so it never holds a
/// partial file. Never follows an existing symlink at the chosen name
/// (`create_new` fails instead): only a candidate nothing occupies yet is
/// ever opened.
fn create_temp(target: &Path) -> io::Result<(PathBuf, File)> {
    create_temp_with(|attempt| temp_candidate(target, attempt, random_salt()))
}

/// `create_temp`'s retry loop, taking its candidate names from `candidate`
/// instead of always drawing a fresh random one — so a test can make
/// specific attempts collide and check the loop actually retries, which a
/// real random salt cannot be made to do on demand.
fn create_temp_with(mut candidate: impl FnMut(u32) -> PathBuf) -> io::Result<(PathBuf, File)> {
    let mut last_err = None;
    for attempt in 0..TEMP_ATTEMPTS {
        let tmp = candidate(attempt);
        match File::options().write(true).create_new(true).open(&tmp) {
            Ok(f) => return Ok((tmp, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last_err = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| io::Error::other("could not create a temporary file")))
}

/// `Err(None)` means the output itself failed and no error line can be written.
fn execute<W: Write>(
    p: &dyn Parser,
    inv: Invocation,
    writer: &mut VtjWriter<W>,
    stderr: &mut dyn Write,
) -> Result<(), Option<ParseError>> {
    let mut sources = Vec::new();
    for (i, path) in inv.inputs.iter().enumerate() {
        match SourceFile::open(i as u64, path) {
            Ok(s) => sources.push(s),
            Err(e) => {
                let _ = writeln!(stderr, "{}: cannot read {}: {e}", p.name(), Path::new(path).display());
                return Err(Some(ParseError::new(ErrorCode::SourceUnreadable, format!("source {i} cannot be read"))));
            }
        }
    }
    let header = Header {
        parser: ParserInfo { name: p.name().into(), version: p.version().into() },
        sources: sources.iter().map(SourceFile::header_entry).collect(),
        params: inv.params.clone(),
    };
    match writer.header(&header) {
        Ok(()) => {}
        Err(WriteError::Io(_)) => return Err(None),
        Err(WriteError::Contract(m)) => {
            return Err(Some(ParseError::new(ErrorCode::UnrepresentableInVmkv, format!("internal: {m}"))));
        }
    }
    let mut ctx = Context { sources, params: inv.params, sink: writer, output_failed: false };
    let parsed = p.parse(&mut ctx);
    let output_failed = ctx.output_failed;
    let track = match parsed {
        Ok(t) => t,
        Err(e) => return Err(if output_failed { None } else { Some(e) }),
    };
    if output_failed {
        return Err(None);
    }
    // The header's `sha256` was computed once, before `parse`; a source that
    // changed underneath the parser while it read from it (decision 44)
    // would otherwise let a `.vtj` describe a different byte sequence.
    if let Some(id) = ctx.sources().iter().position(|s| !s.verify_unchanged()) {
        return Err(Some(ParseError::new(
            ErrorCode::SourceUnreadable,
            format!("source {id} changed while it was being parsed"),
        )));
    }
    match writer.finish(&track) {
        Ok(()) => Ok(()),
        Err(WriteError::Io(_)) => Err(None),
        Err(WriteError::Contract(m)) => {
            Err(Some(ParseError::new(ErrorCode::UnrepresentableInVmkv, format!("internal: {m}"))))
        }
    }
}

/// Entry point for parser binaries: runs with the process arguments and exits.
pub fn main(p: &dyn Parser) -> ! {
    // Not `to_string_lossy`: that would silently rewrite a non-UTF-8 path
    // (possible on Unix) before `File::open` ever sees it (decision 43).
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let code = run(p, &args, &mut stdout.lock(), &mut stderr.lock());
    std::process::exit(code)
}

#[cfg(test)]
mod temp_name_tests {
    use super::{create_temp_with, temp_candidate, TEMP_ATTEMPTS};
    use std::path::{Path, PathBuf};

    /// The name's format itself, as a pure function of `(target, attempt,
    /// salt)` — no `RandomState` involved, so nothing here is probabilistic.
    #[test]
    fn temp_candidate_format_is_deterministic_and_not_the_old_fixed_pattern() {
        let target = Path::new("/tmp/out.vtj");
        let pid = std::process::id();
        let a = temp_candidate(target, 3, 0x1122_3344_5566_7788);
        assert_eq!(a, Path::new(&format!("/tmp/.out.vtj.{pid}.1122334455667788.3.tmp")));
        assert_eq!(a, temp_candidate(target, 3, 0x1122_3344_5566_7788), "same inputs, same name");
        assert_ne!(a, temp_candidate(target, 3, 0x1122_3344_5566_7789), "a different salt changes the name");
        assert_ne!(a, temp_candidate(target, 4, 0x1122_3344_5566_7788), "a different attempt changes the name");
        let old_style = format!(".out.vtj.{pid}.tmp");
        assert_ne!(a.file_name().unwrap().to_str().unwrap(), old_style, "not the old predictable name");
    }

    /// The retry loop itself, with the candidate source under the test's
    /// control instead of a real (and so uncontrollably random) salt: some
    /// attempts are made to collide on purpose, and the loop must skip past
    /// them, or give up cleanly after exactly `TEMP_ATTEMPTS` of them.
    #[test]
    fn create_temp_retries_past_existing_names_and_gives_up_after_the_limit() {
        let dir = std::env::temp_dir().join(format!("vtj-temp-retry-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let taken: Vec<PathBuf> = (0..2).map(|i| dir.join(format!("taken-{i}.tmp"))).collect();
        for t in &taken {
            std::fs::write(t, b"").unwrap();
        }
        let free = dir.join("free.tmp");
        let _ = std::fs::remove_file(&free);
        let mut calls = 0;
        let seq = taken.clone();
        let (tmp, _file) = create_temp_with(|attempt| {
            calls += 1;
            seq.get(attempt as usize).cloned().unwrap_or_else(|| free.clone())
        })
        .unwrap();
        assert_eq!(calls, 3, "two collisions, then a free name");
        assert_eq!(tmp, free);
        std::fs::remove_file(&tmp).unwrap();

        let mut calls = 0;
        let result = create_temp_with(|attempt| {
            calls += 1;
            taken[attempt as usize % taken.len()].clone()
        });
        assert!(result.is_err(), "every candidate collided");
        assert_eq!(calls, TEMP_ATTEMPTS, "gives up after exactly the attempt limit");
    }
}
