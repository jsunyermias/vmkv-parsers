//! Native FLAC (`A_FLAC`, RFC 9639).
//!
//! - A leading ID3v2 tag is skipped; a trailing ID3v1 tag is recognized.
//! - `codec_private` is every byte from the `fLaC` marker to the first
//!   frame, as one `src` chunk: the Matroska mapping asks for the marker and
//!   all metadata blocks (decision 58).
//! - Each unit is one whole FLAC frame, header and CRC included.
//! - A frame does not carry its own length. Its end is the next position
//!   where the CRC-16 of the bytes so far is zero and a valid frame header
//!   with the expected frame or sample number starts, or the end of the
//!   audio data. A frame longer than its verbatim (uncompressed) encoding
//!   could ever be is invalid, which bounds the memory a frame can take.
//! - Times come from each frame's own coded number: its frame number times
//!   the fixed block size, or its sample number with a variable block size.
//!   Frames must be contiguous. Every frame is a random access point.

use vtj::cli::ParseError;
use vtj::source::SourceFile;
use vtj::*;

/// A frame header is at most 16 bytes: 4 fixed, 7 coded number, 2 block
/// size, 2 sample rate, 1 CRC-8.
const MAX_HEADER: usize = 16;
const STREAMINFO_LEN: u64 = 34;
const ID3V1_LEN: u64 = 128;
/// Bytes read from the source at a time while scanning frames.
const CHUNK: usize = 1 << 16;

const fn crc_table(poly: u16, width: u32) -> [u16; 256] {
    let mut t = [0u16; 256];
    let top: u16 = 1 << (width - 1);
    let mask: u16 = if width == 16 { 0xffff } else { (1 << width) - 1 };
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u16) << (width - 8);
        let mut b = 0;
        while b < 8 {
            c = if c & top != 0 { (c << 1) ^ poly } else { c << 1 };
            b += 1;
        }
        t[i] = c & mask;
        i += 1;
    }
    t
}

const CRC8: [u16; 256] = crc_table(0x07, 8);
const CRC16: [u16; 256] = crc_table(0x8005, 16);

/// CRC-8 of a frame header (polynomial 0x07, initial value 0).
pub fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |c, &b| CRC8[(c ^ b) as usize] as u8)
}

#[inline]
fn crc16_step(c: u16, b: u8) -> u16 {
    (c << 8) ^ CRC16[((c >> 8) as u8 ^ b) as usize]
}

/// CRC-16 of a frame (polynomial 0x8005, initial value 0). Over a whole
/// frame, its own trailing CRC included, it is 0.
pub fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |c, &b| crc16_step(c, b))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfo {
    pub min_block: u32,
    pub max_block: u32,
    pub sample_rate: u32,
    pub channels: u8,
    pub bits_per_sample: u8,
    /// 0 when unknown.
    pub total_samples: u64,
}

pub fn parse_streaminfo(b: &[u8; 34]) -> Result<StreamInfo, String> {
    let min_block = u16::from_be_bytes([b[0], b[1]]) as u32;
    let max_block = u16::from_be_bytes([b[2], b[3]]) as u32;
    let packed = u64::from_be_bytes(b[10..18].try_into().expect("8 bytes"));
    let info = StreamInfo {
        min_block,
        max_block,
        sample_rate: (packed >> 44) as u32,
        channels: ((packed >> 41) & 7) as u8 + 1,
        bits_per_sample: ((packed >> 36) & 31) as u8 + 1,
        total_samples: packed & ((1 << 36) - 1),
    };
    if info.min_block < 16 || info.max_block < info.min_block {
        return Err(format!("STREAMINFO block sizes {min_block}..{max_block} are invalid"));
    }
    if info.sample_rate == 0 {
        return Err("STREAMINFO sample rate is 0".into());
    }
    if info.bits_per_sample < 4 {
        return Err(format!("STREAMINFO declares {} bits per sample", info.bits_per_sample));
    }
    Ok(info)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub variable: bool,
    pub block_size: u32,
    /// `None`: the one in STREAMINFO.
    pub sample_rate: Option<u32>,
    pub channels: u8,
    /// `None`: the one in STREAMINFO.
    pub bits_per_sample: Option<u8>,
    /// Frame number with a fixed block size, first sample number with a
    /// variable one.
    pub number: u64,
    pub len: usize,
}

