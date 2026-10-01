//! WebVTT subtitles (`S_TEXT/WEBVTT`), stored as the Matroska subtitle
//! codec mapping defines it (decision 63).
//!
//! - `codec_private` is every global block before the first cue: the
//!   `WEBVTT` line and its header text, and any STYLE, REGION and NOTE
//!   blocks, without a BOM and without the blank lines after them.
//! - Each cue is one unit: its text, without the final line terminator,
//!   with `random_access` and `duration_required`. Timestamps inside the
//!   text (`<00:01:02.500>`) are rewritten relative to the cue's start, as
//!   the mapping requires; everything else is referenced in the source, line
//!   endings included.
//! - The cue settings, the cue identifier and the NOTE blocks since the
//!   previous cue go into block addition 1: the settings and a line feed,
//!   the identifier and a line feed, then each comment followed by a line
//!   feed. The addition is omitted when all three are absent.
//! - The source must be UTF-8 (WebVTT allows nothing else). Rejected: a
//!   STYLE or REGION block after the first cue, a malformed timing line, an
//!   end before its start, and an inner timestamp outside its cue.
//!   NOTE blocks after the last cue have no cue to travel with and are not
//!   carried.

use vtj::cli::ParseError;
use vtj::*;

/// Same bound as SRT (decision 53): the whole source is scanned in memory.
const MAX_SOURCE_BYTES: u64 = 64 << 20;

/// A line: `[start, end)` without its terminator.
#[derive(Debug, Clone, Copy)]
struct Line {
    start: usize,
    end: usize,
}

