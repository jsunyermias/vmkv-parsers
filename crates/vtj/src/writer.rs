//! Canonical `.vtj` writer. Parsers never write JSON by hand.
//!
//! The writer enforces the line grammar through its API: `header`, then any
//! number of `unit`, then `finish` (which writes `track` and `end` together),
//! or `error` at any point before `finish`. Every record is checked with
//! [`crate::check`] before it is written, so a parser bug surfaces as a
//! [`WriteError::Contract`] instead of an invalid file.

use std::collections::BTreeSet;
use std::io::{self, Write};

use crate::check::{self, SourceSizes};
use crate::types::{End, ErrorCode, ErrorLine, Header, Line, Track, Unit};

#[derive(Debug)]
pub enum WriteError {
    Io(io::Error),
    /// The record or the call order violates the format.
    Contract(String),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::Io(e) => write!(f, "write failed: {e}"),
            WriteError::Contract(m) => write!(f, "format contract violation: {m}"),
        }
    }
}

impl std::error::Error for WriteError {}

impl From<io::Error> for WriteError {
    fn from(e: io::Error) -> Self {
        WriteError::Io(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Start,
    Units,
    Closed,
}

/// Destination for units, as seen by a running parser.
pub trait UnitSink {
    fn unit(&mut self, u: &Unit) -> Result<(), WriteError>;
    fn unit_count(&self) -> u64;
}

impl<W: Write> UnitSink for VtjWriter<W> {
    fn unit(&mut self, u: &Unit) -> Result<(), WriteError> {
        VtjWriter::unit(self, u)
    }

    fn unit_count(&self) -> u64 {
        self.unit_count
    }
}

pub struct VtjWriter<W: Write> {
    out: W,
    state: State,
    sizes: SourceSizes,
    used_ids: BTreeSet<u64>,
    unit_count: u64,
}

fn contract(what: &str, problems: Vec<String>) -> Result<(), WriteError> {
    if problems.is_empty() {
        Ok(())
    } else {
        Err(WriteError::Contract(format!("{what}: {}", problems.join("; "))))
    }
}

impl<W: Write> VtjWriter<W> {
    pub fn new(out: W) -> Self {
        VtjWriter { out, state: State::Start, sizes: SourceSizes::new(), used_ids: BTreeSet::new(), unit_count: 0 }
    }

    fn line(&mut self, l: &Line) -> io::Result<()> {
        let mut s = l.to_canonical();
        s.push('\n');
        self.out.write_all(s.as_bytes())
    }

    fn expect(&self, allowed: &[State], what: &str) -> Result<(), WriteError> {
        if allowed.contains(&self.state) {
            Ok(())
        } else {
            Err(WriteError::Contract(format!("{what} is not allowed in state {:?}", self.state)))
        }
    }

    pub fn header(&mut self, h: &Header) -> Result<(), WriteError> {
        self.expect(&[State::Start], "header")?;
        contract("header", check::header(h))?;
        self.line(&Line::Header(h.clone()))?;
        self.sizes = check::source_sizes(h);
        self.state = State::Units;
        Ok(())
    }

    pub fn unit(&mut self, u: &Unit) -> Result<(), WriteError> {
        self.expect(&[State::Units], "unit")?;
        contract(&format!("unit {}", self.unit_count), check::unit(u, &self.sizes))?;
        self.line(&Line::Unit(u.clone()))?;
        self.used_ids.extend(u.block_additions.iter().map(|b| b.id));
        self.unit_count += 1;
        Ok(())
    }

    pub fn unit_count(&self) -> u64 {
        self.unit_count
    }

    /// Writes `track` and `end`, then flushes.
    pub fn finish(&mut self, t: &Track) -> Result<(), WriteError> {
        self.expect(&[State::Units], "track")?;
        contract("track", check::track(t, &self.sizes, &self.used_ids))?;
        self.line(&Line::Track(t.clone()))?;
        self.line(&Line::End(End { unit_count: self.unit_count }))?;
        self.state = State::Closed;
        self.out.flush()?;
        Ok(())
    }

    /// Writes the terminal `error` line, then flushes. Allowed before the
    /// header and between units, never after `finish`.
    pub fn error(&mut self, code: ErrorCode, message: &str) -> Result<(), WriteError> {
        self.expect(&[State::Start, State::Units], "error")?;
        self.line(&Line::Error(ErrorLine { code, message: message.to_string() }))?;
        self.state = State::Closed;
        self.out.flush()?;
        Ok(())
    }

    pub fn is_closed(&self) -> bool {
        self.state == State::Closed
    }

    pub fn into_inner(self) -> W {
        self.out
    }
}
