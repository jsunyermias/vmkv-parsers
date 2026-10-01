//! DTS-HD extension substream header (ETSI TS 102 114 §7.5): only the
//! fields needed to delimit the substream and, from the first audio asset
//! descriptor, the sample rate, channel count and PCM resolution the
//! decoded track has.

/// Sample rates indexed by the 4-bit code of an asset descriptor.
const RATES: [u32; 16] = [
    8000, 16000, 32000, 64000, 128000, 22050, 44100, 88200, 176400, 352800, 12000, 24000, 48000, 96000, 192000, 384000,
];

/// MSB-first reader that reports running past the end instead of panicking.
struct Bits<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn take(&mut self, n: usize) -> Result<u32, String> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.b.get(self.pos / 8).ok_or("extension substream header ends early")?;
            v = v << 1 | (byte >> (7 - self.pos % 8) & 1) as u32;
            self.pos += 1;
        }
        Ok(v)
    }

    fn skip(&mut self, n: usize) -> Result<(), String> {
        if self.pos + n > self.b.len() * 8 {
            return Err("extension substream header ends early".into());
        }
        self.pos += n;
        Ok(())
    }
}

/// What the decoded audio of an asset is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Asset {
    pub sample_rate: u32,
    pub channels: u32,
    pub pcm_bits: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exss {
    /// Bytes of the header.
    pub header_size: u64,
    /// Bytes of the whole substream, header included.
    pub size: u64,
    /// `None` when the header carries no static fields.
    pub asset: Option<Asset>,
}

/// The sizes of the substream starting at `b[0]` (its sync word), from
/// its first 10 bytes.
pub fn sizes(b: &[u8]) -> Result<(u64, u64), String> {
    let mut r = Bits { b, pos: 32 + 8 };
    r.take(2)?; // substream index
    let wide = r.take(1)? as usize;
    let header_size = r.take(8 + 4 * wide)? as u64 + 1;
    let size = r.take(16 + 4 * wide)? as u64 + 1;
    if header_size > size {
        return Err(format!("extension substream header of {header_size} bytes in a substream of {size}"));
    }
    Ok((header_size, size))
}

/// Parses the whole header, `b` holding at least `header_size` bytes.
pub fn parse(b: &[u8]) -> Result<Exss, String> {
    let (header_size, size) = sizes(b)?;
    let mut r = Bits { b: &b[..(header_size as usize).min(b.len())], pos: 32 + 8 };
    let index = r.take(2)? as usize;
    let wide = r.take(1)? as usize;
    r.take(8 + 4 * wide)?;
    r.take(16 + 4 * wide)?;
    let static_fields = r.take(1)? == 1;
    let assets = if static_fields {
        r.skip(2 + 3)?; // reference clock code, frame duration
        if r.take(1)? == 1 {
            r.skip(36)?; // timecode
        }
        let presentations = r.take(3)? as usize + 1;
        let assets = r.take(3)? as usize + 1;
        let mut masks = Vec::with_capacity(presentations);
        for _ in 0..presentations {
            masks.push(r.take(index + 1)?);
        }
        for mask in masks {
            for j in 0..=index {
                if mask & (1 << j) != 0 {
                    r.skip(8)?; // active audio asset mask
                }
            }
        }
        if r.take(1)? == 1 {
            // Mixing metadata.
            r.skip(2)?;
            let mask_bits = (r.take(2)? as usize + 1) << 2;
            let configs = r.take(2)? as usize + 1;
            r.skip(configs * mask_bits)?;
        }
        assets
    } else {
        1
    };
    if assets > 1 {
        return Err(format!("{assets} audio assets in one extension substream"));
    }
    r.take(16 + 4 * wide)?; // asset size
                            // Audio asset descriptor.
    r.take(9)?; // descriptor size
    r.take(3)?; // asset index
    let asset = if static_fields {
        if r.take(1)? == 1 {
            r.skip(4)?; // asset type
        }
        if r.take(1)? == 1 {
            r.skip(24)?; // language
        }
        if r.take(1)? == 1 {
            let text = r.take(10)? as usize + 1;
            r.skip(text * 8)?;
        }
        let pcm_bits = r.take(5)? + 1;
        let sample_rate = RATES[r.take(4)? as usize];
        let channels = r.take(8)? + 1;
        Some(Asset { sample_rate, channels, pcm_bits })
    } else {
        None
    };
    Ok(Exss { header_size, size, asset })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The extension substream header of a real DTS-HD MA 5.1 track, 16 bit
    /// at 48 kHz (only metadata: sizes, masks and the asset descriptor).
    const MA51: [u8; 32] = [
        0x64, 0x58, 0x20, 0x25, 0x00, 0x03, 0xe0, 0x08, 0x78, 0x00, 0x80, 0x80, 0x08, 0xc1, 0xa0, 0x3f, 0x01, 0x69,
        0xe0, 0x08, 0x04, 0x00, 0x8e, 0x00, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x5d, 0xf3,
    ];

    #[test]
    fn real_ma_header() {
        assert_eq!(sizes(&MA51), Ok((32, 68)));
        let x = parse(&MA51).unwrap();
        assert_eq!(
            x,
            Exss { header_size: 32, size: 68, asset: Some(Asset { sample_rate: 48000, channels: 6, pcm_bits: 16 }) }
        );
        assert!(parse(&MA51[..16]).unwrap_err().contains("ends early"));
    }
}
