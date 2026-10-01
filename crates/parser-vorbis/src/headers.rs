//! Vorbis I header packets (Vorbis I specification, §4.2 and §3.2.1).
//!
//! The setup header is walked field by field to its end, because the
//! mode configurations whose block flags give each audio packet its
//! duration come after every codebook, floor, residue and mapping, all of
//! variable length. Nothing is decoded beyond what is needed to skip each
//! structure and check that it is well formed.

/// LSB-first bit reader, as Vorbis packs its fields.
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: u64,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    /// Reads `n` ≤ 32 bits; `None` past the end of the packet.
    pub fn read(&mut self, n: u32) -> Option<u32> {
        debug_assert!(n <= 32);
        if self.pos + n as u64 > self.data.len() as u64 * 8 {
            return None;
        }
        let mut v = 0u64;
        for i in 0..n as u64 {
            let p = self.pos + i;
            let bit = (self.data[(p / 8) as usize] >> (p % 8)) & 1;
            v |= (bit as u64) << i;
        }
        self.pos += n as u64;
        Some(v as u32)
    }

    pub fn skip(&mut self, n: u64) -> Option<()> {
        let end = self.pos.checked_add(n)?;
        if end > self.data.len() as u64 * 8 {
            return None;
        }
        self.pos = end;
        Some(())
    }
}

/// Bits needed to store `v` (`ilog` in the specification).
pub fn ilog(v: u32) -> u32 {
    32 - v.leading_zeros()
}

