//! SubRip subtitles (`S_TEXT/UTF8`).
//!
//! - The payload of each unit is only the cue text, without its index or
//!   timing lines. Every unit carries `random_access` and
//!   `duration_required`: cues are independent and the gaps between them
//!   carry no subtitle (decision 39).
//! - A UTF-8 source, with or without a BOM, is referenced directly with
//!   `src` chunks. Line endings inside a multi-line cue are kept exactly as
//!   in the source (LF or CRLF): a `src` chunk is a reference, not a
//!   transform, and rule 7 forbids inventing or discarding data.
//! - A source that is not valid UTF-8 is transcoded once to UTF-8 and every
//!   payload becomes `inline`, per the spec. UTF-16 (BOM `FF FE` or
//!   `FE FF`) is decoded as UTF-16; any other non-UTF-8 byte is decoded as
//!   Windows-1252, the common fallback for legacy 8-bit `.srt` files
//!   (decision 39).
//! - Blank lines inside a cue are kept as part of its text when what follows
//!   them is not a new cue (an index line and a timing line) nor the end of
//!   the file; `--blank-lines-in-cue strict` makes every blank line end the
//!   cue instead (decision 67).
//! - Rejected: a cue whose index line is not a run of ASCII digits, a
//!   timing line that is not `HH:MM:SS,mmm --> HH:MM:SS,mmm` (a decimal
//!   point instead of a comma is not accepted: guessing the separator would
//!   mask real corruption), and an end instant before its start.

use vtj::cli::{ParamSpec, ParseError};
use vtj::*;

/// Windows-1252 code points for bytes `0x80..=0x9F`; the rest of the byte
/// range maps to the identical Unicode code point (true of both Latin-1 and
/// Windows-1252). Five bytes in this range are undefined in Windows-1252
/// (0x81, 0x8D, 0x8F, 0x90, 0x9D); they fall back to their Latin-1 C1
/// control code, since no better deterministic choice exists.
const CP1252_HIGH: [u16; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x008D,
    0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A,
    0x0153, 0x009D, 0x017E, 0x0178,
];

fn cp1252_to_utf8(raw: &[u8]) -> Vec<u8> {
    let mut s = String::with_capacity(raw.len());
    for &b in raw {
        let cp = if (0x80..=0x9f).contains(&b) { CP1252_HIGH[(b - 0x80) as usize] as u32 } else { b as u32 };
        s.push(char::from_u32(cp).expect("every byte maps to a valid scalar value"));
    }
    s.into_bytes()
}

fn utf16_to_utf8(bytes: &[u8], big_endian: bool) -> Result<Vec<u8>, ParseError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(ParseError::truncated("UTF-16 content has an odd number of trailing bytes"));
    }
    let units = bytes.chunks_exact(2).map(|c| {
        if big_endian {
            u16::from_be_bytes([c[0], c[1]])
        } else {
            u16::from_le_bytes([c[0], c[1]])
        }
    });
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .map(String::into_bytes)
        .map_err(|_| ParseError::invalid("invalid UTF-16 surrogate pair"))
}

/// The text scanned for cues, and how a cue's payload is built from it.
enum Text<'a> {
    /// Valid UTF-8 bytes of the source itself, `base` bytes in (past a UTF-8
    /// BOM, if any). Cue payloads reference the source directly.
    Source { data: &'a [u8], base: u64 },
    /// Transcoded to UTF-8; cue payloads embed their text directly.
    Transcoded(Vec<u8>),
}

impl Text<'_> {
    fn bytes(&self) -> &[u8] {
        match self {
            Text::Source { data, .. } => data,
            Text::Transcoded(v) => v,
        }
    }

    fn payload(&self, start: usize, len: usize) -> DataChain {
        match self {
            Text::Source { base, .. } => vec![Chunk::src(0, base + start as u64, len as u64)],
            Text::Transcoded(v) => vec![Chunk::inline(v[start..start + len].to_vec())],
        }
    }
}

