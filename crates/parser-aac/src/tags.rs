//! Metadata tags wrapped around the elementary stream.
//!
//! These tags are not audio: the parser skips them and describes only the
//! range between them. Other parsers keep their own copy on purpose: parsers
//! share no codec or container logic, so a change here can only change this
//! parser's output, which is versioned by this crate.
//!
//! - Leading: ID3v2 (repeated tags are skipped too).
//! - Trailing, in any order: ID3v1 (`TAG`, 128 bytes), APEv2 (`APETAGEX`
//!   footer) and Lyrics3v2 (`LYRICS200`).

use vtj::cli::ParseError;
use vtj::source::SourceFile;

/// Byte range `[start, end)` of a source that remains after removing tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioRange {
    pub start: u64,
    pub end: u64,
}

fn read(src: &mut SourceFile, offset: u64, len: usize) -> Result<Vec<u8>, ParseError> {
    let mut buf = vec![0u8; len];
    src.read_at(offset, &mut buf)?;
    Ok(buf)
}

fn syncsafe(b: &[u8]) -> Option<u64> {
    b.iter().try_fold(0u64, |acc, &x| (x < 0x80).then_some((acc << 7) | x as u64))
}

/// Size of the ID3v2 tag at `offset`, header and footer included, if any.
fn id3v2_at(src: &mut SourceFile, offset: u64, end: u64) -> Result<Option<u64>, ParseError> {
    if end - offset < 10 {
        return Ok(None);
    }
    let h = read(src, offset, 10)?;
    if &h[..3] != b"ID3" || h[3] == 0xff || h[4] == 0xff {
        return Ok(None);
    }
    let size =
        syncsafe(&h[6..10]).ok_or_else(|| ParseError::invalid(format!("malformed ID3v2 size at byte {offset}")))?;
    let footer = if h[3] >= 4 && h[5] & 0x10 != 0 { 10 } else { 0 };
    let total = 10 + size + footer;
    if total > end - offset {
        return Err(ParseError::truncated(format!("ID3v2 tag at byte {offset} extends past the end of the file")));
    }
    Ok(Some(total))
}

/// Finds the audio range of `src` by skipping leading and trailing tags.
pub fn audio_range(src: &mut SourceFile) -> Result<AudioRange, ParseError> {
    let size = src.size();
    let mut start = 0;
    while let Some(n) = id3v2_at(src, start, size)? {
        start += n;
    }
    let mut end = size;
    loop {
        let avail = end - start;
        if avail >= 128 && read(src, end - 128, 3)? == b"TAG" {
            end -= 128;
            continue;
        }
        if avail >= 32 {
            let f = read(src, end - 32, 32)?;
            if &f[..8] == b"APETAGEX" {
                let tag = u32::from_le_bytes(f[12..16].try_into().expect("4 bytes")) as u64;
                let flags = u32::from_le_bytes(f[20..24].try_into().expect("4 bytes"));
                let total = tag + if flags & 0x8000_0000 != 0 { 32 } else { 0 };
                if tag < 32 || total > avail {
                    return Err(ParseError::invalid(format!("malformed APEv2 tag ending at byte {end}")));
                }
                end -= total;
                continue;
            }
        }
        if avail >= 15 {
            let f = read(src, end - 15, 15)?;
            if &f[6..] == b"LYRICS200" {
                let body = std::str::from_utf8(&f[..6]).ok().and_then(|s| s.parse::<u64>().ok());
                let total = body.map(|b| b + 15).filter(|t| *t <= avail);
                let Some(total) = total else {
                    return Err(ParseError::invalid(format!("malformed Lyrics3v2 tag ending at byte {end}")));
                };
                if read(src, end - total, 11)? != b"LYRICSBEGIN" {
                    return Err(ParseError::invalid(format!("malformed Lyrics3v2 tag ending at byte {end}")));
                }
                end -= total;
                continue;
            }
        }
        break;
    }
    Ok(AudioRange { start, end })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(bytes: &[u8]) -> Result<AudioRange, ParseError> {
        let dir = std::env::temp_dir().join(format!("{}-tags-{}", env!("CARGO_PKG_NAME"), std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{}.bin", bytes.len()));
        std::fs::write(&p, bytes).unwrap();
        audio_range(&mut SourceFile::open(0, &p).unwrap())
    }

    fn id3v2(body: usize, footer: bool) -> Vec<u8> {
        let s = body as u32;
        let mut v = vec![b'I', b'D', b'3', 4, 0, if footer { 0x10 } else { 0 }];
        v.extend([(s >> 21) as u8 & 0x7f, (s >> 14) as u8 & 0x7f, (s >> 7) as u8 & 0x7f, s as u8 & 0x7f]);
        v.extend(vec![0u8; body + if footer { 10 } else { 0 }]);
        v
    }

    fn ape(items: usize, header: bool) -> Vec<u8> {
        let tag = (items + 32) as u32;
        let mut v = Vec::new();
        if header {
            v.extend(b"APETAGEX");
            v.extend([0u8; 24]);
        }
        v.extend(vec![b'x'; items]);
        v.extend(b"APETAGEX");
        v.extend(2000u32.to_le_bytes());
        v.extend(tag.to_le_bytes());
        v.extend(1u32.to_le_bytes());
        v.extend((if header { 0x8000_0000u32 } else { 0 }).to_le_bytes());
        v.extend([0u8; 8]);
        v
    }

    #[test]
    fn strips_all_tag_kinds() {
        let mut f = id3v2(300, false);
        f.extend(id3v2(20, true));
        let start = f.len() as u64;
        f.extend(vec![0xffu8; 1000]);
        let end = f.len() as u64;
        f.extend(ape(40, true));
        f.extend(b"LYRICSBEGIN");
        f.extend(b"abc");
        f.extend(b"000014LYRICS200");
        let mut v1 = b"TAG".to_vec();
        v1.extend([b' '; 125]);
        f.extend(v1);
        assert_eq!(range(&f).unwrap(), AudioRange { start, end });
    }

    #[test]
    fn untagged_and_errors() {
        assert_eq!(range(&[0xff; 50]).unwrap(), AudioRange { start: 0, end: 50 });
        let mut bad = id3v2(10, false);
        bad[6] = 0x80;
        assert_eq!(range(&bad).unwrap_err().code, vtj::ErrorCode::InvalidBitstream);
        let cut = id3v2(100, false)[..50].to_vec();
        assert_eq!(range(&cut).unwrap_err().code, vtj::ErrorCode::TruncatedBitstream);
    }
}
