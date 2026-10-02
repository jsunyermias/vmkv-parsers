//! VobSub subtitles (`S_VOBSUB`) from an `.idx` + `.sub` pair: source 0 is
//! the `.idx`, source 1 the `.sub` (decision 72).
//!
//! - Only `.idx` version 7 or later, which the Matroska mapping requires.
//! - One `.idx` can hold several language streams; each is its own track.
//!   With more than one, `--stream-index` chooses which to describe.
//! - `codec_private` is the `.idx` with its comments, blank lines and the
//!   `langidx`, `id`, `alt`, `delay` and `timestamp` lines removed: the
//!   setting lines before the first stream, referenced in the source.
//! - `delay` lines add up within their stream and restart at every `id`
//!   line, as in VSFilter (the format's reference implementation; FFmpeg
//!   carries them across streams instead).
//! - Each `timestamp` line of the stream is one unit: the subpicture unit
//!   (SPU) found from its `filepos` in the `.sub` MPEG program stream,
//!   gathered from the payloads of the stream's `private_stream_1` PES
//!   packets (substream `0x20 + index`), one `src` chunk per packet. Its time
//!   is the timestamp plus the stream's `delay` lines so far; its duration
//!   is the date of the SPU's stop-display command, or unknown without one.
//!   Every unit is a random access point.

use vtj::cli::{ParamSpec, ParseError};
use vtj::source::SourceFile;
use vtj::*;

const IDX_MAX_BYTES: u64 = 16 << 20;
/// Packs scanned from a `filepos` for the start of its SPU, and for each of
/// its continuations, before giving up.
const MAX_PACKETS_PER_SPU: usize = 256;

const STREAM_INDEX: ParamSpec =
    ParamSpec::int("stream_index", 0, 31, "which stream of the .idx to describe (its `index:`)")
        .default("the only one");

