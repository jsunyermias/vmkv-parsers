//! Dolby TrueHD (`A_TRUEHD`), as a raw `.thd` stream of access units.
//!
//! - Each unit is one access unit, as the Matroska mapping defines: a
//!   4-byte header whose low 12 bits of the first word give the length in
//!   16-bit words. An access unit lasts `40 << (rate code & 7)` samples
//!   (1/1200 s at 48 kHz).
//! - Units that carry a major sync (`F8726FBA`) are the random access
//!   points; the first access unit must carry one, since it holds the
//!   sample rate, the channel layouts and the substream count.
//! - Channels are those of the widest presentation in the major sync: the
//!   8-channel one when it is used, otherwise the 6-channel one (an Atmos
//!   stream reports its 7.1 bed, as FFmpeg does).
//! - Each access unit's check nibble (parity over its header and its
//!   substream directory) is verified, as decoders do.
//! - Rejected: MLP (`F8726FBB`, `UNSUPPORTED_CODEC_VARIANT`), a reserved
//!   rate code, and a major sync that changes the rate, the channels or
//!   the substream count (decision 70).

use vtj::cli::ParseError;
use vtj::*;

const SYNC_TRUEHD: u32 = 0xf872_6fba;
const SYNC_MLP: u32 = 0xf872_6fbb;
const MAJOR_SYNC: usize = 28;
/// Channels per bit of a TrueHD channel assignment, from bit 0: L/R, C,
/// LFE, Ls/Rs, Lvh/Rvh, Lc/Rc, Lrs/Rrs, Cs, Ts, Lsd/Rsd, Lw/Rw, Cvh, LFE2.
const CHANNELS_PER_BIT: [u32; 13] = [2, 1, 1, 2, 2, 2, 2, 1, 1, 2, 2, 1, 1];

