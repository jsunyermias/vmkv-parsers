//! AC-3 (`A_AC3`, ATSC A/52) and E-AC-3 (`A_EAC3`, A/52 Annex E) elementary
//! streams.
//!
//! - Each unit is one whole syncframe, sync word included. Every frame is a
//!   random access point and lasts 1536 samples (AC-3) or 256 per audio
//!   block (E-AC-3: 1, 2, 3 or 6 blocks).
//! - The codec comes from `bsid`: up to 8 is AC-3, 11 to 16 is E-AC-3. The
//!   half- and quarter-rate AC-3 variants (`bsid` 9 and 10) are rejected.
//! - E-AC-3 dependent substreams and independent substreams other than 0
//!   are rejected: they need a channel map or a program choice this parser
//!   does not make (decision 59).
//! - With `--crc verify` (the default), each frame's CRC-16 is checked over
//!   the whole frame, as decoders do.
//! - Sample rate, channel layout and codec must not change mid-stream.

use vtj::cli::{ParamSpec, ParseError};
use vtj::*;

/// Bitrates in kbit/s, indexed by `frmsizecod >> 1`.
const BITRATES: [u32; 19] = [32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640];
const AC3_RATES: [u32; 3] = [48000, 44100, 32000];
const EAC3_HALF_RATES: [u32; 3] = [24000, 22050, 16000];
/// Channels per `acmod`, without LFE.
const ACMOD_CHANNELS: [u8; 8] = [2, 1, 2, 3, 3, 4, 4, 5];
const HEADER: usize = 8;

const CRC: ParamSpec = ParamSpec::choice(
    "crc",
    &["verify", "ignore"],
    "verify: a frame whose CRC-16 does not match is invalid; ignore: frames are taken as they are",
)
.default("verify");

const fn crc_table() -> [u16; 256] {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u16) << 8;
        let mut b = 0;
        while b < 8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x8005 } else { c << 1 };
            b += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

const CRC_TABLE: [u16; 256] = crc_table();

/// CRC-16 (polynomial 0x8005, initial value 0). Over a whole frame after
/// the sync word, both CRC words included, it is 0.
pub fn crc16(data: &[u8]) -> u16 {
    data.iter().fold(0u16, |c, &b| (c << 8) ^ CRC_TABLE[((c >> 8) as u8 ^ b) as usize])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ac3,
    Eac3,
}

impl Kind {
    pub fn codec_id(self) -> &'static str {
        match self {
            Kind::Ac3 => "A_AC3",
            Kind::Eac3 => "A_EAC3",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncFrame {
    pub kind: Kind,
    pub bsid: u8,
    /// E-AC-3 `strmtyp`: 0 independent, 1 dependent, 2 AC-3 converted to
    /// E-AC-3 (independent). Always 0 for AC-3.
    pub stream_type: u8,
    pub substream_id: u8,
    pub frame_len: u64,
    pub sample_rate: u32,
    pub samples: u32,
    pub acmod: u8,
    pub lfe: bool,
}

impl SyncFrame {
    pub fn channels(&self) -> u8 {
        ACMOD_CHANNELS[self.acmod as usize] + self.lfe as u8
    }

    /// What must stay the same for the whole track.
    fn layout(&self) -> (Kind, u32, u8, bool) {
        (self.kind, self.sample_rate, self.acmod, self.lfe)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderError {
    NoSync,
    Bsid(u8),
    SampleRate,
    FrameSize(u8),
    StreamType,
}

/// MSB-first bit reader over a header.
struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn take(&mut self, n: usize) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            let bit = self.b.get(self.pos / 8).map_or(0, |x| x >> (7 - self.pos % 8) & 1);
            v = v << 1 | bit as u32;
            self.pos += 1;
        }
        v
    }
}

/// Parses the first `HEADER` bytes of a syncframe.
pub fn parse_header(b: &[u8; HEADER]) -> Result<SyncFrame, HeaderError> {
    if b[0] != 0x0b || b[1] != 0x77 {
        return Err(HeaderError::NoSync);
    }
    let bsid = b[5] >> 3;
    match bsid {
        0..=8 => {
            let mut r = Bits { b, pos: 32 };
            let fscod = r.take(2) as usize;
            let frmsizecod = r.take(6) as u8;
            r.take(5 + 3); // bsid, bsmod
            let acmod = r.take(3) as u8;
            if fscod == 3 {
                return Err(HeaderError::SampleRate);
            }
            if frmsizecod >= 38 {
                return Err(HeaderError::FrameSize(frmsizecod));
            }
            if acmod & 1 != 0 && acmod != 1 {
                r.take(2); // cmixlev
            }
            if acmod & 4 != 0 {
                r.take(2); // surmixlev
            }
            if acmod == 2 {
                r.take(2); // dsurmod
            }
            let lfe = r.take(1) == 1;
            let kbps = BITRATES[(frmsizecod >> 1) as usize];
            let words = match fscod {
                0 => kbps * 2,
                1 => kbps * 320 / 147 + (frmsizecod & 1) as u32,
                _ => kbps * 3,
            };
            Ok(SyncFrame {
                kind: Kind::Ac3,
                bsid,
                stream_type: 0,
                substream_id: 0,
                frame_len: words as u64 * 2,
                sample_rate: AC3_RATES[fscod],
                samples: 1536,
                acmod,
                lfe,
            })
        }
        11..=16 => {
            let mut r = Bits { b, pos: 16 };
            let stream_type = r.take(2) as u8;
            let substream_id = r.take(3) as u8;
            let frmsiz = r.take(11);
            let fscod = r.take(2) as usize;
            let (sample_rate, blocks) = if fscod == 3 {
                let fscod2 = r.take(2) as usize;
                if fscod2 == 3 {
                    return Err(HeaderError::SampleRate);
                }
                (EAC3_HALF_RATES[fscod2], 6)
            } else {
                (AC3_RATES[fscod], [1, 2, 3, 6][r.take(2) as usize])
            };
            let acmod = r.take(3) as u8;
            let lfe = r.take(1) == 1;
            if stream_type == 3 {
                return Err(HeaderError::StreamType);
            }
            // frmsiz counts 16-bit words minus one; a frame must at least
            // hold its own header.
            let frame_len = (frmsiz as u64 + 1) * 2;
            if frame_len < HEADER as u64 {
                return Err(HeaderError::FrameSize(frmsiz as u8));
            }
            Ok(SyncFrame {
                kind: Kind::Eac3,
                bsid,
                stream_type,
                substream_id,
                frame_len,
                sample_rate,
                samples: 256 * blocks,
                acmod,
                lfe,
            })
        }
        _ => Err(HeaderError::Bsid(bsid)),
    }
}

pub struct Ac3;

impl Parser for Ac3 {
    fn name(&self) -> &'static str {
        "vmkv-parser-ac3"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn params(&self) -> &'static [ParamSpec] {
        &[CRC]
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let verify = ctx.param_str("crc") != Some("ignore");
        let size = ctx.source(0).size();
        let mut first: Option<SyncFrame> = None;
        let mut timeline: Option<Timeline> = None;
        let mut frame = Vec::new();
        let mut pos = 0u64;
        let mut count = 0u64;