/// Parses the frame header at the start of `b`, which holds up to
/// `MAX_HEADER` bytes (fewer only at the end of the source).
pub fn parse_frame_header(b: &[u8]) -> Result<FrameHeader, &'static str> {
    if b.len() < 6 {
        return Err("frame header cut short");
    }
    if b[0] != 0xff || b[1] & 0xfe != 0xf8 {
        return Err("no frame sync");
    }
    let variable = b[1] & 1 == 1;
    let (bs_code, sr_code) = (b[2] >> 4, b[2] & 15);
    let (ch_code, ss_code) = (b[3] >> 4, (b[3] >> 1) & 7);
    if b[3] & 1 != 0 {
        return Err("reserved bit set in frame header");
    }
    if bs_code == 0 {
        return Err("reserved block size code");
    }
    if sr_code == 15 {
        return Err("invalid sample rate code");
    }
    let channels = match ch_code {
        0..=7 => ch_code + 1,
        8..=10 => 2,
        _ => return Err("reserved channel assignment"),
    };
    let bits_per_sample = match ss_code {
        0 => None,
        1 => Some(8),
        2 => Some(12),
        4 => Some(16),
        5 => Some(20),
        6 => Some(24),
        7 => Some(32),
        _ => return Err("reserved sample size code"),
    };

    // Coded number: UTF-8-like, up to 31 bits (6 bytes) for a frame number,
    // up to 36 bits (7 bytes) for a sample number.
    let first = b[4];
    let extra = match first.leading_ones() {
        0 => 0,
        n @ 2..=7 => n as usize - 1,
        _ => return Err("invalid coded number"),
    };
    if extra > if variable { 6 } else { 5 } {
        return Err("coded number too long");
    }
    let mut pos = 5;
    let mut number = if extra == 0 { first as u64 } else { (first & (0x7f >> (extra + 1))) as u64 };
    for _ in 0..extra {
        let c = *b.get(pos).ok_or("frame header cut short")?;
        if c & 0xc0 != 0x80 {
            return Err("invalid coded number");
        }
        number = number << 6 | (c & 0x3f) as u64;
        pos += 1;
    }

    let mut read = |n: usize| -> Result<u32, &'static str> {
        let s = b.get(pos..pos + n).ok_or("frame header cut short")?;
        pos += n;
        Ok(s.iter().fold(0u32, |v, &x| v << 8 | x as u32))
    };
    let block_size = match bs_code {
        1 => 192,
        2..=5 => 576 << (bs_code - 2),
        6 => read(1)? + 1,
        7 => read(2)? + 1,
        _ => 256 << (bs_code - 8),
    };
    if block_size > 65535 {
        return Err("block size 65536");
    }
    let sample_rate = match sr_code {
        0 => None,
        1 => Some(88200),
        2 => Some(176400),
        3 => Some(192000),
        4 => Some(8000),
        5 => Some(16000),
        6 => Some(22050),
        7 => Some(24000),
        8 => Some(32000),
        9 => Some(44100),
        10 => Some(48000),
        11 => Some(96000),
        12 => Some(read(1)? * 1000),
        13 => Some(read(2)?),
        _ => Some(read(2)? * 10),
    };
    let crc = *b.get(pos).ok_or("frame header cut short")?;
    if crc8(&b[..pos]) != crc {
        return Err("frame header CRC-8 mismatch");
    }
    Ok(FrameHeader { variable, block_size, sample_rate, channels, bits_per_sample, number, len: pos + 1 })
}

/// Longest a frame with header `h` can be: the header, every channel
/// stored verbatim (one extra bit per sample for a side channel, a subframe
/// header with the longest wasted-bits prefix), and the CRC-16.
fn max_frame_len(h: &FrameHeader, bits: u32) -> u64 {
    let per_channel = (h.block_size as u64 * (bits as u64 + 1)).div_ceil(8) + 6;
    MAX_HEADER as u64 + h.channels as u64 * per_channel + 2
}

/// A sliding window of source bytes, `[base, base + data.len())`.
struct Window {
    data: Vec<u8>,
    base: u64,
    end: u64,
}

impl Window {
    /// Makes the window reach `upto` (or the end of the audio data).
    fn fill(&mut self, src: &mut SourceFile, upto: u64) -> Result<(), ParseError> {
        let upto = upto.min(self.end);
        let have = self.base + self.data.len() as u64;
        if upto <= have {
            return Ok(());
        }
        let n = ((upto - have) as usize).max(CHUNK).min((self.end - have) as usize);
        let old = self.data.len();
        self.data.resize(old + n, 0);
        src.read_at(have, &mut self.data[old..])?;
        Ok(())
    }

    fn drop_before(&mut self, pos: u64) {
        let n = (pos - self.base) as usize;
        self.data.drain(..n);
        self.base = pos;
    }

