//! Whole-file validation of `.vtj` output (the specification's checklist).

use std::collections::BTreeSet;

use crate::check::{self, SourceSizes};
use crate::json;
use crate::types::{decode_line, Header, Line};

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Also check codec-dependent fields against the Matroska mappings.
    pub codec_aware: bool,
    /// Stop collecting problems after this many (0 = unlimited).
    pub max_problems: usize,
}

/// A problem at a 1-based line number (0 = the file as a whole).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.line == 0 {
            write!(f, "file: {}", self.message)
        } else {
            write!(f, "line {}: {}", self.line, self.message)
        }
    }
}

/// Classification of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `header unit* track end`, all checks passed.
    Success,
    /// `header? unit* error`, well formed. The output must be discarded.
    Failure,
    /// Anything else.
    Invalid,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub outcome: Outcome,
    pub problems: Vec<Problem>,
    /// The header, when one was decoded.
    pub header: Option<Header>,
    pub unit_count: u64,
}

struct Collector {
    problems: Vec<Problem>,
    max: usize,
}

impl Collector {
    fn push(&mut self, line: usize, message: impl Into<String>) {
        if self.max == 0 || self.problems.len() < self.max {
            self.problems.push(Problem { line, message: message.into() });
        }
    }

    fn extend(&mut self, line: usize, msgs: Vec<String>) {
        for m in msgs {
            self.push(line, m);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Start,
    Units,
    Track,
    Done,
}

/// Validates a complete `.vtj` file.
pub fn validate(bytes: &[u8], opts: &Options) -> Report {
    let mut c = Collector { problems: Vec::new(), max: opts.max_problems };
    let mut header: Option<Header> = None;
    let mut sizes = SourceSizes::new();
    let mut used_ids = BTreeSet::new();
    let mut unit_count: u64 = 0;
    let mut state = State::Start;
    let mut saw_error = false;
    let mut saw_end = false;

    if bytes.is_empty() {
        c.push(0, "empty file");
    }
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        c.push(0, "UTF-8 byte order mark is not allowed");
    }
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        c.push(0, "last line must end with LF");
    }

    let body = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let lines: Vec<&[u8]> = if bytes.is_empty() { Vec::new() } else { body.split(|&b| b == b'\n').collect() };
    let total = lines.len();

    for (idx, raw) in lines.into_iter().enumerate() {
        let n = idx + 1;
        if state == State::Done {
            c.push(n, "no lines are allowed after end or error");
            break;
        }
        let raw = if idx == 0 { raw.strip_prefix(&[0xef, 0xbb, 0xbf][..]).unwrap_or(raw) } else { raw };
        if raw.contains(&b'\r') {
            c.push(n, "CR is not allowed; lines end with LF only");
        }
        let text = match std::str::from_utf8(raw) {
            Ok(t) => t,
            Err(e) => {
                c.push(n, format!("invalid UTF-8 at byte {}", e.valid_up_to()));
                continue;
            }
        };
        if text.is_empty() {
            c.push(n, "empty line");
            continue;
        }
        let value = match json::parse(text) {
            Ok(v) => v,
            Err(e) => {
                c.push(n, format!("invalid JSON: {e}"));
                continue;
            }
        };
        let line = match decode_line(&value) {
            Ok(l) => l,
            Err(e) => {
                c.push(n, e);
                continue;
            }
        };
        let canonical = line.to_canonical();
        if canonical != text.trim_end_matches('\r') {
            let at = canonical.bytes().zip(text.bytes()).take_while(|(a, b)| a == b).count();
            c.push(n, format!("not in canonical serialization (first difference at byte {at}); expected: {canonical}"));
        }

        match (&line, state) {
            (Line::Header(_), State::Start) if idx == 0 => {}
            (Line::Header(_), _) => c.push(n, "header must be the first line and appear once"),
            (Line::Unit(_), State::Start | State::Units) => {}
            (Line::Unit(_), _) => c.push(n, "unit lines must come before track"),
            (Line::Track(_), State::Start | State::Units) => {}
            (Line::Track(_), _) => c.push(n, "only one track line is allowed"),
            (Line::End(_), State::Track) => {}
            (Line::End(_), _) => c.push(n, "end must directly follow track"),
            (Line::Error(_), State::Start | State::Units) => {}
            (Line::Error(_), _) => c.push(n, "error cannot follow track"),
        }

        match line {
            Line::Header(h) => {
                if header.is_none() && idx == 0 {
                    c.extend(n, check::header(&h));
                    sizes = check::source_sizes(&h);
                    header = Some(h);
                }
                state = State::Units;
            }
            Line::Unit(u) => {
                c.extend(n, check::unit(&u, &sizes));
                used_ids.extend(u.block_additions.iter().map(|b| b.id));
                unit_count += 1;
                state = State::Units;
            }
            Line::Track(t) => {
                c.extend(n, check::track(&t, &sizes, &used_ids));
                if opts.codec_aware {
                    c.extend(n, check::codec_aware(&t));
                }
                state = State::Track;
            }
            Line::End(e) => {
                if e.unit_count != unit_count {
                    c.push(n, format!("unit_count is {} but the file has {unit_count} unit lines", e.unit_count));
                }
                saw_end = true;
                state = State::Done;
            }
            Line::Error(_) => {
                saw_error = true;
                state = State::Done;
            }
        }
    }

    if !saw_end && !saw_error && total > 0 {
        c.push(0, "incomplete output: missing end line (the output must be discarded)");
    }
    if header.is_none() && !saw_error && total > 0 {
        c.push(0, "missing header line");
    }

    let outcome = if !c.problems.is_empty() {
        Outcome::Invalid
    } else if saw_error {
        Outcome::Failure
    } else {
        Outcome::Success
    };
    Report { outcome, problems: c.problems, header, unit_count }
}
