//! DTS Coherent Acoustics (`A_DTS`, ETSI TS 102 114), with or without
//! DTS-HD extension substreams (MA, HRA).
//!
//! - The core in its usual form, 16-bit big-endian words with sync word
//!   `7FFE8001`: each unit is one core frame of `FSIZE + 1` bytes lasting
//!   `(NBLKS + 1) × 32` samples, followed by the DTS-HD extension
//!   substreams (sync `64582025`) up to the next core frame, as Matroska
//!   blocks hold them (decision 69). Every frame is a random access point.
//! - Without extension, channels are the core's `AMODE` plus LFE, at the
//!   core's sample rate. With it, the first audio asset descriptor gives the
//!   decoded sample rate and channel count (a 7.1 MA track has a 5.1
//!   core), and times still advance by the core frame.
//! - Rejected: the 14-bit and little-endian packings
//!   (`UNSUPPORTED_CODEC_VARIANT`), termination frames, a stream without a
//!   core (DTS Express) and several assets in one substream
//!   (`UNSUPPORTED_FEATURE`): no real sample of them to verify against
//!   (decision 62). Core layout, extension presence and the asset's sample
//!   rate and channels must not change mid-stream.

pub mod exss;

use vtj::cli::ParseError;
use vtj::*;

const SYNC_CORE: u32 = 0x7ffe_8001;
const SYNC_CORE_LE: u32 = 0xfe7f_0180;
const SYNC_14_BE: u32 = 0x1fff_e800;
const SYNC_14_LE: u32 = 0xff1f_00e8;
const SYNC_SUBSTREAM: u32 = 0x6458_2025;
const HEADER: usize = 11;

/// Sample rate per `SFREQ`; 0 is invalid.
const RATES: [u32; 16] = [0, 8000, 16000, 32000, 0, 0, 11025, 22050, 44100, 0, 0, 12000, 24000, 48000, 0, 0];
/// Channels per `AMODE` (0 to 15; higher values are user defined).
const AMODE_CHANNELS: [u8; 16] = [1, 2, 2, 2, 2, 3, 3, 4, 4, 5, 6, 6, 6, 7, 8, 8];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreFrame {
    pub samples: u32,
    pub frame_len: u64,
    pub amode: u8,
    pub sample_rate: u32,
    pub lfe: bool,
}

impl CoreFrame {
    pub fn channels(&self) -> u8 {
        AMODE_CHANNELS[self.amode as usize] + self.lfe as u8
    }

    fn layout(&self) -> (u32, u8, bool) {
        (self.sample_rate, self.amode, self.lfe)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderError {
    NoSync,
    Packing(&'static str),
    Substream,
    Termination,
    Blocks(u8),
    FrameSize(u64),
    Amode(u8),
    SampleRate(u8),
}

/// MSB-first bit reader.
struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn take(&mut self, n: usize) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = v << 1 | (self.b[self.pos / 8] >> (7 - self.pos % 8) & 1) as u32;
            self.pos += 1;
        }
        v
    }
}

pub fn parse_header(b: &[u8; HEADER]) -> Result<CoreFrame, HeaderError> {
    match u32::from_be_bytes(b[..4].try_into().expect("4 bytes")) {
        SYNC_CORE => {}
        SYNC_CORE_LE => return Err(HeaderError::Packing("16-bit little-endian")),
        SYNC_14_BE => return Err(HeaderError::Packing("14-bit big-endian")),
        SYNC_14_LE => return Err(HeaderError::Packing("14-bit little-endian")),
        SYNC_SUBSTREAM => return Err(HeaderError::Substream),
        _ => return Err(HeaderError::NoSync),
    }
    let mut r = Bits { b, pos: 32 };
    let normal = r.take(1) == 1;
    r.take(5); // SHORT: samples of a termination frame
    r.take(1); // CPF
    let nblks = r.take(7) as u8;
    let fsize = r.take(14) as u64;
    let amode = r.take(6) as u8;
    let sfreq = r.take(4) as u8;
    r.take(5 + 1 + 1 + 1 + 1 + 1 + 3 + 1 + 1); // RATE to ASPF
    let lff = r.take(2);
    if !normal {
        return Err(HeaderError::Termination);
    }
    if nblks < 5 {
        return Err(HeaderError::Blocks(nblks));
    }
    if fsize < 95 {
        return Err(HeaderError::FrameSize(fsize + 1));
    }
    if amode >= 16 {
        return Err(HeaderError::Amode(amode));
    }
    let sample_rate = RATES[sfreq as usize];
    if sample_rate == 0 {
        return Err(HeaderError::SampleRate(sfreq));
    }
    Ok(CoreFrame {
        samples: (nblks as u32 + 1) * 32,
        frame_len: fsize + 1,
        amode,
        sample_rate,
        lfe: lff == 1 || lff == 2,
    })
}

