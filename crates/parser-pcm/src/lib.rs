//! PCM in WAV (RIFF), RF64 and BW64 (`A_PCM/INT/LIT`, `A_PCM/FLOAT/IEEE`).
//!
//! - Formats: `WAVE_FORMAT_PCM`, `WAVE_FORMAT_IEEE_FLOAT` and
//!   `WAVE_FORMAT_EXTENSIBLE` with either subformat. Anything else (ADPCM,
//!   A-law, μ-law, compressed audio in WAV) is `UNSUPPORTED_CODEC_VARIANT`.
//! - Integer PCM maps to `A_PCM/INT/LIT` (8-bit WAV samples are unsigned,
//!   as that mapping reads them at bit depth 8); float to `A_PCM/FLOAT/IEEE`.
//! - `bit_depth` is the container size of a sample (`block_align` /
//!   channels × 8): fewer valid bits are stored left-justified in it, which
//!   is how Matroska reads them too.
//! - PCM has no frames: each unit holds `--unit-samples` samples (by
//!   default a 25th of the sample rate, 40 ms), the last one what is left.
//!   Times are exact sample positions (decision 60).
//! - A `data` chunk declaring more bytes than the source holds is
//!   `TRUNCATED_BITSTREAM`, unless `--truncated-data keep`.

use vtj::cli::{ParamSpec, ParseError};
use vtj::*;

const FORMAT_PCM: u16 = 1;
const FORMAT_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xfffe;
/// Tail shared by every `KSDATAFORMAT_SUBTYPE_*` GUID after its first two
/// bytes (the format tag).
const GUID_TAIL: [u8; 14] = [0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71];
const MAX_UNIT_SAMPLES: i64 = 65536;

const UNIT_SAMPLES: ParamSpec =
    ParamSpec::int("unit_samples", 1, MAX_UNIT_SAMPLES, "samples per unit (the last unit holds what is left)")
        .default("sample rate / 25 (40 ms)");
const TRUNCATED_DATA: ParamSpec = ParamSpec::choice(
    "truncated_data",
    &["error", "keep"],
    "a data chunk longer than the source: error, or keep the whole sample frames present",
)
.default("error");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub float: bool,
    pub channels: u16,
    pub sample_rate: u32,
    pub block_align: u16,
    /// Bits per sample in its container (`block_align` / channels × 8).
    pub container_bits: u16,
}

impl Format {
    pub fn codec_id(&self) -> &'static str {
        if self.float {
            "A_PCM/FLOAT/IEEE"
        } else {
            "A_PCM/INT/LIT"
        }
    }
}