        while pos < size {
            if size - pos < HEADER as u64 {
                return Err(ParseError::truncated(format!("frame {count} header cut at byte {size}")));
            }
            let mut hb = [0u8; HEADER];
            ctx.source(0).read_at(pos, &mut hb)?;
            let h = parse_header(&hb).map_err(|e| match e {
                HeaderError::NoSync => ParseError::invalid(format!("no AC-3 sync at byte {pos}")),
                HeaderError::Bsid(b @ (9 | 10)) => ParseError::new(
                    ErrorCode::UnsupportedCodecVariant,
                    format!("frame {count} has bsid {b} (reduced sample rate AC-3)"),
                ),
                HeaderError::Bsid(b) => ParseError::invalid(format!("frame {count} at byte {pos} has bsid {b}")),
                HeaderError::SampleRate => {
                    ParseError::invalid(format!("frame {count} at byte {pos} has a reserved sample rate code"))
                }
                HeaderError::FrameSize(c) => {
                    ParseError::invalid(format!("frame {count} at byte {pos} has an invalid frame size code {c}"))
                }
                HeaderError::StreamType => {
                    ParseError::invalid(format!("frame {count} at byte {pos} has a reserved stream type"))
                }
            })?;
            if h.stream_type == 1 {
                return Err(ParseError::unsupported(format!(
                    "frame {count} at byte {pos} is an E-AC-3 dependent substream"
                )));
            }
            if h.substream_id != 0 {
                return Err(ParseError::unsupported(format!(
                    "frame {count} at byte {pos} is E-AC-3 independent substream {}",
                    h.substream_id
                )));
            }
            if h.frame_len > size - pos {
                return Err(ParseError::truncated(format!("frame {count} cut at byte {size}")));
            }
            match first {
                None => {
                    first = Some(h);
                    timeline = Some(Timeline::new(Rational::new(h.sample_rate as i64, 1), 0)?);
                }
                Some(f) if f.layout() != h.layout() => {
                    return Err(ParseError::new(
                        ErrorCode::InconsistentTrackParameters,
                        format!(
                            "frame {count} at byte {pos} changes codec/sample rate/acmod/LFE from {:?} to {:?}",
                            f.layout(),
                            h.layout()
                        ),
                    ));
                }
                Some(_) => {}
            }
            frame.clear();
            frame.extend_from_slice(&hb);
            frame.resize(h.frame_len as usize, 0);
            ctx.source(0).read_at(pos + HEADER as u64, &mut frame[HEADER..])?;
            if verify && crc16(&frame[2..]) != 0 {
                return Err(ParseError::invalid(format!("frame {count} at byte {pos}: CRC mismatch")));
            }
            let (pts, dur) = timeline.as_mut().expect("set with the first frame").advance(h.samples as i128)?;
            ctx.emit(&Unit::new(
                pts,
                dur,
                Flags::NONE.with(Flag::RandomAccess),
                vec![Chunk::src(0, pos, h.frame_len)],
            ))?;
            count += 1;
            pos += h.frame_len;
        }