    fn slice(&self, from: u64, len: usize) -> &[u8] {
        let s = (from - self.base) as usize;
        &self.data[s..(s + len).min(self.data.len())]
    }

    fn byte(&self, at: u64) -> u8 {
        self.data[(at - self.base) as usize]
    }
}

/// Offset just past a leading ID3v2 tag, or 0.
fn skip_id3v2(src: &mut SourceFile) -> Result<u64, ParseError> {
    let mut pos = 0;
    while src.size() - pos >= 10 {
        let mut h = [0u8; 10];
        src.read_at(pos, &mut h)?;
        if &h[..3] != b"ID3" {
            break;
        }
        if h[6..10].iter().any(|b| b & 0x80 != 0) {
            return Err(ParseError::invalid(format!("ID3v2 tag at byte {pos} has an invalid size")));
        }
        let size = h[6..10].iter().fold(0u64, |v, &b| v << 7 | b as u64);
        let footer = if h[5] & 0x10 != 0 { 10 } else { 0 };
        pos += 10 + size + footer;
        if pos > src.size() {
            return Err(ParseError::truncated("ID3v2 tag runs past the end of the source"));
        }
    }
    Ok(pos)
}

pub struct Flac;

impl Parser for Flac {
    fn name(&self) -> &'static str {
        "vmkv-parser-flac"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let src = ctx.source(0);
        let size = src.size();
        let marker = skip_id3v2(src)?;
        let mut m = [0u8; 4];
        if size - marker < 4 {
            return Err(ParseError::new(ErrorCode::MissingInitializationData, "no fLaC marker"));
        }
        src.read_at(marker, &mut m)?;
        if &m != b"fLaC" {
            return Err(ParseError::new(
                ErrorCode::MissingInitializationData,
                format!("no fLaC marker at byte {marker}"),
            ));
        }

        // Metadata blocks: STREAMINFO first and only once.
        let mut pos = marker + 4;
        let mut info: Option<StreamInfo> = None;
        loop {
            let mut h = [0u8; 4];
            if size - pos < 4 {
                return Err(ParseError::truncated(format!("metadata block header cut at byte {size}")));
            }
            src.read_at(pos, &mut h)?;
            let (last, kind) = (h[0] & 0x80 != 0, h[0] & 0x7f);
            let len = u32::from_be_bytes([0, h[1], h[2], h[3]]) as u64;
            if size - pos - 4 < len {
                return Err(ParseError::truncated(format!("metadata block at byte {pos} cut at byte {size}")));
            }
            match (kind, info.is_some()) {
                (0, false) => {
                    if len != STREAMINFO_LEN {
                        return Err(ParseError::invalid(format!("STREAMINFO is {len} bytes instead of 34")));
                    }
                    let mut b = [0u8; 34];
                    src.read_at(pos + 4, &mut b)?;
                    info = Some(parse_streaminfo(&b).map_err(ParseError::invalid)?);
                }
                (0, true) => return Err(ParseError::invalid(format!("second STREAMINFO at byte {pos}"))),
                (_, false) => {
                    return Err(ParseError::new(
                        ErrorCode::MissingInitializationData,
                        "the first metadata block is not STREAMINFO",
                    ))
                }
                (127, true) => {
                    return Err(ParseError::invalid(format!("invalid metadata block type 127 at byte {pos}")))
                }
                (_, true) => {}
            }
            pos += 4 + len;
            if last {
                break;
            }
        }
        let info = info.expect("the loop only ends after STREAMINFO");
        let codec_private = vec![Chunk::src(0, marker, pos - marker)];

        // A trailing ID3v1 tag is only taken as one when the frames end
        // exactly where it starts (checked at the end of the scan).
        let mut tag = [0u8; 3];
        let id3v1 = size - pos >= ID3V1_LEN && {
            src.read_at(size - ID3V1_LEN, &mut tag)?;
            &tag == b"TAG"
        };

        let rate = Rational::new(info.sample_rate as i64, 1);
        let flags = Flags::NONE.with(Flag::RandomAccess);
        let mut w = Window { data: Vec::new(), base: pos, end: size };
        let mut timeline: Option<Timeline> = None;
        let mut fixed_block: Option<u32> = None;
        let mut variable: Option<bool> = None;
        let mut next_sample: u64 = 0;
        let mut samples: u64 = 0;
        let mut count: u64 = 0;
        let mut prev_short = false;