fn decode(raw: &[u8]) -> Result<Text<'_>, ParseError> {
    if let Some(rest) = raw.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        std::str::from_utf8(rest)
            .map_err(|e| ParseError::invalid(format!("invalid UTF-8 at byte {}", 3 + e.valid_up_to())))?;
        return Ok(Text::Source { data: rest, base: 3 });
    }
    if let Some(rest) = raw.strip_prefix(&[0xff, 0xfe]) {
        return Ok(Text::Transcoded(utf16_to_utf8(rest, false)?));
    }
    if let Some(rest) = raw.strip_prefix(&[0xfe, 0xff]) {
        return Ok(Text::Transcoded(utf16_to_utf8(rest, true)?));
    }
    match std::str::from_utf8(raw) {
        Ok(_) => Ok(Text::Source { data: raw, base: 0 }),
        Err(_) => Ok(Text::Transcoded(cp1252_to_utf8(raw))),
    }
}

/// `(content_end, next_line_start)` for the line starting at `pos`: content
/// excludes a trailing CR, and `next` is past the LF (or at `buf.len()` when
/// `pos` starts the last, unterminated line).
fn split_line(buf: &[u8], pos: usize) -> (usize, usize) {
    let mut i = pos;
    while i < buf.len() && buf[i] != b'\n' {
        i += 1;
    }
    let mut end = i;
    if end > pos && buf[end - 1] == b'\r' {
        end -= 1;
    }
    let next = if i < buf.len() { i + 1 } else { i };
    (end, next)
}

fn is_blank(buf: &[u8], start: usize, end: usize) -> bool {
    buf[start..end].iter().all(|b| *b == b' ' || *b == b'\t')
}

fn skip_blank_lines(buf: &[u8], mut pos: usize) -> usize {
    loop {
        let (end, next) = split_line(buf, pos);
        if next == pos || !is_blank(buf, pos, end) {
            return pos;
        }
        pos = next;
    }
}

/// Total milliseconds of a strict `HH:MM:SS,mmm` timestamp.
fn parse_timestamp(s: &str) -> Option<i64> {
    let (hms, ms) = s.split_once(',')?;
    if ms.len() != 3 || !ms.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut parts = hms.split(':');
    let (h, m, sec) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || h.is_empty() || !h.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if m.len() != 2
        || !m.bytes().all(|b| b.is_ascii_digit())
        || sec.len() != 2
        || !sec.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let (h, m, sec, ms): (i64, i64, i64, i64) = (h.parse().ok()?, m.parse().ok()?, sec.parse().ok()?, ms.parse().ok()?);
    if m >= 60 || sec >= 60 {
        return None;
    }
    // `h` alone fits `i64` (it was parsed as one), but the hour field is
    // otherwise unbounded, so the running total is kept in `i128` and only
    // converted back to `i64` at the end; a value that does not fit becomes
    // `None` (INVALID_BITSTREAM), never an overflow panic.
    let total_ms = ((h as i128 * 60 + m as i128) * 60 + sec as i128) * 1000 + ms as i128;
    i64::try_from(total_ms).ok()
}

/// `(start_ms, end_ms)` of a `HH:MM:SS,mmm --> HH:MM:SS,mmm` line; text
/// after the second timestamp (cue positioning hints) is ignored.
fn parse_cue_times(line: &str) -> Option<(i64, i64)> {
    let (start, rest) = line.split_once("-->")?;
    let end = rest.split_whitespace().next()?;
    Some((parse_timestamp(start.trim())?, parse_timestamp(end)?))
}

/// No real-world `.srt` approaches this; it exists only to bound how much
/// memory a single source can make this parser commit (decision 53), since
/// the whole file is scanned in memory, as-is for a UTF-8 source and again,
/// transcoded, for one that is not (decision 39).
const MAX_SOURCE_BYTES: u64 = 64 << 20;