fn split_lines(b: &[u8], from: usize) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut start = from;
    let mut i = from;
    while i < b.len() {
        match b[i] {
            b'\n' => {
                lines.push(Line { start, end: i });
                i += 1;
                start = i;
            }
            b'\r' => {
                let next = if b.get(i + 1) == Some(&b'\n') { i + 2 } else { i + 1 };
                lines.push(Line { start, end: i });
                i = next;
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < b.len() {
        lines.push(Line { start, end: b.len() });
    }
    lines
}

/// Milliseconds of a WebVTT timestamp: `[hh+:]mm:ss.ttt`, hours at least two
/// digits when present, minutes and seconds below 60.
pub fn parse_timestamp(s: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(s).ok()?;
    let (hms, ms) = s.split_once('.')?;
    if ms.len() != 3 || !ms.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let parts: Vec<&str> = hms.split(':').collect();
    let (h, m, sec) = match parts[..] {
        [m, s] => ("0", m, s),
        [h, m, s] if h.len() >= 2 => (h, m, s),
        _ => return None,
    };
    let digits = |x: &str, len: Option<usize>| {
        x.bytes().all(|c| c.is_ascii_digit()) && !x.is_empty() && len.is_none_or(|l| x.len() == l)
    };
    if !digits(h, None) || !digits(m, Some(2)) || !digits(sec, Some(2)) || h.len() > 10 {
        return None;
    }
    let (h, m, sec, ms): (i64, i64, i64, i64) = (h.parse().ok()?, m.parse().ok()?, sec.parse().ok()?, ms.parse().ok()?);
    if m > 59 || sec > 59 {
        return None;
    }
    Some(((h * 60 + m) * 60 + sec) * 1000 + ms)
}

fn format_timestamp(ms: i64) -> String {
    format!("{:02}:{:02}:{:02}.{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

fn is_ws(c: u8) -> bool {
    c == b' ' || c == b'\t'
}

fn trim(b: &[u8], mut s: usize, mut e: usize) -> (usize, usize) {
    while s < e && is_ws(b[s]) {
        s += 1;
    }
    while e > s && is_ws(b[e - 1]) {
        e -= 1;
    }
    (s, e)
}

fn contains_arrow(b: &[u8], l: &Line) -> bool {
    b[l.start..l.end].windows(3).any(|w| w == b"-->")
}

/// Whether a block starting with line `l` begins with `keyword` followed by
/// a space, a tab or the end of the line.
fn starts_with_keyword(b: &[u8], l: &Line, keyword: &[u8]) -> bool {
    let s = &b[l.start..l.end];
    s.starts_with(keyword) && s.get(keyword.len()).is_none_or(|&c| is_ws(c))
}

struct Timing {
    start: i64,
    end: i64,
    /// Cue settings, `[s, e)`, possibly empty.
    settings: (usize, usize),
}

fn parse_timing(b: &[u8], l: &Line) -> Result<Timing, String> {
    let s = &b[l.start..l.end];
    let arrow = s.windows(3).position(|w| w == b"-->").ok_or("no -->")?;
    let (a0, a1) = trim(b, l.start, l.start + arrow);
    let start = parse_timestamp(&b[a0..a1]).ok_or("invalid start timestamp")?;
    let mut p = l.start + arrow + 3;
    if p < l.end && !is_ws(b[p]) {
        return Err("no whitespace after -->".into());
    }
    while p < l.end && is_ws(b[p]) {
        p += 1;
    }
    let mut q = p;
    while q < l.end && !is_ws(b[q]) {
        q += 1;
    }
    let end = parse_timestamp(&b[p..q]).ok_or("invalid end timestamp")?;
    if a1 == l.start + arrow {
        // "-->" must be preceded by whitespace too.
        if arrow == 0 || !is_ws(s[arrow - 1]) {
            return Err("no whitespace before -->".into());
        }
    }
    let settings = trim(b, q, l.end);
    Ok(Timing { start, end, settings })
}

/// The cue text `[s, e)` as a data chain, with every inner timestamp
/// `<…>` rewritten relative to `start`.
fn cue_payload(b: &[u8], s: usize, e: usize, start: i64, end: i64) -> Result<Vec<Chunk>, String> {
    let mut chain = Vec::new();
    let mut from = s;
    let mut i = s;
    while i < e {
        if b[i] == b'<' {
            if let Some(close) = b[i + 1..e].iter().position(|&c| c == b'>') {
                let inner = &b[i + 1..i + 1 + close];
                if inner.first().is_some_and(u8::is_ascii_digit) {
                    if let Some(t) = parse_timestamp(inner) {
                        if t < start || t > end {
                            return Err(format!("inner timestamp {} is outside the cue", format_timestamp(t)));
                        }
                        if i + 1 > from {
                            chain.push(Chunk::src(0, from as u64, (i + 1 - from) as u64));
                        }
                        chain.push(Chunk::inline(format_timestamp(t - start).into_bytes()));
                        from = i + 1 + close;
                        i = from;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    if e > from {
        chain.push(Chunk::src(0, from as u64, (e - from) as u64));
    }
    Ok(chain)
}

pub struct WebVtt;

impl Parser for WebVtt {
    fn name(&self) -> &'static str {
        "vmkv-parser-webvtt"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let size = ctx.source(0).size();
        if size > MAX_SOURCE_BYTES {
            return Err(ParseError::unsupported(format!(
                "source is {size} bytes, more than the {MAX_SOURCE_BYTES}-byte limit for a subtitle file"
            )));
        }
        let mut b = vec![0u8; size as usize];
        ctx.source(0).read_at(0, &mut b)?;
        if let Err(e) = std::str::from_utf8(&b) {
            return Err(ParseError::invalid(format!("not UTF-8 at byte {}", e.valid_up_to())));
        }
        let bom = if b.starts_with(b"\xef\xbb\xbf") { 3 } else { 0 };
        let lines = split_lines(&b, bom);
        let signature = lines.first().filter(|l| starts_with_keyword(&b, l, b"WEBVTT") && !contains_arrow(&b, l));
        if signature.is_none() {
            return Err(ParseError::new(ErrorCode::MissingInitializationData, "no WEBVTT signature"));
        }

        // Blocks: runs of non-empty lines.
        let mut blocks: Vec<&[Line]> = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            if lines[i].start == lines[i].end {
                i += 1;
                continue;
            }
            let s = i;
            // A line containing "-->" starts a new cue even without a blank
            // line before it, unless it is the block's own timing line.
            while i < lines.len() && lines[i].start != lines[i].end {
                let own_timing = i == s || (i == s + 1 && !contains_arrow(&b, &lines[s]));
                if i > s && !own_timing && contains_arrow(&b, &lines[i]) {
                    break;
                }
                i += 1;
            }
            blocks.push(&lines[s..i]);
        }

        if blocks[0].iter().any(|l| contains_arrow(&b, l)) {
            return Err(ParseError::invalid("the WEBVTT header block contains a cue timing line"));
        }
        let flags = Flags::NONE.with(Flag::RandomAccess).with(Flag::DurationRequired);
        let ms = Rational::new(1000, 1);
        let mut header_end = blocks[0].last().expect("non-empty block").end;
        let mut seen_cue = false;
        let mut comments: Vec<(usize, usize)> = Vec::new();
        let mut count = 0u64;

        for block in &blocks[1..] {
            let first = &block[0];
            let is_cue = contains_arrow(&b, first) || block.get(1).is_some_and(|l| contains_arrow(&b, l));
            if !is_cue {
                if starts_with_keyword(&b, first, b"NOTE") {
                    if seen_cue {
                        comments.push((first.start, block.last().expect("non-empty").end));
                    } else {
                        header_end = block.last().expect("non-empty").end;
                    }
                    continue;
                }
                if starts_with_keyword(&b, first, b"STYLE") || starts_with_keyword(&b, first, b"REGION") {
                    if seen_cue {
                        return Err(ParseError::invalid(format!(
                            "{} block at byte {} after the first cue",
                            if b[first.start] == b'S' { "STYLE" } else { "REGION" },
                            first.start
                        )));
                    }
                    header_end = block.last().expect("non-empty").end;
                    continue;
                }
                return Err(ParseError::invalid(format!(
                    "block at byte {} is not a cue, NOTE, STYLE or REGION",
                    first.start
                )));
            }

            seen_cue = true;
            let (id, timing_line) = if contains_arrow(&b, first) { (None, first) } else { (Some(first), &block[1]) };
            let t = parse_timing(&b, timing_line).map_err(|e| {
                ParseError::invalid(format!("cue {count} timing line at byte {}: {e}", timing_line.start))
            })?;
            if t.end < t.start {
                return Err(ParseError::invalid(format!(
                    "cue {count} at byte {} ends before it starts",
                    timing_line.start
                )));
            }
            let text_lines = &block[if id.is_some() { 2 } else { 1 }..];
            let payload = match (text_lines.first(), text_lines.last()) {
                (Some(f), Some(l)) => cue_payload(&b, f.start, l.end, t.start, t.end)
                    .map_err(|e| ParseError::invalid(format!("cue {count} at byte {}: {e}", timing_line.start)))?,
                _ => Vec::new(),
            };

            let mut unit = Unit::new(
                ticks_to_ns(t.start as i128, ms)?,
                ticks_to_ns(t.end as i128, ms)? - ticks_to_ns(t.start as i128, ms)?,
                flags,
                payload,
            );
            let (ss, se) = t.settings;
            if ss < se || id.is_some() || !comments.is_empty() {
                let mut data: Vec<Chunk> = Vec::new();
                // Consecutive line feeds become one inline chunk.
                let lf = |data: &mut Vec<Chunk>| match data.last_mut() {
                    Some(Chunk::Inline(v)) => v.push(b'\n'),
                    _ => data.push(Chunk::inline(b"\n".to_vec())),
                };
                if ss < se {
                    data.push(Chunk::src(0, ss as u64, (se - ss) as u64));
                }
                lf(&mut data);
                if let Some(l) = id {
                    data.push(Chunk::src(0, l.start as u64, (l.end - l.start) as u64));
                }
                lf(&mut data);
                for &(cs, ce) in &comments {
                    data.push(Chunk::src(0, cs as u64, (ce - cs) as u64));
                    lf(&mut data);
                }
                unit.block_additions.push(BlockAddition { id: 1, data });
                comments.clear();
            }
            ctx.emit(&unit)?;
            count += 1;
        }

        let mut track = Track::new(TrackType::Subtitle, "S_TEXT/WEBVTT");
        track.codec_private = Some(vec![Chunk::src(0, bom as u64, (header_end - bom) as u64)]);
        Ok(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        assert_eq!(parse_timestamp(b"00:01.500"), Some(1500));
        assert_eq!(parse_timestamp(b"01:02:03.004"), Some(3_723_004));
        assert_eq!(parse_timestamp(b"100:00:00.000"), Some(360_000_000));
        assert_eq!(parse_timestamp(b"1:02:03.004"), None, "hours need two digits");
        assert_eq!(parse_timestamp(b"00:60.000"), None);
        assert_eq!(parse_timestamp(b"00:01,500"), None);
        assert_eq!(parse_timestamp(b"00:01.50"), None);
        assert_eq!(format_timestamp(3_723_004), "01:02:03.004");
    }

    #[test]
    fn inner_timestamps_are_rewritten() {
        let t = b"a<00:00:05.250>b<c.x>d<00:06.000>";
        let chain = cue_payload(t, 0, t.len(), 5000, 6000).unwrap();
        assert_eq!(
            chain,
            vec![
                Chunk::src(0, 0, 2),
                Chunk::inline(b"00:00:00.250".to_vec()),
                Chunk::src(0, 14, 9),
                Chunk::inline(b"00:00:01.000".to_vec()),
                Chunk::src(0, 32, 1),
            ]
        );
        assert!(cue_payload(t, 0, t.len(), 5500, 6000).unwrap_err().contains("outside the cue"));
    }

    #[test]
    fn line_terminators() {
        let l = split_lines(b"a\r\nb\rc\n\nd", 0);
        let spans: Vec<_> = l.iter().map(|l| (l.start, l.end)).collect();
        assert_eq!(spans, [(0, 1), (3, 4), (5, 6), (7, 7), (8, 9)]);
    }
}