        // Checks a header against the stream and returns its first sample.
        let place = |h: &FrameHeader, variable: Option<bool>, fixed: Option<u32>| -> Result<u64, String> {
            if variable.is_some_and(|v| v != h.variable) {
                return Err("changes the blocking strategy".into());
            }
            if h.sample_rate.is_some_and(|r| r != info.sample_rate) {
                return Err(format!("has sample rate {}, STREAMINFO {}", h.sample_rate.unwrap_or(0), info.sample_rate));
            }
            if h.bits_per_sample.is_some_and(|b| b != info.bits_per_sample) {
                return Err(format!(
                    "has {} bits per sample, STREAMINFO {}",
                    h.bits_per_sample.unwrap_or(0),
                    info.bits_per_sample
                ));
            }
            if h.channels != info.channels {
                return Err(format!("has {} channels, STREAMINFO {}", h.channels, info.channels));
            }
            if h.block_size > info.max_block {
                return Err(format!("has block size {}, STREAMINFO maximum {}", h.block_size, info.max_block));
            }
            Ok(if h.variable { h.number } else { h.number * fixed.unwrap_or(h.block_size) as u64 })
        };

        while pos < size {
            if id3v1 && pos == size - ID3V1_LEN {
                break;
            }
            let src = ctx.source(0);
            w.drop_before(pos);
            w.fill(src, pos + MAX_HEADER as u64)?;
            let h = parse_frame_header(w.slice(pos, MAX_HEADER)).map_err(|e| {
                if e == "frame header cut short" {
                    ParseError::truncated(format!("frame {count} header cut at byte {size}"))
                } else {
                    ParseError::invalid(format!("frame {count} at byte {pos}: {e}"))
                }
            })?;
            let first = place(&h, variable, fixed_block).map_err(|e| {
                ParseError::new(ErrorCode::InconsistentTrackParameters, format!("frame {count} at byte {pos} {e}"))
            })?;
            if prev_short {
                return Err(ParseError::invalid(format!(
                    "frame {count} at byte {pos} follows a frame shorter than the fixed block size"
                )));
            }
            match timeline {
                None => {
                    timeline = Some(Timeline::new(rate, first as i128)?);
                    variable = Some(h.variable);
                    if !h.variable {
                        fixed_block = Some(h.block_size);
                    }
                }
                Some(_) if first != next_sample => {
                    return Err(ParseError::invalid(format!(
                        "frame {count} at byte {pos} starts at sample {first} instead of {next_sample}"
                    )));
                }
                Some(_) => {}
            }
            if fixed_block.is_some_and(|f| h.block_size > f) {
                return Err(ParseError::new(
                    ErrorCode::InconsistentTrackParameters,
                    format!(
                        "frame {count} at byte {pos} has block size {} over the fixed {}",
                        h.block_size,
                        fixed_block.unwrap_or(0)
                    ),
                ));
            }
            prev_short = fixed_block.is_some_and(|f| h.block_size < f);
            next_sample = first + h.block_size as u64;
            let next_number = if h.variable { next_sample } else { h.number + 1 };

            // Scan for the end of the frame.
            let limit = pos + max_frame_len(&h, info.bits_per_sample as u32);
            let min_end = pos + h.len as u64 + h.channels as u64 + 2;
            let mut crc: u16 = 0;
            let mut p = pos;
            let end = loop {
                if p == size || (id3v1 && p == size - ID3V1_LEN && crc == 0) {
                    if crc == 0 {
                        break p;
                    }
                    // Without a CRC match at the end of the data, a cut is
                    // indistinguishable from a corrupt last frame; a cut is
                    // by far the likelier cause (decision 58).
                    return Err(ParseError::truncated(format!("frame {count} cut at byte {size}")));
                }
                if p > limit {
                    return Err(ParseError::invalid(format!(
                        "frame {count} at byte {pos}: no frame end within {} bytes",
                        limit - pos
                    )));
                }
                w.fill(ctx.source(0), p + MAX_HEADER as u64 + 1)?;
                if crc == 0 && p >= min_end && w.byte(p) == 0xff {
                    if let Ok(n) = parse_frame_header(w.slice(p, MAX_HEADER)) {
                        if n.number == next_number && place(&n, Some(h.variable), fixed_block).is_ok() {
                            break p;
                        }
                    }
                }
                crc = crc16_step(crc, w.byte(p));
                p += 1;
            };
            if end < min_end {
                return Err(ParseError::invalid(format!("frame {count} at byte {pos} is too short")));
            }

            let (pts, dur) = timeline.as_mut().expect("set with the first frame").advance(h.block_size as i128)?;
            ctx.emit(&Unit::new(pts, dur, flags, vec![Chunk::src(0, pos, end - pos)]))?;
            samples += h.block_size as u64;
            count += 1;
            pos = end;
        }

