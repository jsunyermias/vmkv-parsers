//! Text decoding, copied from the SRT parser (decision 30: parsers share
//! no logic): a UTF-8 source is referenced as it is; UTF-16 with a BOM and
//! any other non-UTF-8 source (taken as Windows-1252) are transcoded once
//! to UTF-8 and embedded inline.

use vtj::cli::ParseError;
use vtj::Chunk;

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
pub enum Text<'a> {
    /// Valid UTF-8 bytes of the source itself, `base` bytes in (past a UTF-8
    /// BOM, if any). Cue payloads reference the source directly.
    Source { data: &'a [u8], base: u64 },
    /// Transcoded to UTF-8; cue payloads embed their text directly.
    Transcoded(Vec<u8>),
}

impl Text<'_> {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Text::Source { data, .. } => data,
            Text::Transcoded(v) => v,
        }
    }

    /// `len` bytes at `start` of the scanned text, as a chunk.
    pub fn chunk(&self, start: usize, len: usize) -> Chunk {
        match self {
            Text::Source { base, .. } => Chunk::src(0, base + start as u64, len as u64),
            Text::Transcoded(v) => Chunk::inline(v[start..start + len].to_vec()),
        }
    }
}

pub fn decode(raw: &[u8]) -> Result<Text<'_>, ParseError> {
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