/// Milliseconds of `[+-]hh:mm:ss:ms`.
pub fn parse_time(s: &str) -> Option<i64> {
    let s = s.trim();
    let (sign, s) = match s.strip_prefix('-') {
        Some(rest) => (-1, rest),
        None => (1, s.strip_prefix('+').unwrap_or(s)),
    };
    let parts: Vec<&str> = s.split(':').collect();
    let [h, m, sec, ms] = parts[..] else { return None };
    if [h, m, sec, ms].iter().any(|p| p.is_empty() || p.len() > 9 || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let (h, m, sec, ms): (i64, i64, i64, i64) = (h.parse().ok()?, m.parse().ok()?, sec.parse().ok()?, ms.parse().ok()?);
    if m > 59 || sec > 59 || ms > 999 {
        return None;
    }
    Some(sign * (((h * 60 + m) * 60 + sec) * 1000 + ms))
}

/// One language stream of the `.idx`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Stream {
    pub index: u32,
    /// `(milliseconds, delay included; filepos)`.
    pub events: Vec<(i64, u64)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Idx {
    /// `[start, end)` byte spans of the setting lines kept for
    /// `codec_private`, line endings included.
    pub header: Vec<(u64, u64)>,
    pub streams: Vec<Stream>,
}

/// Parses the `.idx` text.
pub fn parse_idx(b: &[u8]) -> Result<Idx, ParseError> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    while start < b.len() {
        let end = b[start..].iter().position(|&c| c == b'\n').map_or(b.len(), |i| start + i + 1);
        lines.push((start, end));
        start = end;
    }
    let text = |(s, e): (usize, usize)| String::from_utf8_lossy(&b[s..e]).trim_end_matches(['\n', '\r']).to_string();
    let first = lines.first().map(|&l| text(l)).unwrap_or_default();
    let version = first
        .split_once("VobSub index file, v")
        .and_then(|(_, v)| v.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or_else(|| ParseError::new(ErrorCode::MissingInitializationData, "not a VobSub index file"))?;
    if version < 7 {
        return Err(ParseError::new(
            ErrorCode::UnsupportedCodecVariant,
            format!("VobSub index version {version}; the Matroska mapping needs version 7 or later"),
        ));
    }

    let mut header = Vec::new();
    let mut streams: Vec<Stream> = Vec::new();
    let mut delay = 0i64;
    for (n, &(s, e)) in lines.iter().enumerate() {
        let line = text((s, e));
        let t = line.trim_start();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let (key, value) =
            t.split_once(':').map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim())).unwrap_or_default();
        match key.as_str() {
            "id" => {
                let index = value
                    .split_once("index:")
                    .and_then(|(_, i)| i.trim().parse::<u32>().ok())
                    .ok_or_else(|| ParseError::invalid(format!("line {}: malformed id line", n + 1)))?;
                if streams.iter().any(|s| s.index == index) {
                    return Err(ParseError::invalid(format!("line {}: stream index {index} declared twice", n + 1)));
                }
                streams.push(Stream { index, events: Vec::new() });
                delay = 0;
            }
            "timestamp" => {
                let (time, pos) = value
                    .split_once(", filepos:")
                    .ok_or_else(|| ParseError::invalid(format!("line {}: malformed timestamp line", n + 1)))?;
                let ms = parse_time(time)
                    .ok_or_else(|| ParseError::invalid(format!("line {}: invalid timestamp", n + 1)))?;
                let pos = u64::from_str_radix(pos.trim(), 16)
                    .map_err(|_| ParseError::invalid(format!("line {}: invalid filepos", n + 1)))?;
                let stream = streams
                    .last_mut()
                    .ok_or_else(|| ParseError::invalid(format!("line {}: timestamp before any id line", n + 1)))?;
                let mut t = ms + delay;
                // As VSFilter: a negative delay never takes a subpicture
                // before the previous one of its stream; the difference is
                // absorbed into the delay.
                if let Some(&(prev, _)) = stream.events.last() {
                    if delay < 0 && t < prev {
                        delay += prev - t;
                        t = prev;
                    }
                }
                stream.events.push((t, pos));
            }
            "delay" => {
                delay +=
                    parse_time(value).ok_or_else(|| ParseError::invalid(format!("line {}: invalid delay", n + 1)))?;
            }
            "langidx" | "alt" => {}
            _ if streams.is_empty() => match header.last_mut() {
                // Consecutive kept lines form one span.
                Some((_, end)) if *end == s as u64 => *end = e as u64,
                _ => header.push((s as u64, e as u64)),
            },
            // Anything else after the first stream is not a setting the
            // decoder needs.
            _ => {}
        }
    }
    Ok(Idx { header, streams })
}

/// Duration of an SPU in 1/90000 s ticks: the date of its first
/// stop-display command, if it has one.
pub fn stop_ticks(spu: &[u8]) -> Result<Option<i128>, String> {
    if spu.len() < 4 {
        return Err("subpicture unit shorter than its header".into());
    }
    let mut off = u16::from_be_bytes([spu[2], spu[3]]) as usize;
    for _ in 0..1024 {
        let head = spu.get(off..off + 4).ok_or("control sequence out of bounds")?;
        let date = u16::from_be_bytes([head[0], head[1]]) as i128;
        let next = u16::from_be_bytes([head[2], head[3]]) as usize;
        let mut p = off + 4;
        loop {
            let cmd = *spu.get(p).ok_or("control sequence runs past the subpicture unit")?;
            p += 1;
            match cmd {
                0x00 | 0x01 => {}
                0x02 => return Ok(Some(date * 1024)),
                0x03 | 0x04 => p += 2,
                0x05 => p += 6,
                0x06 => p += 4,
                0x07 => {
                    let size = spu.get(p..p + 2).ok_or("control sequence out of bounds")?;
                    p += u16::from_be_bytes([size[0], size[1]]) as usize;
                }
                0xff => break,
                c => return Err(format!("unknown control command 0x{c:02x}")),
            }
        }
        if next == off {
            return Ok(None);
        }
        off = next;
    }
    Err("too many control sequences".into())
}

