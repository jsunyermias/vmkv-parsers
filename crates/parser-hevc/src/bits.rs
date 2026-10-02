//! Bit-level reading of an RBSP (emulation prevention already removed):
//! `u(n)`, `ue(v)` and `se(v)` as defined in ITU-T H.265 §9.2, most
//! significant bit first.

/// Strips emulation prevention (`0x000003` → `0x0000`) from the bytes after
/// a NAL unit's one-byte header, producing the RBSP that exp-golomb fields
/// are read from. The original EBSP bytes (what a `src` chunk references)
/// are never altered; this is a parsing-only copy.
pub fn remove_emulation_prevention(ebsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ebsp.len());
    let mut zeros = 0u8;
    for &b in ebsp {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    pub fn bits_left(&self) -> usize {
        self.data.len() * 8 - self.pos.min(self.data.len() * 8)
    }

    pub fn u1(&mut self) -> Result<bool, String> {
        Ok(self.u(1)? != 0)
    }

    /// `n` bits (`0..=63`), most significant first.
    pub fn u(&mut self, n: u32) -> Result<u64, String> {
        if n as usize > self.bits_left() {
            return Err("bitstream ends inside a field".into());
        }
        let mut v = 0u64;
        for _ in 0..n {
            let byte = self.data[self.pos / 8];
            let bit = (byte >> (7 - (self.pos % 8))) & 1;
            v = (v << 1) | bit as u64;
            self.pos += 1;
        }
        Ok(v)
    }

    /// Exp-Golomb unsigned (H.265 §9.2): a run of `n` zero bits, a `1` bit,
    /// then `n` more bits; value is `2^n - 1 + those n bits`.
    pub fn ue(&mut self) -> Result<u64, String> {
        let mut leading_zeros = 0u32;
        while !self.u1()? {
            leading_zeros += 1;
            if leading_zeros > 32 {
                return Err("exp-Golomb code longer than 32 leading zero bits".into());
            }
        }
        if leading_zeros == 0 {
            return Ok(0);
        }
        let suffix = self.u(leading_zeros)?;
        Ok((1u64 << leading_zeros) - 1 + suffix)
    }

    /// Exp-Golomb signed (H.265 §9.2.2): `ue(v)` mapped to alternating
    /// positive/negative values (0, 1, -1, 2, -2, ...).
    pub fn se(&mut self) -> Result<i64, String> {
        let k = self.ue()?;
        if k % 2 == 0 {
            Ok(-((k / 2) as i64))
        } else {
            Ok(k.div_ceil(2) as i64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emulation_prevention_is_removed() {
        assert_eq!(remove_emulation_prevention(&[1, 0, 0, 3, 1, 0, 0, 3, 2]), [1, 0, 0, 1, 0, 0, 2]);
        assert_eq!(remove_emulation_prevention(&[0, 0, 0, 3, 3]), [0, 0, 0, 3]);
        assert_eq!(remove_emulation_prevention(&[5, 6, 7]), [5, 6, 7]);
    }

    #[test]
    fn fixed_length_fields() {
        let mut r = BitReader::new(&[0b1010_1100, 0b1111_0000]);
        assert_eq!(r.u(4).unwrap(), 0b1010);
        assert_eq!(r.u(1).unwrap(), 1);
        assert_eq!(r.u(3).unwrap(), 0b100);
        assert_eq!(r.u(8).unwrap(), 0b1111_0000);
        assert!(r.u(1).is_err(), "nothing left");
    }

    /// Builds bytes from a string of `0`/`1` characters (MSB first),
    /// zero-padded to a whole number of bytes — far less error-prone than
    /// hand-computed binary literals for a multi-field bit layout.
    fn bits_to_bytes(bits: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let mut byte = 0u8;
        let mut n = 0;
        for c in bits.chars() {
            byte = (byte << 1) | if c == '1' { 1 } else { 0 };
            n += 1;
            if n == 8 {
                out.push(byte);
                byte = 0;
                n = 0;
            }
        }
        if n > 0 {
            out.push(byte << (8 - n));
        }
        out
    }

    #[test]
    fn exp_golomb_unsigned() {
        // ue(v) codes for 0,1,2,3,4: "1", "010", "011", "00100", "00101"
        // (the general Exp-Golomb construction in H.265 §9.2).
        let bytes = bits_to_bytes(concat!("1", "010", "011", "00100", "00101"));
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.ue().unwrap(), 0);
        assert_eq!(r.ue().unwrap(), 1);
        assert_eq!(r.ue().unwrap(), 2);
        assert_eq!(r.ue().unwrap(), 3);
        assert_eq!(r.ue().unwrap(), 4);
    }

    #[test]
    fn exp_golomb_signed() {
        // The same ue(v) codes for 0,1,2,3,4 map to se(v) 0,1,-1,2,-2.
        let bytes = bits_to_bytes(concat!("1", "010", "011", "00100", "00101"));
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.se().unwrap(), 0);
        assert_eq!(r.se().unwrap(), 1);
        assert_eq!(r.se().unwrap(), -1);
        assert_eq!(r.se().unwrap(), 2);
        assert_eq!(r.se().unwrap(), -2);
    }
}