/// The greatest `r` with `r^dims ≤ entries` (`lookup1_values`).
pub fn lookup1_values(entries: u32, dims: u32) -> u32 {
    if dims == 0 {
        return 0;
    }
    let fits = |r: u64| {
        let mut acc: u64 = 1;
        for _ in 0..dims {
            acc = match acc.checked_mul(r) {
                Some(a) if a <= entries as u64 => a,
                _ => return false,
            };
        }
        true
    };
    let mut r = (entries as f64).powf(1.0 / dims as f64).floor() as u64;
    while r > 0 && !fits(r) {
        r -= 1;
    }
    while fits(r + 1) {
        r += 1;
    }
    r as u32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Identification {
    pub channels: u8,
    pub sample_rate: u32,
    pub blocksize_0: u32,
    pub blocksize_1: u32,
}

fn check_header(b: &[u8], kind: u8) -> Result<(), String> {
    if b.len() < 7 || b[0] != kind || &b[1..7] != b"vorbis" {
        return Err(format!("not a Vorbis header of type {kind}"));
    }
    Ok(())
}

pub fn parse_identification(b: &[u8]) -> Result<Identification, String> {
    check_header(b, 1)?;
    if b.len() < 30 {
        return Err(format!("identification header is {} bytes, not 30", b.len()));
    }
    let version = u32::from_le_bytes(b[7..11].try_into().expect("4 bytes"));
    if version != 0 {
        return Err(format!("Vorbis version {version}"));
    }
    let channels = b[11];
    let sample_rate = u32::from_le_bytes(b[12..16].try_into().expect("4 bytes"));
    let (e0, e1) = ((b[28] & 15) as u32, (b[28] >> 4) as u32);
    if channels == 0 || sample_rate == 0 {
        return Err("identification header declares 0 channels or sample rate 0".into());
    }
    if !(6..=13).contains(&e0) || !(6..=13).contains(&e1) || e0 > e1 {
        return Err(format!("block sizes 2^{e0} and 2^{e1} are invalid"));
    }
    if b[29] & 1 == 0 {
        return Err("identification header framing bit is not set".into());
    }
    Ok(Identification { channels, sample_rate, blocksize_0: 1 << e0, blocksize_1: 1 << e1 })
}

pub fn check_comment(b: &[u8]) -> Result<(), String> {
    check_header(b, 3)
}

/// Parses the setup header and returns the block flag of each mode.
pub fn parse_setup(b: &[u8], channels: u8) -> Result<Vec<bool>, String> {
    check_header(b, 5)?;
    let mut r = BitReader::new(&b[7..]);
    let short = || "setup header ends early".to_string();
    macro_rules! read {
        ($n:expr) => {
            r.read($n).ok_or_else(short)?
        };
    }

    let codebooks = read!(8) + 1;
    for i in 0..codebooks {
        if read!(24) != 0x564342 {
            return Err(format!("codebook {i} has no sync pattern"));
        }
        let dims = read!(16);
        let entries = read!(24);
        if read!(1) == 1 {
            // Ordered: runs of entries with increasing lengths.
            let mut current = 0u32;
            let mut length = read!(5) + 1;
            while current < entries {
                let n = read!(ilog(entries - current));
                current = current
                    .checked_add(n)
                    .filter(|&c| c <= entries)
                    .ok_or_else(|| format!("codebook {i} has too many entries"))?;
                length += 1;
                if length > 32 && current < entries {
                    return Err(format!("codebook {i} has a codeword longer than 32 bits"));
                }
            }
        } else {
            let sparse = read!(1) == 1;
            for _ in 0..entries {
                if !sparse || read!(1) == 1 {
                    read!(5);
                }
            }
        }
        match read!(4) {
            0 => {}
            kind @ (1 | 2) => {
                read!(32); // minimum value
                read!(32); // delta value
                let value_bits = read!(4) + 1;
                read!(1); // sequence_p
                let values =
                    if kind == 1 { lookup1_values(entries, dims) as u64 } else { entries as u64 * dims as u64 };
                r.skip(values * value_bits as u64).ok_or_else(short)?;
            }
            t => return Err(format!("codebook {i} has lookup type {t}")),
        }
    }
    let book = |n: u32, what: &str| -> Result<(), String> {
        if n >= codebooks {
            return Err(format!("{what} uses codebook {n} of {codebooks}"));
        }
        Ok(())
    };

    for _ in 0..read!(6) + 1 {
        if read!(16) != 0 {
            return Err("time domain transform is not 0".into());
        }
    }

    let floors = read!(6) + 1;
    for i in 0..floors {
        match read!(16) {
            0 => {
                // order, rate, bark map size, amplitude bits and offset
                r.skip(8 + 16 + 16 + 6 + 8).ok_or_else(short)?;
                for _ in 0..read!(4) + 1 {
                    book(read!(8), &format!("floor {i}"))?;
                }
            }
            1 => {
                let partitions = read!(5);
                let mut classes = Vec::new();
                for _ in 0..partitions {
                    classes.push(read!(4));
                }
                let class_count = classes.iter().max().map_or(0, |&m| m + 1);
                let mut dims = vec![0u32; class_count as usize];
                for d in dims.iter_mut() {
                    *d = read!(3) + 1;
                    let subclasses = read!(2);
                    if subclasses != 0 {
                        book(read!(8), &format!("floor {i}"))?;
                    }
                    for _ in 0..1u32 << subclasses {
                        let b = read!(8);
                        if b != 0 {
                            book(b - 1, &format!("floor {i}"))?;
                        }
                    }
                }
                read!(2); // multiplier
                let range_bits = read!(4);
                let mut points = 2u32;
                for &c in &classes {
                    for _ in 0..dims[c as usize] {
                        read!(range_bits);
                        points += 1;
                    }
                }
                if points > 65 {
                    return Err(format!("floor {i} has {points} points, more than 65"));
                }
            }
            t => return Err(format!("floor {i} has type {t}")),
        }
    }

    let residues = read!(6) + 1;
    for i in 0..residues {
        let t = read!(16);
        if t > 2 {
            return Err(format!("residue {i} has type {t}"));
        }
        r.skip(24 + 24).ok_or_else(short)?; // begin, end
        read!(24); // partition size
        let classifications = read!(6) + 1;
        book(read!(8), &format!("residue {i}"))?;
        let mut cascade = Vec::new();
        for _ in 0..classifications {
            let low = read!(3);
            let high = if read!(1) == 1 { read!(5) } else { 0 };
            cascade.push(high << 3 | low);
        }
        for c in cascade {
            for j in 0..8 {
                if c & (1 << j) != 0 {
                    book(read!(8), &format!("residue {i}"))?;
                }
            }
        }
    }

    let mappings = read!(6) + 1;
    let channel_bits = ilog(channels as u32 - 1);
    for i in 0..mappings {
        if read!(16) != 0 {
            return Err(format!("mapping {i} is not type 0"));
        }
        let submaps = if read!(1) == 1 { read!(4) + 1 } else { 1 };
        if read!(1) == 1 {
            for _ in 0..read!(8) + 1 {
                let (m, a) = (read!(channel_bits), read!(channel_bits));
                if m == a || m >= channels as u32 || a >= channels as u32 {
                    return Err(format!("mapping {i} couples channels {m} and {a}"));
                }
            }
        }
        if read!(2) != 0 {
            return Err(format!("mapping {i} has reserved bits set"));
        }
        if submaps > 1 {
            for _ in 0..channels {
                if read!(4) >= submaps {
                    return Err(format!("mapping {i} uses a submap it does not have"));
                }
            }
        }
        for _ in 0..submaps {
            read!(8);
            if read!(8) >= floors {
                return Err(format!("mapping {i} uses a floor it does not have"));
            }
            if read!(8) >= residues {
                return Err(format!("mapping {i} uses a residue it does not have"));
            }
        }
    }

    let modes = read!(6) + 1;
    let mut flags = Vec::new();
    for i in 0..modes {
        let blockflag = read!(1) == 1;
        if read!(16) != 0 || read!(16) != 0 {
            return Err(format!("mode {i} has a non-zero window or transform type"));
        }
        if read!(8) >= mappings {
            return Err(format!("mode {i} uses a mapping it does not have"));
        }
        flags.push(blockflag);
    }
    if read!(1) != 1 {
        return Err("setup header framing bit is not set".into());
    }
    Ok(flags)
}

/// Block size of an audio packet from its first bits, or why it is not one.
pub fn packet_blocksize(b: &[u8], id: &Identification, modes: &[bool]) -> Result<u32, String> {
    let mut r = BitReader::new(b);
    match r.read(1) {
        None => return Err("empty packet".into()),
        Some(1) => return Err("header packet among the audio packets".into()),
        Some(_) => {}
    }
    let mode = r.read(ilog(modes.len() as u32 - 1)).ok_or("packet ends before its mode number")?;
    let flag = *modes.get(mode as usize).ok_or_else(|| format!("mode {mode} of {}", modes.len()))?;
    Ok(if flag { id.blocksize_1 } else { id.blocksize_0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_reader_is_lsb_first() {
        let mut r = BitReader::new(&[0b1010_1101, 0xff]);
        assert_eq!(r.read(1), Some(1));
        assert_eq!(r.read(3), Some(0b110));
        assert_eq!(r.read(8), Some(0xfa));
        assert_eq!(r.read(5), None);
        assert_eq!(r.read(4), Some(0xf));
    }

    #[test]
    fn helpers() {
        assert_eq!([ilog(0), ilog(1), ilog(2), ilog(3), ilog(4), ilog(7), ilog(8)], [0, 1, 2, 2, 3, 3, 4]);
        assert_eq!(lookup1_values(81, 4), 3);
        assert_eq!(lookup1_values(80, 4), 2);
        assert_eq!(lookup1_values(1, 1), 1);
        assert_eq!(lookup1_values(u32::MAX >> 8, 1), u32::MAX >> 8);
        assert_eq!(lookup1_values(1000, 3), 10);
        assert_eq!(lookup1_values(999, 3), 9);
    }

    #[test]
    fn identification() {
        let mut b = vec![1];
        b.extend(b"vorbis");
        b.extend(0u32.to_le_bytes());
        b.push(2);
        b.extend(44100u32.to_le_bytes());
        b.extend([0; 12]);
        b.push(0xb8);
        b.push(1);
        let id = parse_identification(&b).unwrap();
        assert_eq!(id, Identification { channels: 2, sample_rate: 44100, blocksize_0: 256, blocksize_1: 2048 });
        b[28] = 0x8b;
        assert!(parse_identification(&b).unwrap_err().contains("block sizes"));
        b[28] = 0xb8;
        b[29] = 0;
        assert!(parse_identification(&b).unwrap_err().contains("framing"));
    }

    #[test]
    fn audio_packet_mode() {
        let id = Identification { channels: 1, sample_rate: 8000, blocksize_0: 64, blocksize_1: 512 };
        let modes = [false, true];
        assert_eq!(packet_blocksize(&[0b10], &id, &modes), Ok(512));
        assert_eq!(packet_blocksize(&[0b00], &id, &modes), Ok(64));
        assert!(packet_blocksize(&[0b1], &id, &modes).unwrap_err().contains("header packet"));
        assert!(packet_blocksize(&[], &id, &modes).is_err());
        assert!(packet_blocksize(&[0b110], &id, &[false, true, true]).unwrap_err().contains("mode 3 of 3"));
    }
}