/// The SPU of substream `sub_id` starting at or after `pos` in the
/// program stream: its total size and its `(offset, length)` pieces.
fn read_spu(src: &mut SourceFile, mut pos: u64, sub_id: u8) -> Result<Vec<(u64, u64)>, String> {
    let size = src.size();
    let mut pieces: Vec<(u64, u64)> = Vec::new();
    let mut have = 0u64;
    let mut need: Option<u64> = None;
    let mut scanned = 0usize;
    loop {
        if need.is_some_and(|n| have >= n) {
            return Ok(pieces);
        }
        if scanned > MAX_PACKETS_PER_SPU {
            return Err("subpicture unit not complete within 256 packets".into());
        }
        scanned += 1;
        if size - pos.min(size) < 6 {
            return Err(format!("program stream ends at byte {size} inside a subpicture unit"));
        }
        let mut h = [0u8; 14];
        let n = (size - pos).min(14) as usize;
        src.read_at(pos, &mut h[..n]).map_err(|_| "source read failed".to_string())?;
        if h[..3] != [0, 0, 1] {
            return Err(format!("no program stream start code at byte {pos}"));
        }
        match h[3] {
            0xba => {
                if n < 14 {
                    return Err(format!("pack header at byte {pos} cut short"));
                }
                pos += match h[4] {
                    b if b & 0xc0 == 0x40 => 14 + (h[13] & 7) as u64,
                    b if b & 0xf0 == 0x20 => 12,
                    _ => return Err(format!("unknown pack header at byte {pos}")),
                };
            }
            0xb9 => pos += 4,
            id if id >= 0xb9 => {
                let len = u16::from_be_bytes([h[4], h[5]]) as u64;
                let end = pos + 6 + len;
                if end > size {
                    return Err(format!("PES packet at byte {pos} cut at byte {size}"));
                }
                if id == 0xbd {
                    if h[6] & 0xc0 != 0x80 {
                        return Err(format!("private stream PES at byte {pos} is not MPEG-2"));
                    }
                    let data = pos + 9 + h[8] as u64;
                    if data + 1 > end {
                        return Err(format!("private stream PES at byte {pos} has no substream id"));
                    }
                    let mut sid = [0u8];
                    src.read_at(data, &mut sid).map_err(|_| "source read failed".to_string())?;
                    if sid[0] == sub_id {
                        let (start, mut len) = (data + 1, end - data - 1);
                        if need.is_none() {
                            if len < 2 {
                                return Err(format!("SPU at byte {start} has no size field"));
                            }
                            let mut s = [0u8; 2];
                            src.read_at(start, &mut s).map_err(|_| "source read failed".to_string())?;
                            need = Some(u16::from_be_bytes(s) as u64);
                        }
                        // The SPU's own size ends it; trailing bytes of the
                        // packet past it are not part of the unit.
                        len = len.min(need.expect("set above") - have);
                        if len > 0 {
                            pieces.push((start, len));
                            have += len;
                        }
                    }
                }
                pos = end;
            }
            id => return Err(format!("unexpected start code 0x{id:02x} at byte {pos}")),
        }
    }
}

pub struct VobSub;

