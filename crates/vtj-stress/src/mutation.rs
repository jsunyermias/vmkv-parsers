//! Mutations of an input file and their textual form.
//!
//! Every mutation has a canonical spec string with absolute offsets, used to
//! select it exactly (`--variant`) and to report it:
//!
//! | Spec | Effect |
//! | --- | --- |
//! | `truncate@N` | keep bytes `[0, N)` |
//! | `flip@N:M` | XOR byte `N` with mask `M` |
//! | `set@N:V` | set byte `N` to `V` |
//! | `zero@N+L` | set `L` bytes from `N` to zero |
//! | `delete@N+L` | remove `L` bytes from `N` |
//! | `insert@N+L:V` | insert `L` bytes of value `V` at `N` |
//! | `insert@N+L:rand` | insert `L` pseudo-random bytes at `N` |
//! | `dup@N+L` | insert a copy of bytes `[N, N+L)` at `N+L` |
//! | `random@S` / `random@SxK` | `K` (default 3) random edits from seed `S` |
//!
//! Numbers accept decimal or `0x` hexadecimal. Offsets past the end of the
//! file are clamped, so every spec applies to every file.

use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fill {
    Byte(u8),
    Random,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mutation {
    Truncate { at: u64 },
    Flip { at: u64, mask: u8 },
    Set { at: u64, value: u8 },
    Zero { at: u64, len: u64 },
    Delete { at: u64, len: u64 },
    Insert { at: u64, len: u64, fill: Fill },
    Dup { at: u64, len: u64 },
    Random { seed: u64, edits: u32 },
}

/// The kinds of mutation, as named on the command line.
pub const KINDS: [&str; 8] = ["truncate", "flip", "set", "zero", "delete", "insert", "dup", "random"];

/// SplitMix64: small, deterministic and good enough to pick positions.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, n)`; `n` must be > 0.
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }
}

impl Mutation {
    pub fn kind(&self) -> &'static str {
        match self {
            Mutation::Truncate { .. } => "truncate",
            Mutation::Flip { .. } => "flip",
            Mutation::Set { .. } => "set",
            Mutation::Zero { .. } => "zero",
            Mutation::Delete { .. } => "delete",
            Mutation::Insert { .. } => "insert",
            Mutation::Dup { .. } => "dup",
            Mutation::Random { .. } => "random",
        }
    }

    /// Applies the mutation to `data`.
    pub fn apply(&self, data: &[u8]) -> Vec<u8> {
        let n = data.len();
        let cl = |x: u64| (x.min(n as u64)) as usize;
        let mut v = data.to_vec();
        match *self {
            Mutation::Truncate { at } => v.truncate(cl(at)),
            Mutation::Flip { at, mask } => {
                if let Some(b) = v.get_mut(at as usize) {
                    *b ^= mask;
                }
            }
            Mutation::Set { at, value } => {
                if let Some(b) = v.get_mut(at as usize) {
                    *b = value;
                }
            }
            Mutation::Zero { at, len } => {
                let (a, b) = (cl(at), cl(at.saturating_add(len)));
                v[a..b].fill(0);
            }
            Mutation::Delete { at, len } => {
                let (a, b) = (cl(at), cl(at.saturating_add(len)));
                v.drain(a..b);
            }
            Mutation::Insert { at, len, fill } => {
                let bytes: Vec<u8> = match fill {
                    Fill::Byte(x) => vec![x; len as usize],
                    Fill::Random => {
                        let mut r = Rng::new(at ^ len.rotate_left(32));
                        (0..len).map(|_| r.next_u64() as u8).collect()
                    }
                };
                let a = cl(at);
                v.splice(a..a, bytes);
            }
            Mutation::Dup { at, len } => {
                let (a, b) = (cl(at), cl(at.saturating_add(len)));
                let copy = v[a..b].to_vec();
                v.splice(b..b, copy);
            }
            Mutation::Random { seed, edits } => {
                let mut r = Rng::new(seed);
                for _ in 0..edits {
                    let size = v.len() as u64;
                    if size == 0 {
                        break;
                    }
                    let at = r.below(size);
                    let len = 1 + r.below(64);
                    let m = match r.below(6) {
                        0 => Mutation::Flip { at, mask: 1 << r.below(8) },
                        1 => Mutation::Set { at, value: r.next_u64() as u8 },
                        2 => Mutation::Zero { at, len },
                        3 => Mutation::Delete { at, len },
                        4 => Mutation::Insert { at, len, fill: Fill::Random },
                        _ => Mutation::Dup { at, len },
                    };
                    v = m.apply(&v);
                }
            }
        }
        v
    }
}

impl fmt::Display for Mutation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Mutation::Truncate { at } => write!(f, "truncate@{at}"),
            Mutation::Flip { at, mask } => write!(f, "flip@{at}:0x{mask:02x}"),
            Mutation::Set { at, value } => write!(f, "set@{at}:0x{value:02x}"),
            Mutation::Zero { at, len } => write!(f, "zero@{at}+{len}"),
            Mutation::Delete { at, len } => write!(f, "delete@{at}+{len}"),
            Mutation::Insert { at, len, fill: Fill::Byte(v) } => write!(f, "insert@{at}+{len}:0x{v:02x}"),
            Mutation::Insert { at, len, fill: Fill::Random } => write!(f, "insert@{at}+{len}:rand"),
            Mutation::Dup { at, len } => write!(f, "dup@{at}+{len}"),
            Mutation::Random { seed, edits } => write!(f, "random@{seed}x{edits}"),
        }
    }
}