/// Parses a `fmt ` chunk body.
pub fn parse_fmt(b: &[u8]) -> Result<Format, ParseError> {
    if b.len() < 16 {
        return Err(ParseError::invalid(format!("fmt chunk is {} bytes, fewer than 16", b.len())));
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let mut tag = u16_at(0);
    let channels = u16_at(2);
    let sample_rate = u32::from_le_bytes(b[4..8].try_into().expect("4 bytes"));
    let block_align = u16_at(12);
    let bits = u16_at(14);
    let mut valid_bits = bits;
    if tag == FORMAT_EXTENSIBLE {
        if b.len() < 40 || u16_at(16) < 22 {
            return Err(ParseError::invalid("WAVE_FORMAT_EXTENSIBLE fmt chunk is too short"));
        }
        valid_bits = u16_at(18);
        if b[26..40] != GUID_TAIL {
            return Err(ParseError::new(
                ErrorCode::UnsupportedCodecVariant,
                "WAVE_FORMAT_EXTENSIBLE with a non-PCM subformat",
            ));
        }
        tag = u16_at(24);
        if valid_bits == 0 {
            valid_bits = bits;
        }
    }
    let float = match tag {
        FORMAT_PCM => false,
        FORMAT_FLOAT => true,
        t => {
            return Err(ParseError::new(
                ErrorCode::UnsupportedCodecVariant,
                format!("WAV format tag 0x{t:04x} is not PCM"),
            ))
        }
    };
    if channels == 0 {
        return Err(ParseError::invalid("fmt chunk declares 0 channels"));
    }
    if sample_rate == 0 {
        return Err(ParseError::invalid("fmt chunk declares sample rate 0"));
    }
    if block_align == 0 || block_align % channels != 0 {
        return Err(ParseError::invalid(format!("block align {block_align} is not a multiple of {channels} channels")));
    }
    let container_bits = block_align / channels * 8;
    if bits == 0 || bits > container_bits || valid_bits > bits {
        return Err(ParseError::invalid(format!(
            "{bits} bits per sample ({valid_bits} valid) do not fit block align {block_align} for {channels} channels"
        )));
    }
    // A container wider than the declared sample size rounded up to bytes
    // would be padding Matroska cannot express.
    if container_bits != bits.div_ceil(8) * 8 {
        return Err(ParseError::unsupported(format!("{bits}-bit samples in {container_bits}-bit containers")));
    }
    let supported = if float { matches!(container_bits, 32 | 64) } else { matches!(container_bits, 8 | 16 | 24 | 32) };
    if !supported {
        return Err(ParseError::unsupported(format!(
            "{container_bits}-bit {} samples",
            if float { "float" } else { "integer" }
        )));
    }
    Ok(Format { float, channels, sample_rate, block_align, container_bits })
}

pub struct Pcm;

struct Chunk4 {
    id: [u8; 4],
    size: u64,
    body: u64,
}

fn read_chunk(src: &mut vtj::source::SourceFile, pos: u64) -> Result<Option<Chunk4>, ParseError> {
    let size = src.size();
    if size == pos {
        return Ok(None);
    }
    if size - pos < 8 {
        return Err(ParseError::truncated(format!("chunk header at byte {pos} cut at byte {size}")));
    }
    let mut h = [0u8; 8];
    src.read_at(pos, &mut h)?;
    Ok(Some(Chunk4 {
        id: h[..4].try_into().expect("4 bytes"),
        size: u32::from_le_bytes(h[4..8].try_into().expect("4 bytes")) as u64,
        body: pos + 8,
    }))
}

impl Parser for Pcm {
    fn name(&self) -> &'static str {
        "vmkv-parser-pcm"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn params(&self) -> &'static [ParamSpec] {
        &[UNIT_SAMPLES, TRUNCATED_DATA]
    }

    fn parse(&self, ctx: &mut Context<'_>) -> Result<Track, ParseError> {
        let keep_truncated = ctx.param_str("truncated_data") == Some("keep");
        let unit_param = ctx.param_int("unit_samples");
        let src = ctx.source(0);
        let size = src.size();
        let mut h = [0u8; 12];
        if size < 12 {
            return Err(ParseError::new(ErrorCode::MissingInitializationData, "no RIFF/RF64 WAVE header"));
        }
        src.read_at(0, &mut h)?;
        let rf64 = match &h[..4] {
            b"RIFF" => false,
            b"RF64" | b"BW64" => true,
            b"RIFX" => return Err(ParseError::new(ErrorCode::UnsupportedCodecVariant, "big-endian RIFX WAV")),
            _ => return Err(ParseError::new(ErrorCode::MissingInitializationData, "no RIFF/RF64 WAVE header")),
        };
        // RF64 puts 0xffffffff here; its real size is in ds64.
        let riff_end = if rf64 { 0 } else { 8 + u32::from_le_bytes(h[4..8].try_into().expect("4 bytes")) as u64 };
        if &h[8..12] != b"WAVE" {
            return Err(ParseError::new(ErrorCode::MissingInitializationData, "RIFF file is not WAVE"));
        }

        // RF64/BW64: the 64-bit data size lives in the first chunk, `ds64`.
        let mut ds64_data: Option<u64> = None;
        let mut ds64_samples = 0u64;
        let mut pos = 12u64;
        if rf64 {
            let c = read_chunk(src, pos)?.ok_or_else(|| ParseError::truncated("ds64 chunk cut short"))?;
            if &c.id != b"ds64" || c.size < 24 {
                return Err(ParseError::invalid("RF64 file does not start with a ds64 chunk"));
            }
            if size - c.body < 24 {
                return Err(ParseError::truncated("ds64 chunk cut short"));
            }
            let mut d = [0u8; 24];
            src.read_at(c.body, &mut d)?;
            ds64_data = Some(u64::from_le_bytes(d[8..16].try_into().expect("8 bytes")));
            ds64_samples = u64::from_le_bytes(d[16..24].try_into().expect("8 bytes"));
            pos = c.body + c.size + (c.size & 1);
        }

        let mut format: Option<Format> = None;
        let (data_start, mut data_len) = loop {
            let Some(c) = read_chunk(src, pos)? else {
                // A RIFF size past the end of the source means the file was
                // cut before its data chunk.
                return Err(match format {
                    _ if riff_end > size => ParseError::truncated(format!(
                        "the source ends at byte {size}, before the data chunk; RIFF declares {riff_end} bytes"
                    )),
                    None => ParseError::new(ErrorCode::MissingInitializationData, "no fmt chunk"),
                    Some(_) => ParseError::invalid("no data chunk"),
                });
            };
            match &c.id {
                b"fmt " => {
                    if format.is_some() {
                        return Err(ParseError::invalid(format!("second fmt chunk at byte {pos}")));
                    }
                    if c.size > 1024 {
                        return Err(ParseError::invalid(format!("fmt chunk is {} bytes", c.size)));
                    }
                    if size - c.body < c.size {
                        return Err(ParseError::truncated("fmt chunk cut short"));
                    }
                    let mut b = vec![0u8; c.size as usize];
                    src.read_at(c.body, &mut b)?;
                    format = Some(parse_fmt(&b)?);
                }
                b"data" => {
                    if format.is_none() {
                        return Err(ParseError::new(
                            ErrorCode::MissingInitializationData,
                            "data chunk before fmt chunk",
                        ));
                    }
                    let len = match ds64_data {
                        Some(d) if c.size == 0xffff_ffff => d,
                        _ => c.size,
                    };
                    break (c.body, len);
                }
                _ => {}
            }
            pos = c.body.saturating_add(c.size + (c.size & 1));
            if pos > size {
                return Err(ParseError::truncated(format!(
                    "chunk at byte {} runs past the end of the source",
                    c.body - 8
                )));
            }
        };
        let f = format.expect("checked before the data chunk");
        let align = f.block_align as u64;
        // Some writers (FFmpeg among them) count the pad byte of an
        // odd-sized data chunk in the ds64 data size; its sample count then
        // says where the audio really ends (decision 60).
        if ds64_data.is_some() && ds64_samples != 0 {
            let exact = ds64_samples.saturating_mul(align);
            if exact % 2 == 1 && data_len == exact + 1 {
                data_len = exact;
            }
        }

        if data_len > size - data_start {
            if !keep_truncated {
                return Err(ParseError::truncated(format!(
                    "data chunk declares {data_len} bytes, the source holds {}",
                    size - data_start
                )));
            }
            data_len = (size - data_start) / align * align;
        }
        if data_len % align != 0 {
            return Err(ParseError::invalid(format!(
                "data chunk of {data_len} bytes is not a multiple of block align {align}"
            )));
        }
        let total = data_len / align;
        if total == 0 {
            return Err(ParseError::invalid("data chunk holds no samples"));
        }

        let per_unit = match unit_param {
            Some(n) => n as u64,
            None => (f.sample_rate as u64 / 25).clamp(1, MAX_UNIT_SAMPLES as u64),
        };
        let rate = Rational::new(f.sample_rate as i64, 1);
        let mut timeline = Timeline::new(rate, 0)?;
        let flags = Flags::NONE.with(Flag::RandomAccess);
        let mut done = 0u64;
        while done < total {
            let n = per_unit.min(total - done);
            let (pts, dur) = timeline.advance(n as i128)?;
            ctx.emit(&Unit::new(pts, dur, flags, vec![Chunk::src(0, data_start + done * align, n * align)]))?;
            done += n;
        }

        let mut track = Track::new(TrackType::Audio, f.codec_id());
        let mut audio = Audio::new(rate, f.channels as u64);
        audio.bit_depth = Some(f.container_bits as u64);
        track.audio = Some(audio);
        Ok(track)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(tag: u16, ch: u16, rate: u32, align: u16, bits: u16) -> Vec<u8> {
        let mut b = tag.to_le_bytes().to_vec();
        b.extend(ch.to_le_bytes());
        b.extend(rate.to_le_bytes());
        b.extend((rate * align as u32).to_le_bytes());
        b.extend(align.to_le_bytes());
        b.extend(bits.to_le_bytes());
        b
    }

    fn extensible(ch: u16, align: u16, bits: u16, valid: u16, sub: u16) -> Vec<u8> {
        let mut b = fmt(FORMAT_EXTENSIBLE, ch, 48000, align, bits);
        b.extend(22u16.to_le_bytes());
        b.extend(valid.to_le_bytes());
        b.extend(3u32.to_le_bytes());
        b.extend(sub.to_le_bytes());
        b.extend(GUID_TAIL);
        b
    }

    #[test]
    fn formats() {
        let f = parse_fmt(&fmt(1, 2, 44100, 4, 16)).unwrap();
        assert_eq!((f.codec_id(), f.container_bits), ("A_PCM/INT/LIT", 16));
        assert_eq!(parse_fmt(&fmt(3, 1, 96000, 4, 32)).unwrap().codec_id(), "A_PCM/FLOAT/IEEE");
        // 20 valid bits in 24-bit containers, extensible.
        let f = parse_fmt(&extensible(2, 6, 24, 20, 1)).unwrap();
        assert_eq!((f.float, f.container_bits), (false, 24));
        assert!(parse_fmt(&extensible(2, 8, 32, 32, 3)).unwrap().float);
        // 12 bits stored in 16-bit containers.
        assert_eq!(parse_fmt(&fmt(1, 1, 8000, 2, 12)).unwrap().container_bits, 16);
    }

    #[test]
    fn rejected_formats() {
        let code = |b: &[u8]| parse_fmt(b).unwrap_err().code;
        assert_eq!(code(&fmt(7, 1, 8000, 1, 8)), ErrorCode::UnsupportedCodecVariant, "mu-law");
        assert_eq!(code(&fmt(0x55, 2, 44100, 1, 0)), ErrorCode::UnsupportedCodecVariant, "MP3 in WAV");
        let mut ext = extensible(2, 4, 16, 16, 1);
        ext[26] = 0xff;
        assert_eq!(code(&ext), ErrorCode::UnsupportedCodecVariant);
        assert_eq!(code(&fmt(1, 2, 44100, 3, 8)), ErrorCode::InvalidBitstream, "align not per channel");
        assert_eq!(code(&fmt(1, 2, 44100, 4, 24)), ErrorCode::InvalidBitstream, "24 bits in 16");
        assert_eq!(code(&fmt(1, 1, 8000, 4, 16)), ErrorCode::UnsupportedFeature, "16 bits in 32");
        assert_eq!(code(&fmt(3, 1, 8000, 2, 16)), ErrorCode::UnsupportedFeature, "16-bit float");
        assert_eq!(code(&fmt(1, 1, 8000, 8, 64)), ErrorCode::UnsupportedFeature, "64-bit integer");
        assert_eq!(code(&fmt(1, 0, 8000, 2, 16)), ErrorCode::InvalidBitstream);
        assert_eq!(code(&fmt(1, 1, 0, 2, 16)), ErrorCode::InvalidBitstream);
    }
}