        let Some(first) = first else {
            return Err(ParseError::invalid("no AC-3 frames"));
        };
        let mut track = Track::new(TrackType::Audio, first.kind.codec_id());
        track.audio = Some(Audio::new(Rational::new(first.sample_rate as i64, 1), first.channels() as u64));
        Ok(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac3_frame_sizes() {
        // 48 kHz, 192 kbit/s (frmsizecod 20): 768 bytes.
        let h = parse_header(&[0x0b, 0x77, 0, 0, 20, 0x40, 0x40, 0]).unwrap();
        assert_eq!((h.kind, h.frame_len, h.sample_rate, h.samples), (Kind::Ac3, 768, 48000, 1536));
        // 44.1 kHz, 32 kbit/s, odd frmsizecod: 70 words.
        assert_eq!(parse_header(&[0x0b, 0x77, 0, 0, 0x41, 0x40, 0x40, 0]).unwrap().frame_len, 140);
        // 44.1 kHz, 640 kbit/s: 1393 or 1394 words.
        assert_eq!(parse_header(&[0x0b, 0x77, 0, 0, 0x40 | 36, 0x40, 0x40, 0]).unwrap().frame_len, 2786);
        assert_eq!(parse_header(&[0x0b, 0x77, 0, 0, 0x40 | 37, 0x40, 0x40, 0]).unwrap().frame_len, 2788);
        // 32 kHz, 640 kbit/s: 1920 words.
        assert_eq!(parse_header(&[0x0b, 0x77, 0, 0, 0x80 | 36, 0x40, 0x40, 0]).unwrap().frame_len, 3840);
        assert_eq!(parse_header(&[0x0b, 0x77, 0, 0, 38, 0x40, 0x40, 0]), Err(HeaderError::FrameSize(38)));
        assert_eq!(parse_header(&[0x0b, 0x77, 0, 0, 0xc0, 0x40, 0x40, 0]), Err(HeaderError::SampleRate));
    }

    #[test]
    fn ac3_channels_follow_the_optional_fields() {
        // acmod 7 (3/2): cmixlev and surmixlev before lfeon.
        // bits after bsid/bsmod: 111 cc ss L -> 0b1110_0001 = LFE on.
        let h = parse_header(&[0x0b, 0x77, 0, 0, 20, 0x40, 0b1110_0001, 0]).unwrap();
        assert_eq!((h.acmod, h.lfe, h.channels()), (7, true, 6));
        // acmod 2 (2/0): dsurmod, then lfeon: 010 dd L -> 0b0100_0100.
        let h = parse_header(&[0x0b, 0x77, 0, 0, 20, 0x40, 0b0100_0100, 0]).unwrap();
        assert_eq!((h.acmod, h.lfe, h.channels()), (2, true, 3));
        // acmod 1 (mono): no optional fields: 001 L -> 0b0011_0000.
        let h = parse_header(&[0x0b, 0x77, 0, 0, 20, 0x40, 0b0011_0000, 0]).unwrap();
        assert_eq!((h.acmod, h.lfe, h.channels()), (1, true, 2));
    }

    #[test]
    fn eac3_headers() {
        // Independent, substream 0, frmsiz 511 (1024 bytes), 48 kHz, 6
        // blocks, acmod 7, LFE, bsid 16.
        let h = parse_header(&[0x0b, 0x77, 0x01, 0xff, 0x3f, 0x80, 0, 0]).unwrap();
        assert_eq!(
            h,
            SyncFrame {
                kind: Kind::Eac3,
                bsid: 16,
                stream_type: 0,
                substream_id: 0,
                frame_len: 1024,
                sample_rate: 48000,
                samples: 1536,
                acmod: 7,
                lfe: true
            }
        );
        // Half rate (fscod 3, fscod2 1 = 22.05 kHz) always has 6 blocks.
        let h = parse_header(&[0x0b, 0x77, 0x01, 0xff, 0xd4, 0x80, 0, 0]).unwrap();
        assert_eq!((h.sample_rate, h.samples, h.acmod), (22050, 1536, 2));
        // One block: numblkscod 0.
        assert_eq!(parse_header(&[0x0b, 0x77, 0x01, 0xff, 0x04, 0x80, 0, 0]).unwrap().samples, 256);
        // Dependent substream 0 and reserved stream type 3.
        assert_eq!(parse_header(&[0x0b, 0x77, 0x41, 0xff, 0x3f, 0x80, 0, 0]).unwrap().stream_type, 1);
        assert_eq!(parse_header(&[0x0b, 0x77, 0xc1, 0xff, 0x3f, 0x80, 0, 0]), Err(HeaderError::StreamType));
        assert_eq!(parse_header(&[0x0b, 0x77, 0x01, 0xff, 0x3f, 0x88, 0, 0]), Err(HeaderError::Bsid(17)));
        assert_eq!(parse_header(&[0x0b, 0x78, 0x01, 0xff, 0x3f, 0x80, 0, 0]), Err(HeaderError::NoSync));
    }
}