/// A zero-filled buffer of exactly `size` bytes, guarding against: a source
/// over the documented limit above; one too large to address on this
/// platform (`usize::try_from`; u64 and usize share range on a 64-bit
/// target, so only a 32-bit one can ever hit this); and an allocation
/// failure for an absurdly large but in-range size (`try_reserve_exact`,
/// mainly useful on a platform where `MAX_SOURCE_BYTES` itself does not fit
/// `usize`). Any of the three becomes a well-formed `error` line instead of
/// exhausting memory or aborting the process outright.
fn allocate_for_source(size: u64) -> Result<Vec<u8>, ParseError> {
    if size > MAX_SOURCE_BYTES {
        return Err(ParseError::new(
            ErrorCode::UnsupportedFeature,
            format!("source is {size} bytes, more than the {MAX_SOURCE_BYTES}-byte limit for a subtitle file"),
        ));
    }
    let size_usize = usize::try_from(size).map_err(|_| {
        ParseError::new(
            ErrorCode::SourceUnreadable,
            format!("source is {size} bytes, too large to address on this platform"),
        )
    })?;
    let mut raw = Vec::new();
    raw.try_reserve_exact(size_usize).map_err(|_| {
        ParseError::new(ErrorCode::SourceUnreadable, format!("cannot allocate {size_usize} bytes to read the source"))
    })?;
    raw.resize(size_usize, 0);
    Ok(raw)
}

const BLANK_LINES_IN_CUE: ParamSpec = ParamSpec::choice(
    "blank_lines_in_cue",
    &["keep", "strict"],
    "keep: blank lines not followed by a new cue (index and timing line) are part of the cue text; \
     strict: a blank line always ends the cue",
)
.default("keep");

/// Whether a new cue starts at `pos`: an index line followed by a timing line.
fn cue_starts_at(buf: &[u8], pos: usize) -> bool {
    let (end, next) = split_line(buf, pos);
    let index = &buf[pos..end];
    let index = std::str::from_utf8(index).map(str::trim).unwrap_or("");
    if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) || next >= buf.len() {
        return false;
    }
    let (end, _) = split_line(buf, next);
    std::str::from_utf8(&buf[next..end]).ok().and_then(parse_cue_times).is_some()
}

pub struct Srt;

impl Parser for Srt {
    fn name(&self) -> &'static str {
        "vmkv-parser-srt"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn params(&self) -> &'static [ParamSpec] {
        &[BLANK_LINES_IN_CUE]
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let strict = ctx.param_str("blank_lines_in_cue") == Some("strict");
        let mut raw = allocate_for_source(ctx.source(0).size())?;
        ctx.source(0).read_at(0, &mut raw)?;
        let text = decode(&raw)?;
        let buf = text.bytes();
        let ms_rate = Rational::new(1000, 1);
        let flags = Flags::NONE.with(Flag::RandomAccess).with(Flag::DurationRequired);