/// Parses a decimal or `0x` hexadecimal number.
pub fn parse_num(s: &str) -> Result<u64, String> {
    let r = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16),
        None => s.parse(),
    };
    r.map_err(|_| format!("invalid number \"{s}\""))
}

fn parse_byte(s: &str) -> Result<u8, String> {
    let v = parse_num(s)?;
    u8::try_from(v).map_err(|_| format!("byte value {v} is larger than 0xff"))
}

impl FromStr for Mutation {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let (kind, arg) = s.split_once('@').ok_or_else(|| format!("\"{s}\": expected KIND@ARGS"))?;
        let at_len = |a: &str| -> Result<(u64, u64), String> {
            let (at, len) = a.split_once('+').ok_or_else(|| format!("\"{s}\": expected OFFSET+LENGTH"))?;
            Ok((parse_num(at)?, parse_num(len)?))
        };
        let m = match kind {
            "truncate" => Mutation::Truncate { at: parse_num(arg)? },
            "flip" => {
                let (at, mask) = arg.split_once(':').unwrap_or((arg, "0xff"));
                Mutation::Flip { at: parse_num(at)?, mask: parse_byte(mask)? }
            }
            "set" => {
                let (at, v) = arg.split_once(':').ok_or_else(|| format!("\"{s}\": expected set@OFFSET:VALUE"))?;
                Mutation::Set { at: parse_num(at)?, value: parse_byte(v)? }
            }
            "zero" => {
                let (at, len) = at_len(arg)?;
                Mutation::Zero { at, len }
            }
            "delete" => {
                let (at, len) = at_len(arg)?;
                Mutation::Delete { at, len }
            }
            "insert" => {
                let (range, fill) = arg.split_once(':').unwrap_or((arg, "0"));
                let (at, len) = at_len(range)?;
                let fill = if fill == "rand" { Fill::Random } else { Fill::Byte(parse_byte(fill)?) };
                Mutation::Insert { at, len, fill }
            }
            "dup" => {
                let (at, len) = at_len(arg)?;
                Mutation::Dup { at, len }
            }
            "random" => {
                let (seed, edits) = arg.split_once('x').map_or((arg, 3), |(a, b)| (a, b.parse().unwrap_or(u32::MAX)));
                if edits == u32::MAX || edits == 0 {
                    return Err(format!("\"{s}\": expected random@SEED or random@SEEDxEDITS with EDITS > 0"));
                }
                Mutation::Random { seed: parse_num(seed)?, edits }
            }
            _ => return Err(format!("\"{s}\": unknown mutation kind \"{kind}\" (known: {})", KINDS.join(", "))),
        };
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_roundtrip() {
        for s in [
            "truncate@10",
            "flip@3:0x80",
            "set@0:0x00",
            "zero@5+3",
            "delete@2+4",
            "insert@1+2:0xff",
            "insert@1+2:rand",
            "dup@0+3",
            "random@42x5",
        ] {
            assert_eq!(s.parse::<Mutation>().unwrap().to_string(), s);
        }
        assert_eq!("flip@0x10".parse::<Mutation>().unwrap(), Mutation::Flip { at: 16, mask: 0xff });
        assert_eq!("random@7".parse::<Mutation>().unwrap(), Mutation::Random { seed: 7, edits: 3 });
        for bad in ["flip", "nope@1", "zero@1", "set@1", "flip@1:0x100", "random@1x0", "truncate@x"] {
            assert!(bad.parse::<Mutation>().is_err(), "{bad}");
        }
    }

    #[test]
    fn apply_each_kind() {
        let d = [0u8, 1, 2, 3, 4, 5];
        let ap = |s: &str| s.parse::<Mutation>().unwrap().apply(&d);
        assert_eq!(ap("truncate@4"), [0, 1, 2, 3]);
        assert_eq!(ap("truncate@99"), d);
        assert_eq!(ap("flip@1:0x80"), [0, 0x81, 2, 3, 4, 5]);
        assert_eq!(ap("set@5:0xaa"), [0, 1, 2, 3, 4, 0xaa]);
        assert_eq!(ap("zero@1+2"), [0, 0, 0, 3, 4, 5]);
        assert_eq!(ap("delete@1+2"), [0, 3, 4, 5]);
        assert_eq!(ap("delete@4+99"), [0, 1, 2, 3]);
        assert_eq!(ap("insert@2+2:0x09"), [0, 1, 9, 9, 2, 3, 4, 5]);
        assert_eq!(ap("dup@1+2"), [0, 1, 2, 1, 2, 3, 4, 5]);
        assert_eq!(ap("flip@99:0x01"), d, "out of range is a no-op");
        assert_eq!(ap("random@9x4"), ap("random@9x4"), "deterministic");
        assert_eq!(ap("insert@0+8:rand"), ap("insert@0+8:rand"));
        assert_ne!(ap("random@9x4"), d);
    }
}
