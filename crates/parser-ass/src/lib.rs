//! SSA and ASS subtitles (`S_TEXT/SSA`, `S_TEXT/ASS`), stored as the
//! Matroska subtitle codec mapping defines it (decision 64).
//!
//! - The codec is `S_TEXT/ASS` with a `[V4+ Styles]` section and
//!   `S_TEXT/SSA` with a `[V4 Styles]` one.
//! - `codec_private` is every line of the script that is not a `Dialogue:`
//!   event, in order and with its line ending: the headers, the styles, the
//!   `[Events]` format line, `Comment:` events and any later section such as
//!   `[Fonts]`. This is what real muxers store, and nothing is lost.
//! - Each `Dialogue:` line is one unit with `random_access` and
//!   `duration_required`, timed by its Start and End (centiseconds). Its
//!   payload is `ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text`:
//!   ReadOrder counts the dialogue lines from 0, Layer is empty for SSA, and
//!   the other fields are referenced in the source (a run of fields that is
//!   contiguous there becomes one `src` chunk).
//! - Text that is not UTF-8 is transcoded as in the SRT parser and then
//!   embedded inline (decision 39).

pub mod text;

use vtj::cli::ParseError;
use vtj::*;

/// Same bound as SRT (decision 53): the whole source is scanned in memory.
const MAX_SOURCE_BYTES: u64 = 64 << 20;

/// The fields of a Matroska ASS block after ReadOrder, in order.
const BLOCK_FIELDS: [&str; 8] = ["layer", "style", "name", "marginl", "marginr", "marginv", "effect", "text"];