        if count == 0 {
            return Err(ParseError::invalid("no FLAC frames"));
        }
        if info.total_samples != 0 && samples != info.total_samples {
            let msg = format!("STREAMINFO declares {} samples, the frames carry {samples}", info.total_samples);
            return Err(if samples < info.total_samples {
                ParseError::truncated(msg)
            } else {
                ParseError::invalid(msg)
            });
        }
        let mut track = Track::new(TrackType::Audio, "A_FLAC");
        track.codec_private = Some(codec_private);
        let mut audio = Audio::new(rate, info.channels as u64);
        audio.bit_depth = Some(info.bits_per_sample as u64);
        track.audio = Some(audio);
        Ok(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_check_values() {
        // RFC 9639 uses the plain (non-reflected, zero-init) CRC-8/SMBUS and
        // CRC-16/UMTS; "123456789" check values from the CRC catalogue.
        assert_eq!(crc8(b"123456789"), 0xf4);
        assert_eq!(crc16(b"123456789"), 0xfee8);
        let mut v = b"123456789".to_vec();
        v.extend(crc16(&v).to_be_bytes());
        assert_eq!(crc16(&v), 0, "a frame with its own CRC sums to 0");
    }

    fn header(bytes: &[u8]) -> Vec<u8> {
        let mut v = bytes.to_vec();
        v.push(crc8(&v));
        v
    }

    #[test]
    fn frame_headers() {
        // Fixed, 4608 samples (code 5), 44.1 kHz, stereo independent, 16 bit,
        // frame 0.
        let h = parse_frame_header(&header(&[0xff, 0xf8, 0x59, 0x18, 0x00])).unwrap();
        assert_eq!(
            h,
            FrameHeader {
                variable: false,
                block_size: 4608,
                sample_rate: Some(44100),
                channels: 2,
                bits_per_sample: Some(16),
                number: 0,
                len: 6
            }
        );
        // Variable, 16-bit block size 1000 - 1, 16-bit Hz rate, mid/side,
        // sample number 4096 in three bytes.
        let h =
            parse_frame_header(&header(&[0xff, 0xf9, 0x7d, 0xa8, 0xe1, 0x80, 0x80, 0x03, 0xe7, 0xbb, 0x80])).unwrap();
        assert_eq!((h.block_size, h.sample_rate, h.channels, h.number, h.len), (1000, Some(48000), 2, 4096, 12));
        assert!(h.variable);
        // Coded number 0x80 cannot start a sequence.
        assert_eq!(parse_frame_header(&header(&[0xff, 0xf8, 0x59, 0x18, 0x80])), Err("invalid coded number"));
        let mut bad = header(&[0xff, 0xf8, 0x59, 0x18, 0x00]);
        bad[5] ^= 1;
        assert_eq!(parse_frame_header(&bad), Err("frame header CRC-8 mismatch"));
        assert_eq!(parse_frame_header(&header(&[0xff, 0xf8, 0x09, 0x18, 0x00])), Err("reserved block size code"));
        assert_eq!(parse_frame_header(&header(&[0xff, 0xf8, 0x5f, 0x18, 0x00])), Err("invalid sample rate code"));
        assert_eq!(parse_frame_header(&header(&[0xff, 0xf8, 0x59, 0xb8, 0x00])), Err("reserved channel assignment"));
        assert_eq!(parse_frame_header(&header(&[0xff, 0xf8, 0x59, 0x16, 0x00])), Err("reserved sample size code"));
        assert_eq!(parse_frame_header(&header(&[0xff, 0xfa, 0x59, 0x18, 0x00])), Err("no frame sync"));
    }

    #[test]
    fn streaminfo_fields() {
        // From testdata/media/flac_stereo.flac.
        let b: [u8; 34] = [
            0x12, 0x00, 0x12, 0x00, 0x00, 0x03, 0x05, 0x00, 0x05, 0x14, 0x0a, 0xc4, 0x42, 0xf0, 0x00, 0x00, 0xac, 0x44,
            0xb4, 0x8e, 0x1d, 0xd0, 0xc1, 0x93, 0xfd, 0xd4, 0x0c, 0x05, 0xf2, 0x88, 0xd6, 0x01, 0x57, 0xa3,
        ];
        let i = parse_streaminfo(&b).unwrap();
        assert_eq!(
            i,
            StreamInfo {
                min_block: 4608,
                max_block: 4608,
                sample_rate: 44100,
                channels: 2,
                bits_per_sample: 16,
                total_samples: 44100
            }
        );
    }
}