pub struct Dts;

impl Parser for Dts {
    fn name(&self) -> &'static str {
        "vmkv-parser-dts"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let size = ctx.source(0).size();
        let mut first: Option<CoreFrame> = None;
        let mut timeline: Option<Timeline> = None;
        let mut pos = 0u64;
        let mut count = 0u64;
        let mut has_extension: Option<bool> = None;
        let mut asset: Option<exss::Asset> = None;

        while pos < size {
            if size - pos < HEADER as u64 {
                return Err(ParseError::truncated(format!("frame {count} header cut at byte {size}")));
            }
            let mut hb = [0u8; HEADER];
            ctx.source(0).read_at(pos, &mut hb)?;
            let at = format!("frame {count} at byte {pos}");
            let h = parse_header(&hb).map_err(|e| match e {
                HeaderError::NoSync => ParseError::invalid(format!("no DTS sync at byte {pos}")),
                HeaderError::Packing(p) => {
                    ParseError::new(ErrorCode::UnsupportedCodecVariant, format!("{at} uses the {p} packing"))
                }
                HeaderError::Substream => {
                    ParseError::unsupported(format!("{at} is a DTS-HD extension substream without a core frame"))
                }
                HeaderError::Termination => ParseError::unsupported(format!("{at} is a termination frame")),
                HeaderError::Blocks(n) => ParseError::invalid(format!("{at} has {} PCM sample blocks", n as u32 + 1)),
                HeaderError::FrameSize(n) => ParseError::invalid(format!("{at} is {n} bytes")),
                HeaderError::Amode(a) => ParseError::unsupported(format!("{at} has user-defined channel mode {a}")),
                HeaderError::SampleRate(s) => ParseError::invalid(format!("{at} has invalid sample rate code {s}")),
            })?;
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
                        format!("{at} changes sample rate/channel mode/LFE from {:?} to {:?}", f.layout(), h.layout()),
                    ));
                }
                Some(_) => {}
            }
            // Extension substreams that follow the core frame.
            let mut end = pos + h.frame_len;
            let mut extended = false;
            while size - end >= 4 {
                let mut sync = [0u8; 4];
                ctx.source(0).read_at(end, &mut sync)?;
                if u32::from_be_bytes(sync) != SYNC_SUBSTREAM {
                    break;
                }
                // The sizes end at bit 67, or 75 with wide fields.
                if size - end < 10 {
                    return Err(ParseError::truncated(format!("frame {count} substream header cut at byte {size}")));
                }
                let mut head = [0u8; 10];
                ctx.source(0).read_at(end, &mut head)?;
                let (header_size, ss_size) = exss::sizes(&head)
                    .map_err(|e| ParseError::invalid(format!("frame {count} substream at byte {end}: {e}")))?;
                if ss_size > size - end {
                    return Err(ParseError::truncated(format!("frame {count} substream cut at byte {size}")));
                }
                let mut hb = vec![0u8; header_size as usize];
                ctx.source(0).read_at(end, &mut hb)?;
                let x = exss::parse(&hb).map_err(|e| {
                    let code =
                        if e.contains("assets") { ErrorCode::UnsupportedFeature } else { ErrorCode::InvalidBitstream };
                    ParseError::new(code, format!("frame {count} substream at byte {end}: {e}"))
                })?;
                if let Some(a) = x.asset {
                    match asset {
                        None => asset = Some(a),
                        Some(f) if (f.sample_rate, f.channels) != (a.sample_rate, a.channels) => {
                            return Err(ParseError::new(
                                ErrorCode::InconsistentTrackParameters,
                                format!(
                                    "frame {count} substream at byte {end} changes sample rate/channels from {:?} to {:?}",
                                    (f.sample_rate, f.channels),
                                    (a.sample_rate, a.channels)
                                ),
                            ));
                        }
                        Some(_) => {}
                    }
                }
                extended = true;
                end += ss_size;
            }
            match has_extension {
                None => has_extension = Some(extended),
                Some(e) if e != extended => {
                    return Err(ParseError::new(
                        ErrorCode::InconsistentTrackParameters,
                        format!(
                            "{at} {} DTS-HD extension, unlike the first frame",
                            if extended { "has a" } else { "lacks the" }
                        ),
                    ));
                }
                Some(_) => {}
            }

            let (pts, dur) = timeline.as_mut().expect("set with the first frame").advance(h.samples as i128)?;
            ctx.emit(&Unit::new(pts, dur, Flags::NONE.with(Flag::RandomAccess), vec![Chunk::src(0, pos, end - pos)]))?;
            count += 1;
            pos = end;
        }

        let Some(first) = first else {
            return Err(ParseError::invalid("no DTS frames"));
        };
        let mut track = Track::new(TrackType::Audio, "A_DTS");
        track.audio = Some(match (has_extension, asset) {
            (Some(true), Some(a)) => Audio::new(Rational::new(a.sample_rate as i64, 1), a.channels as u64),
            (Some(true), None) => {
                return Err(ParseError::new(
                    ErrorCode::MissingInitializationData,
                    "no DTS-HD extension substream carries an asset descriptor with static fields",
                ))
            }
            _ => Audio::new(Rational::new(first.sample_rate as i64, 1), first.channels() as u64),
        });
        Ok(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A core header with the given fields; RATE and the flags are zero.
    fn header(normal: bool, nblks: u8, fsize: u16, amode: u8, sfreq: u8, lff: u8) -> [u8; HEADER] {
        let mut bits: u128 = SYNC_CORE as u128;
        let mut push = |v: u128, n: u32| bits = bits << n | v;
        push(normal as u128, 1);
        push(31, 5);
        push(0, 1);
        push(nblks as u128, 7);
        push(fsize as u128, 14);
        push(amode as u128, 6);
        push(sfreq as u128, 4);
        push(0, 15);
        push(lff as u128, 2);
        push(0, 1);
        let mut b = [0u8; HEADER];
        b.copy_from_slice(&(bits << (128 - 88)).to_be_bytes()[..HEADER]);
        b
    }

    #[test]
    fn core_headers() {
        let h = parse_header(&header(true, 15, 2012, 9, 13, 2)).unwrap();
        assert_eq!(h, CoreFrame { samples: 512, frame_len: 2013, amode: 9, sample_rate: 48000, lfe: true });
        assert_eq!(h.channels(), 6);
        assert_eq!(parse_header(&header(true, 15, 2012, 2, 8, 0)).unwrap().channels(), 2);
        assert_eq!(parse_header(&header(false, 15, 2012, 9, 13, 2)), Err(HeaderError::Termination));
        assert_eq!(parse_header(&header(true, 4, 2012, 9, 13, 2)), Err(HeaderError::Blocks(4)));
        assert_eq!(parse_header(&header(true, 15, 94, 9, 13, 2)), Err(HeaderError::FrameSize(95)));
        assert_eq!(parse_header(&header(true, 15, 2012, 16, 13, 2)), Err(HeaderError::Amode(16)));
        assert_eq!(parse_header(&header(true, 15, 2012, 9, 4, 2)), Err(HeaderError::SampleRate(4)));
    }

    #[test]
    fn other_sync_words() {
        let with = |sync: u32| {
            let mut b = [0u8; HEADER];
            b[..4].copy_from_slice(&sync.to_be_bytes());
            parse_header(&b)
        };
        assert_eq!(with(SYNC_CORE_LE), Err(HeaderError::Packing("16-bit little-endian")));
        assert_eq!(with(SYNC_14_BE), Err(HeaderError::Packing("14-bit big-endian")));
        assert_eq!(with(SYNC_14_LE), Err(HeaderError::Packing("14-bit little-endian")));
        assert_eq!(with(SYNC_SUBSTREAM), Err(HeaderError::Substream));
        assert_eq!(with(0x0b77_0000), Err(HeaderError::NoSync));
    }
}