/// Centiseconds of `H:MM:SS.CC`.
pub fn parse_time(s: &str) -> Option<i64> {
    let (hms, cs) = s.trim().split_once('.')?;
    let mut parts = hms.split(':');
    let (h, m, sec) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || cs.len() != 2 || m.len() != 2 || sec.len() != 2 || h.is_empty() || h.len() > 9 {
        return None;
    }
    if ![h, m, sec, cs].iter().all(|p| p.bytes().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let (h, m, sec, cs): (i64, i64, i64, i64) = (h.parse().ok()?, m.parse().ok()?, sec.parse().ok()?, cs.parse().ok()?);
    if m > 59 || sec > 59 {
        return None;
    }
    Some(((h * 60 + m) * 60 + sec) * 100 + cs)
}

/// Lines as `(start, content_end, next)`: the content excludes CR/LF.
fn lines(b: &[u8]) -> Vec<(usize, usize, usize)> {
    let mut v = Vec::new();
    let mut s = 0;
    while s < b.len() {
        let mut e = s;
        while e < b.len() && b[e] != b'\n' && b[e] != b'\r' {
            e += 1;
        }
        let next = match (b.get(e), b.get(e + 1)) {
            (Some(b'\r'), Some(b'\n')) => e + 2,
            (Some(_), _) => e + 1,
            (None, _) => e,
        };
        v.push((s, e, next));
        s = next;
    }
    v
}

/// Payload pieces over the scanned text, merged as they are pushed.
#[derive(Debug, PartialEq, Eq)]
enum Piece {
    Span(usize, usize),
    Bytes(Vec<u8>),
}

#[derive(Default)]
struct Builder {
    pieces: Vec<Piece>,
}

impl Builder {
    fn bytes(&mut self, b: &[u8]) {
        match self.pieces.last_mut() {
            Some(Piece::Bytes(v)) => v.extend_from_slice(b),
            _ => self.pieces.push(Piece::Bytes(b.to_vec())),
        }
    }

    /// A comma, then a field. A field that follows the previous one in the
    /// text with exactly a comma between them extends its span instead.
    fn field(&mut self, text: &[u8], span: Option<(usize, usize)>, first: bool) {
        if let (false, Some((c, d)), Some(Piece::Span(_, b))) = (first, span, self.pieces.last_mut()) {
            if c == *b + 1 && text[*b] == b',' {
                *b = d;
                return;
            }
        }
        if !first {
            self.bytes(b",");
        }
        if let Some((c, d)) = span {
            self.pieces.push(Piece::Span(c, d));
        }
    }
}

pub struct Ass;

impl Parser for Ass {
    fn name(&self) -> &'static str {
        "vmkv-parser-ass"
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
        let mut raw = vec![0u8; size as usize];
        ctx.source(0).read_at(0, &mut raw)?;
        let text = text::decode(&raw)?;
        let b = text.bytes();
        let all = lines(b);

        let mut codec: Option<&'static str> = None;
        let mut section = String::new();
        let mut format: Option<Vec<String>> = None;
        let mut private: Vec<(usize, usize)> = Vec::new();
        let mut units: Vec<Unit> = Vec::new();
        let flags = Flags::NONE.with(Flag::RandomAccess).with(Flag::DurationRequired);
        let cs = Rational::new(100, 1);

        for &(s, e, next) in &all {
            let line = std::str::from_utf8(&b[s..e]).expect("decoded text is UTF-8");
            let trimmed = line.trim();
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                section = trimmed.to_ascii_lowercase();
                match section.as_str() {
                    "[v4+ styles]" => codec = codec.or(Some("S_TEXT/ASS")),
                    "[v4 styles]" => codec = codec.or(Some("S_TEXT/SSA")),
                    _ => {}
                }
            }
            let is_dialogue = section == "[events]" && line.starts_with("Dialogue:");
            if !is_dialogue {
                if section == "[events]" && line.starts_with("Format:") {
                    format = Some(line["Format:".len()..].split(',').map(|f| f.trim().to_ascii_lowercase()).collect());
                }
                match private.last_mut() {
                    Some((_, end)) if *end == s => *end = next,
                    _ => private.push((s, next)),
                }
                continue;
            }

            let n = units.len();
            let fields_def = format.as_ref().ok_or_else(|| {
                ParseError::new(
                    ErrorCode::MissingInitializationData,
                    format!("Dialogue line at byte {s} before the [Events] Format line"),
                )
            })?;
            // Split into as many fields as the format names; the last takes
            // the rest of the line, commas included.
            let mut spans = Vec::with_capacity(fields_def.len());
            let mut p = s + "Dialogue:".len();
            while p < e && b[p] == b' ' {
                p += 1;
            }
            for i in 0..fields_def.len() {
                if i + 1 == fields_def.len() {
                    spans.push((p, e));
                    break;
                }
                let Some(comma) = b[p..e].iter().position(|&c| c == b',') else {
                    return Err(ParseError::invalid(format!(
                        "Dialogue line at byte {s} has fewer than the {} fields of its format",
                        fields_def.len()
                    )));
                };
                spans.push((p, p + comma));
                p += comma + 1;
            }
            let get = |name: &str| fields_def.iter().position(|f| f == name).map(|i| spans[i]);
            let time = |name: &str| -> Result<i64, ParseError> {
                let (fs, fe) = get(name).ok_or_else(|| {
                    ParseError::new(
                        ErrorCode::MissingInitializationData,
                        format!("the [Events] format has no {name} field"),
                    )
                })?;
                let v = std::str::from_utf8(&b[fs..fe]).expect("UTF-8");
                parse_time(v)
                    .ok_or_else(|| ParseError::invalid(format!("dialogue {n} at byte {s} has an invalid {name} time")))
            };
            let (start, end) = (time("start")?, time("end")?);
            if end < start {
                return Err(ParseError::invalid(format!("dialogue {n} at byte {s} ends before it starts")));
            }
            if get("text").is_none() {
                return Err(ParseError::new(
                    ErrorCode::MissingInitializationData,
                    "the [Events] format has no Text field",
                ));
            }

            let mut builder = Builder::default();
            builder.bytes(format!("{n},").as_bytes());
            for (i, name) in BLOCK_FIELDS.iter().enumerate() {
                builder.field(b, get(name), i == 0);
            }
            let mut payload: Vec<Chunk> = Vec::new();
            for piece in builder.pieces {
                let chunk = match piece {
                    Piece::Span(x, y) if x == y => continue,
                    Piece::Span(x, y) => text.chunk(x, y - x),
                    Piece::Bytes(v) => Chunk::inline(v),
                };
                // Transcoded text becomes inline too: merge adjacent ones.
                match (payload.last_mut(), &chunk) {
                    (Some(Chunk::Inline(a)), Chunk::Inline(c)) => a.extend_from_slice(c),
                    _ => payload.push(chunk),
                }
            }
            let pts = ticks_to_ns(start as i128, cs)?;
            units.push(Unit::new(pts, ticks_to_ns(end as i128, cs)? - pts, flags, payload));
        }

        let codec = codec.ok_or_else(|| {
            ParseError::new(ErrorCode::MissingInitializationData, "no [V4+ Styles] or [V4 Styles] section")
        })?;
        if format.is_none() {
            return Err(ParseError::new(ErrorCode::MissingInitializationData, "no [Events] Format line"));
        }
        for u in &units {
            ctx.emit(u)?;
        }
        let mut track = Track::new(TrackType::Subtitle, codec);
        let mut cp: Vec<Chunk> = Vec::new();
        for (s, e) in private {
            let c = text.chunk(s, e - s);
            match (cp.last_mut(), &c) {
                (Some(Chunk::Inline(a)), Chunk::Inline(x)) => a.extend_from_slice(x),
                _ => cp.push(c),
            }
        }
        track.codec_private = Some(cp);
        Ok(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_time("0:02:40.65"), Some(16065));
        assert_eq!(parse_time(" 10:00:00.00"), Some(3_600_000));
        assert_eq!(parse_time("0:02:40.650"), None);
        assert_eq!(parse_time("0:2:40.65"), None);
        assert_eq!(parse_time("0:02:60.00"), None);
        assert_eq!(parse_time("0:02:40,65"), None);
    }

    #[test]
    fn contiguous_fields_merge() {
        let t = b"a,b,c";
        let mut bld = Builder::default();
        bld.bytes(b"7,");
        bld.field(t, Some((0, 1)), true);
        bld.field(t, None, false);
        bld.field(t, Some((2, 3)), false);
        bld.field(t, Some((4, 5)), false);
        assert_eq!(
            bld.pieces,
            [Piece::Bytes(b"7,".to_vec()), Piece::Span(0, 1), Piece::Bytes(b",,".to_vec()), Piece::Span(2, 5)]
        );
    }
}