        let mut pos = 0usize;
        loop {
            pos = skip_blank_lines(buf, pos);
            if pos >= buf.len() {
                break;
            }
            let cue_start = pos;

            let (end, next) = split_line(buf, pos);
            let index_line = std::str::from_utf8(&buf[pos..end]).expect("validated UTF-8").trim();
            if index_line.is_empty() || !index_line.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ParseError::invalid(format!("expected a cue index at byte {cue_start}")));
            }
            pos = next;
            if pos >= buf.len() {
                return Err(ParseError::truncated(format!("cue at byte {cue_start} is missing its timing line")));
            }

            let (end, next) = split_line(buf, pos);
            let timing_line = std::str::from_utf8(&buf[pos..end]).expect("validated UTF-8");
            let (start_ms, end_ms) = parse_cue_times(timing_line)
                .ok_or_else(|| ParseError::invalid(format!("malformed timing at byte {pos}")))?;
            if end_ms < start_ms {
                return Err(ParseError::invalid(format!("cue at byte {cue_start} ends before it starts")));
            }
            pos = next;

            // The text runs to the first blank line, unless (decision 67)
            // what follows the blank lines is neither a new cue nor the end
            // of the file: then the blank lines belong to the text, as in
            // cues that start with blank lines to raise the text on screen.
            let text_start = pos;
            let mut text_end = pos;
            while pos < buf.len() {
                let (end, next) = split_line(buf, pos);
                if is_blank(buf, pos, end) {
                    let after = skip_blank_lines(buf, next);
                    if strict || after >= buf.len() || cue_starts_at(buf, after) {
                        pos = next;
                        break;
                    }
                    let (end, _) = split_line(buf, after);
                    let line = std::str::from_utf8(&buf[after..end]).expect("validated UTF-8");
                    if parse_cue_times(line).is_some() {
                        return Err(ParseError::invalid(format!("timing line without a cue index at byte {after}")));
                    }
                    pos = after;
                    continue;
                }
                text_end = end;
                pos = next;
            }

            let pts = ticks_to_ns(start_ms as i128, ms_rate)?;
            let end_ns = ticks_to_ns(end_ms as i128, ms_rate)?;
            let payload = text.payload(text_start, text_end - text_start);
            ctx.emit(&Unit::new(pts, end_ns - pts, flags, payload))?;
        }

        // A file with no cues is an empty track, not an error (decision 68).
        Ok(Track::new(TrackType::Subtitle, "S_TEXT/UTF8"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_for_source_of_an_ordinary_size_succeeds() {
        let raw = allocate_for_source(83).unwrap();
        assert_eq!(raw.len(), 83);
        assert!(raw.iter().all(|&b| b == 0));
    }

    #[test]
    fn allocate_for_source_rejects_anything_over_the_documented_limit() {
        let err = allocate_for_source(MAX_SOURCE_BYTES + 1).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedFeature);
        assert!(err.message.contains("more than the"), "{}", err.message);
        assert!(allocate_for_source(MAX_SOURCE_BYTES).is_ok(), "exactly the limit is still accepted");
    }

    // Both lower-level guards are, in practice, dead code reachable only
    // beyond `MAX_SOURCE_BYTES`, which the check above already refuses
    // first — kept anyway as defense in depth, documented rather than
    // tested for exactly that reason:
    // - `try_reserve_exact` rejects a capacity beyond `isize::MAX` bytes
    //   immediately (Rust's own allocator-layer check); relevant only if
    //   `MAX_SOURCE_BYTES` were ever raised past that, far beyond any real
    //   subtitle file.
    // - `usize::try_from` only fails when `size` exceeds `usize::MAX`,
    //   which cannot happen on a 64-bit target (usize and u64 share range
    //   there) regardless of `MAX_SOURCE_BYTES`; only a 32-bit build, with
    //   a limit raised past ~4 GiB, would ever reach it.

    #[test]
    fn timestamp_parsing() {
        assert_eq!(parse_timestamp("00:00:01,000"), Some(1000));
        assert_eq!(parse_timestamp("01:02:03,004"), Some(3_723_004));
        assert_eq!(parse_timestamp("0:00:01,000"), Some(1000), "hours need not be zero-padded");
        assert_eq!(parse_timestamp("00:60:00,000"), None, "minutes out of range");
        assert_eq!(parse_timestamp("00:00:60,000"), None, "seconds out of range");
        assert_eq!(parse_timestamp("00:00:01.000"), None, "a period is not accepted");
        assert_eq!(parse_timestamp("00:00:01,00"), None, "milliseconds must be 3 digits");
        assert_eq!(parse_timestamp("1:00:01,000"), Some(3_601_000));
    }

    #[test]
    fn huge_hours_are_rejected_instead_of_overflowing() {
        // The hour field is otherwise unbounded, so an astronomically large
        // but syntactically valid value must fail cleanly, never panic.
        assert_eq!(parse_timestamp("153722867280912930:00:00,000"), None);
        assert_eq!(parse_timestamp("2562047788015:00:00,000"), Some(2562047788015 * 3_600_000));
        assert_eq!(parse_timestamp("2562047788016:00:00,000"), None, "one hour past what i64 ms can hold");
    }

    #[test]
    fn cue_times_ignore_position_hints() {
        assert_eq!(parse_cue_times("00:00:01,000 --> 00:00:03,500 X1:1 X2:2 Y1:3 Y2:4"), Some((1000, 3500)));
        assert_eq!(parse_cue_times("00:00:01,000-->00:00:03,500"), Some((1000, 3500)));
        assert_eq!(parse_cue_times("00:00:01,000 -> 00:00:03,500"), None, "arrow must be \"-->\"");
    }

    #[test]
    fn cp1252_round_trips_ascii_and_maps_high_bytes() {
        assert_eq!(cp1252_to_utf8(b"caf\xe9"), "café".as_bytes());
        assert_eq!(cp1252_to_utf8(b"\x93quote\x94"), "\u{201c}quote\u{201d}".as_bytes());
        assert_eq!(cp1252_to_utf8(b"\x80"), "\u{20ac}".as_bytes(), "euro sign");
    }
}
