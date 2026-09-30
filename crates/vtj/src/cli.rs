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

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufWriter, Write};

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
    Int,
    /// `N/D` (or `N`, meaning `N/1`), both terms > 0.
    Rational,
    String,
}

/// An external parameter a parser accepts.
#[derive(Debug, Clone, Copy)]
pub struct ParamSpec {
    /// snake_case name used in `header.params`; the option is `--kebab-case`.
    pub name: &'static str,
    pub kind: ParamKind,
    pub help: &'static str,
}

/// Frame rate for streams that carry no timing (`TIMING_REQUIRED`).
pub const FRAME_RATE: ParamSpec =
    ParamSpec { name: "frame_rate", kind: ParamKind::Rational, help: "frame rate as N/D, e.g. 24000/1001" };

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
            ParseError::new(ErrorCode::UnrepresentableInVmkv, "source read failed")
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
    output: Option<String>,
    inputs: Vec<String>,
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
        ParamKind::Int => Ok(ParamValue::Int(int(raw)?)),
        ParamKind::String => Ok(ParamValue::String(raw.to_string())),
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

fn parse_args(p: &dyn Parser, args: &[String]) -> Result<Parsed, String> {
    let mut inv = Invocation { output: None, inputs: Vec::new(), params: BTreeMap::new() };
    let mut i = 0;
    let mut only_inputs = false;
    while i < args.len() {
        let a = &args[i];
        i += 1;
        if only_inputs || !a.starts_with('-') || a == "-" {
            inv.inputs.push(a.clone());
            continue;
        }
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if a.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            if let Some(v) = inline.clone() {
                return Ok(v);
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
                inv.output = Some(value(flag)?);
            }
            _ => {
                let spec = flag
                    .strip_prefix("--")
                    .and_then(|n| p.params().iter().find(|s| kebab(s.name) == n))
                    .ok_or_else(|| format!("unknown option {flag}"))?;
                let v = parse_param(spec, &value(flag)?)?;
                if inv.params.insert(spec.name.to_string(), v).is_some() {
                    return Err(format!("{flag} given twice"));
                }
            }
        }
    }
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
        s.push_str(&format!("  --{} <VALUE>  {}\n", kebab(spec.name), spec.help));
    }
    s.push_str("  -h, --help           print help\n  -V, --version        print version\n");
    s
}

/// Runs `p` with `args` (without the program name). `stdout` receives the
/// output unless `--output` is given; diagnostics go to `stderr`.
pub fn run(p: &dyn Parser, args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
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

    let out: Box<dyn Write + '_> = match inv.output.as_deref() {
        None | Some("-") => Box::new(BufWriter::new(stdout)),
        Some(path) => match File::create(path) {
            Ok(f) => Box::new(BufWriter::new(f)),
            Err(e) => {
                let _ = writeln!(stderr, "{}: cannot create output {path}: {e}", p.name());
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
    if code == EXIT_OUTPUT_ERROR {
        let _ = writeln!(stderr, "{}: the output could not be written and must be discarded", p.name());
    }
    code
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
                let _ = writeln!(stderr, "{}: cannot read {path}: {e}", p.name());
                return Err(Some(ParseError::new(
                    ErrorCode::UnrepresentableInVmkv,
                    format!("source {i} cannot be read"),
                )));
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
    let args: Vec<String> = std::env::args_os().skip(1).map(|a: OsString| a.to_string_lossy().into_owned()).collect();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let code = run(p, &args, &mut stdout.lock(), &mut stderr.lock());
    std::process::exit(code)
}
