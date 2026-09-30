//! Standard base64 (RFC 4648 §4) with mandatory `=` padding.
//!
//! Decoding is strict so that every byte string has exactly one accepted
//! encoding: the length must be a multiple of 4, padding is mandatory, and
//! the unused trailing bits of the last symbol must be zero.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes `data` as padded standard base64.
pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

fn value(c: u8) -> Option<u32> {
    match c {
        b'A'..=b'Z' => Some((c - b'A') as u32),
        b'a'..=b'z' => Some((c - b'a') as u32 + 26),
        b'0'..=b'9' => Some((c - b'0') as u32 + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decodes canonical padded standard base64. Returns `None` for any other input.
pub fn decode(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let quads = bytes.len() / 4;
    for (i, q) in bytes.chunks(4).enumerate() {
        let last = i + 1 == quads;
        let pad = if last { q.iter().rev().take_while(|&&c| c == b'=').count() } else { 0 };
        if pad > 2 {
            return None;
        }
        let mut n: u32 = 0;
        for (j, &c) in q.iter().enumerate() {
            let v = if j >= 4 - pad { 0 } else { value(c)? };
            n = (n << 6) | v;
        }
        match pad {
            0 => out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]),
            1 => {
                if n & 0xff != 0 {
                    return None;
                }
                out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8]);
            }
            _ => {
                if n & 0xffff != 0 {
                    return None;
                }
                out.push((n >> 16) as u8);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_examples() {
        assert_eq!(encode(&[0x12, 0x10]), "EhA=");
        assert_eq!(encode(&4340u32.to_be_bytes()), "AAAQ9A==");
        assert_eq!(encode(&1200u32.to_be_bytes()), "AAAEsA==");
        assert_eq!(encode(&[0x01, 0x00, 0x04]), "AQAE");
        assert_eq!(decode("AU0AKP/hABk=").unwrap(), [0x01, 0x4d, 0x00, 0x28, 0xff, 0xe1, 0x00, 0x19]);
    }

    #[test]
    fn roundtrip_all_lengths() {
        let data: Vec<u8> = (0..=255u8).collect();
        for len in 0..data.len() {
            let e = encode(&data[..len]);
            assert_eq!(decode(&e).unwrap(), &data[..len]);
        }
    }

    #[test]
    fn rejects_non_canonical() {
        assert!(decode("EhB=").is_none(), "non-zero trailing bits");
        assert!(decode("EhA").is_none(), "missing padding");
        assert!(decode("Eh==").is_none(), "non-zero trailing bits with double pad");
        assert!(decode("E===").is_none());
        assert!(decode("Eh=A").is_none(), "padding in the middle");
        assert!(decode("EhA=EhA=").is_none(), "padding before the end");
        assert!(decode("Eh A").is_none());
        assert!(decode("EhA-").is_none(), "url-safe alphabet");
        assert_eq!(decode("").unwrap(), Vec::<u8>::new());
    }
}