fn channels(assignment: u32) -> u32 {
    (0..13).filter(|i| assignment >> i & 1 == 1).map(|i| CHANNELS_PER_BIT[i]).sum()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MajorSync {
    pub sample_rate: u32,
    /// Samples per access unit.
    pub samples: u32,
    pub channels: u32,
    pub substreams: u32,
    /// Bytes of the major sync, extensions included.
    pub size: usize,
}

/// Parses a major sync starting at `b[0]` (its format sync).
pub fn parse_major_sync(b: &[u8]) -> Result<MajorSync, String> {
    if b.len() < MAJOR_SYNC {
        return Err("major sync cut short".into());
    }
    match u32::from_be_bytes(b[..4].try_into().expect("4 bytes")) {
        SYNC_TRUEHD => {}
        SYNC_MLP => return Err("MLP".into()),
        _ => return Err("no major sync".into()),
    }
    let rate = (b[4] >> 4) as u32;
    if rate == 0xf || rate & 7 > 2 {
        return Err(format!("reserved sample rate code {rate}"));
    }
    let sample_rate = if rate & 8 != 0 { 44100 } else { 48000 } << (rate & 7);
    let samples = 40 << (rate & 7);
    let w = u32::from_be_bytes(b[4..8].try_into().expect("4 bytes"));
    let six = w >> 15 & 0x1f;
    let eight = w & 0x1fff;
    if u16::from_be_bytes([b[8], b[9]]) != 0xb752 {
        return Err("major sync signature is not B752".into());
    }
    let substreams = (b[16] >> 4) as u32;
    if substreams == 0 {
        return Err("major sync declares 0 substreams".into());
    }
    let size = if b[25] & 1 == 1 { MAJOR_SYNC + 2 + (b[26] >> 4) as usize * 2 } else { MAJOR_SYNC };
    let channels = if eight != 0 { channels(eight) } else { channels(six) };
    Ok(MajorSync { sample_rate, samples, channels, substreams, size })
}

/// Whether the check nibble of the access unit `au` holds: the XOR of the
/// nibbles of its 4-byte header and of its substream directory is 0xF.
fn parity_ok(au: &[u8], directory_at: usize, substreams: u32) -> Result<bool, String> {
    let mut x = au[..4].iter().fold(0u8, |a, b| a ^ b);
    let mut p = directory_at;
    for _ in 0..substreams {
        let entry = au.get(p..p + 2).ok_or("substream directory runs past the access unit")?;
        let extra = entry[0] & 0x80 != 0;
        let len = if extra { 4 } else { 2 };
        let entry = au.get(p..p + len).ok_or("substream directory runs past the access unit")?;
        x = entry.iter().fold(x, |a, b| a ^ b);
        p += len;
    }
    Ok((x >> 4 ^ x) & 0xf == 0xf)
}

pub struct TrueHd;

impl Parser for TrueHd {
    fn name(&self) -> &'static str {
        "vmkv-parser-truehd"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let size = ctx.source(0).size();
        let mut first: Option<MajorSync> = None;
        let mut timeline: Option<Timeline> = None;
        let mut au = Vec::new();
        let mut pos = 0u64;
        let mut count = 0u64;

        while pos < size {
            if size - pos < 4 {
                return Err(ParseError::truncated(format!("access unit {count} header cut at byte {size}")));
            }
            let mut h = [0u8; 4];
            ctx.source(0).read_at(pos, &mut h)?;
            let len = (u16::from_be_bytes([h[0], h[1]]) & 0xfff) as u64 * 2;
            let at = format!("access unit {count} at byte {pos}");
            if len < 4 {
                return Err(ParseError::invalid(format!("{at} is {len} bytes")));
            }
            if len > size - pos {
                return Err(ParseError::truncated(format!("access unit {count} cut at byte {size}")));
            }
            au.resize(len as usize, 0);
            ctx.source(0).read_at(pos, &mut au)?;

            let major = if au.len() >= 8 && au[4..8] == SYNC_TRUEHD.to_be_bytes()
                || au.len() >= 8 && au[4..8] == SYNC_MLP.to_be_bytes()
            {
                let m = parse_major_sync(&au[4..]).map_err(|e| match e.as_str() {
                    "MLP" => ParseError::new(ErrorCode::UnsupportedCodecVariant, format!("{at} is MLP, not TrueHD")),
                    _ => ParseError::invalid(format!("{at}: {e}")),
                })?;
                match first {
                    None => {
                        first = Some(m);
                        timeline = Some(Timeline::new(Rational::new(m.sample_rate as i64, 1), 0)?);
                    }
                    Some(f)
                        if (f.sample_rate, f.channels, f.substreams) != (m.sample_rate, m.channels, m.substreams) =>
                    {
                        return Err(ParseError::new(
                            ErrorCode::InconsistentTrackParameters,
                            format!(
                                "{at} changes sample rate/channels/substreams from {:?} to {:?}",
                                (f.sample_rate, f.channels, f.substreams),
                                (m.sample_rate, m.channels, m.substreams)
                            ),
                        ));
                    }
                    Some(_) => {}
                }
                Some(m)
            } else {
                None
            };
            let Some(stream) = first else {
                return Err(ParseError::new(
                    ErrorCode::MissingInitializationData,
                    "the first access unit carries no major sync",
                ));
            };
            let directory = 4 + major.map_or(0, |m| m.size);
            let ok =
                parity_ok(&au, directory, stream.substreams).map_err(|e| ParseError::invalid(format!("{at}: {e}")))?;
            if !ok {
                return Err(ParseError::invalid(format!("{at}: check nibble mismatch")));
            }

            let flags = if major.is_some() { Flags::NONE.with(Flag::RandomAccess) } else { Flags::NONE };
            let (pts, dur) =
                timeline.as_mut().expect("set with the first major sync").advance(stream.samples as i128)?;
            ctx.emit(&Unit::new(pts, dur, flags, vec![Chunk::src(0, pos, len)]))?;
            count += 1;
            pos += len;
        }

        let Some(first) = first else {
            return Err(ParseError::invalid("no TrueHD access units"));
        };
        let mut track = Track::new(TrackType::Audio, "A_TRUEHD");
        track.audio = Some(Audio::new(Rational::new(first.sample_rate as i64, 1), first.channels as u64));
        Ok(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The major sync of a real TrueHD 5.1 track at 48 kHz.
    const REAL: [u8; 28] = [
        0xf8, 0x72, 0x6f, 0xba, 0x00, 0x57, 0xa0, 0x0f, 0xb7, 0x52, 0x00, 0x00, 0x00, 0x00, 0x83, 0xc2, 0x20, 0x3c,
        0x00, 0x00, 0x42, 0xee, 0xe3, 0x06, 0xe3, 0x00, 0x78, 0x33,
    ];

    #[test]
    fn real_major_sync() {
        assert_eq!(
            parse_major_sync(&REAL),
            Ok(MajorSync { sample_rate: 48000, samples: 40, channels: 6, substreams: 2, size: 28 })
        );
        let mut mlp = REAL;
        mlp[3] = 0xbb;
        assert_eq!(parse_major_sync(&mlp), Err("MLP".into()));
        let mut sig = REAL;
        sig[8] = 0;
        assert!(parse_major_sync(&sig).unwrap_err().contains("B752"));
        let mut rate = REAL;
        rate[4] = 0x30;
        assert!(parse_major_sync(&rate).unwrap_err().contains("reserved sample rate"));
    }

    #[test]
    fn channel_assignments() {
        assert_eq!(channels(0b1111), 6, "5.1");
        assert_eq!(channels(0b100_1111), 8, "7.1 with Lrs/Rrs");
        assert_eq!(channels(0b1), 2);
    }
}