impl Parser for VobSub {
    fn name(&self) -> &'static str {
        "vmkv-parser-vobsub"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn params(&self) -> &'static [ParamSpec] {
        &[STREAM_INDEX]
    }

    fn inputs(&self) -> (usize, usize) {
        (2, 2)
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let chosen = ctx.param_int("stream_index");
        let idx_size = ctx.source(0).size();
        if idx_size > IDX_MAX_BYTES {
            return Err(ParseError::unsupported(format!("the .idx is {idx_size} bytes, more than {IDX_MAX_BYTES}")));
        }
        let mut text = vec![0u8; idx_size as usize];
        ctx.source(0).read_at(0, &mut text)?;
        let idx = parse_idx(&text)?;
        let stream = match chosen {
            Some(i) => idx
                .streams
                .iter()
                .find(|s| s.index as i64 == i)
                .ok_or_else(|| ParseError::invalid(format!("the .idx has no stream with index {i}")))?,
            None => match idx.streams.as_slice() {
                [only] => only,
                [] => return Err(ParseError::invalid("the .idx declares no stream")),
                many => {
                    let list: Vec<String> = many.iter().map(|s| s.index.to_string()).collect();
                    return Err(ParseError::unsupported(format!(
                        "the .idx declares {} streams ({}); choose one with --stream-index",
                        many.len(),
                        list.join(", ")
                    )));
                }
            },
        };
        let sub_id = 0x20 + stream.index as u8;
        let ms = Rational::new(1000, 1);
        let ticks = Rational::new(90000, 1);
        let flags = Flags::NONE.with(Flag::RandomAccess);
        for (n, &(time, filepos)) in stream.events.iter().enumerate() {
            let pieces = read_spu(ctx.source(1), filepos, sub_id)
                .map_err(|e| ParseError::invalid(format!("subpicture {n} at filepos 0x{filepos:x}: {e}")))?;
            let mut spu = Vec::new();
            for &(o, l) in &pieces {
                let mut b = vec![0u8; l as usize];
                ctx.source(1).read_at(o, &mut b)?;
                spu.extend(b);
            }
            let duration = match stop_ticks(&spu).map_err(|e| ParseError::invalid(format!("subpicture {n}: {e}")))? {
                Some(t) => ticks_to_ns(t, ticks)?,
                None => -1,
            };
            let payload = pieces.iter().map(|&(o, l)| Chunk::src(1, o, l)).collect();
            ctx.emit(&Unit::new(ticks_to_ns(time as i128, ms)?, duration, flags, payload))?;
        }
        let mut track = Track::new(TrackType::Subtitle, "S_VOBSUB");
        track.codec_private = Some(idx.header.iter().map(|&(s, e)| Chunk::src(0, s, e - s)).collect());
        Ok(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_time("00:00:36:036"), Some(36036));
        assert_eq!(parse_time("-00:00:01:500"), Some(-1500));
        assert_eq!(parse_time("+01:02:03:004"), Some(3_723_004));
        assert_eq!(parse_time("00:60:00:000"), None);
        assert_eq!(parse_time("00:00:00"), None);
    }

    #[test]
    fn idx_structure() {
        let t = b"# VobSub index file, v7 (do not modify this line!)\n#\nsize: 720x480\n\npalette: 000000, ffffff\nlangidx: 0\nid: en, index: 0\ntimestamp: 00:00:01:101, filepos: 000000000\ndelay: 00:00:01:000\ntimestamp: 00:00:08:708, filepos: 000001000\nid: es, index: 1\ntimestamp: 00:00:02:000, filepos: 000002000\n";
        let idx = parse_idx(t).unwrap();
        let header: Vec<&[u8]> = idx.header.iter().map(|&(s, e)| &t[s as usize..e as usize]).collect();
        assert_eq!(header, [&b"size: 720x480\n"[..], b"palette: 000000, ffffff\n"], "a blank line splits the span");
        assert_eq!(idx.streams.len(), 2);
        assert_eq!(idx.streams[0].events, [(1101, 0), (9708, 0x1000)], "the delay applies to what follows it");
        assert_eq!(idx.streams[1].events, [(2000, 0x2000)], "and not to the next stream");
        // A negative delay that would go back in time is absorbed.
        let back = b"# VobSub index file, v7\nid: en, index: 0\ntimestamp: 00:00:05:000, filepos: 0\ndelay: -00:00:02:000\ntimestamp: 00:00:06:000, filepos: 800\ntimestamp: 00:00:09:000, filepos: 1000\n";
        assert_eq!(parse_idx(back).unwrap().streams[0].events, [(5000, 0), (5000, 0x800), (8000, 0x1000)]);
        let v6 = parse_idx(b"# VobSub index file, v6\n").unwrap_err();
        assert_eq!(v6.code, ErrorCode::UnsupportedCodecVariant);
    }

    #[test]
    fn spu_stop_date() {
        // size 16, control at 4: date 0 (start display) -> next 10; date
        // 0x90 (stop display) -> next 10 (last).
        let spu = [0, 16, 0, 4, 0x00, 0x00, 0x00, 0x0a, 0x01, 0xff, 0x00, 0x90, 0x00, 0x0a, 0x02, 0xff];
        assert_eq!(stop_ticks(&spu), Ok(Some(0x90 * 1024)));
        let no_stop = [0, 10, 0, 4, 0x00, 0x00, 0x00, 0x04, 0x01, 0xff];
        assert_eq!(stop_ticks(&no_stop), Ok(None));
        assert!(stop_ticks(&[0, 10, 0, 40]).is_err());
    }
}
