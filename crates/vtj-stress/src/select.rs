//! Which mutations to generate for a file.

use crate::mutation::{parse_num, Fill, Mutation, Rng, KINDS};

/// A file offset relative to its size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Offset {
    /// `N`: from the start.
    Start(u64),
    /// `-N`: from the end (`-0` is the end).
    End(u64),
    /// `P%`: a fraction of the size.
    Percent(f64),
}

impl Offset {
    pub fn parse(s: &str) -> Result<Offset, String> {
        if let Some(p) = s.strip_suffix('%') {
            let v: f64 = p.parse().map_err(|_| format!("invalid percentage \"{s}\""))?;
            if !(0.0..=100.0).contains(&v) {
                return Err(format!("percentage \"{s}\" outside 0..100"));
            }
            return Ok(Offset::Percent(v));
        }
        match s.strip_prefix('-') {
            Some(n) => Ok(Offset::End(parse_num(n)?)),
            None => Ok(Offset::Start(parse_num(s)?)),
        }
    }

    pub fn resolve(self, size: u64) -> u64 {
        match self {
            Offset::Start(n) => n.min(size),
            Offset::End(n) => size.saturating_sub(n),
            Offset::Percent(p) => ((size as f64) * p / 100.0).floor() as u64,
        }
    }
}

/// How positions inside the range are chosen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Positions {
    /// Every `N`th offset from the start of the range.
    Step(u64),
    /// `N` offsets spread evenly over the range.
    Even(u64),
    /// `N` distinct pseudo-random offsets from the seed.
    Random(u64),
}

#[derive(Debug, Clone)]
pub struct Selection {
    pub kinds: Vec<String>,
    pub from: Offset,
    pub to: Offset,
    pub positions: Positions,
    pub len: u64,
    pub masks: Vec<u8>,
    pub value: u8,
    pub insert_random: bool,
    pub random_count: u64,
    pub random_edits: u32,
    pub seed: u64,
    pub max: Option<usize>,
}

impl Default for Selection {
    fn default() -> Self {
        Selection {
            kinds: vec!["truncate".into()],
            from: Offset::Start(0),
            to: Offset::End(0),
            positions: Positions::Step(1),
            len: 1,
            masks: vec![0xff],
            value: 0,
            insert_random: false,
            random_count: 100,
            random_edits: 3,
            seed: 0,
            max: None,
        }
    }
}

impl Selection {
    pub fn validate(&self) -> Result<(), String> {
        if self.kinds.is_empty() {
            return Err("no mutation kinds selected".into());
        }
        for k in &self.kinds {
            if !KINDS.contains(&k.as_str()) {
                return Err(format!("unknown mutation kind \"{k}\" (known: {})", KINDS.join(", ")));
            }
        }
        match self.positions {
            Positions::Step(0) => return Err("--step must be > 0".into()),
            Positions::Even(0) | Positions::Random(0) => return Err("the number of positions must be > 0".into()),
            _ => {}
        }
        if self.len == 0 {
            return Err("--len must be > 0".into());
        }
        if self.masks.is_empty() || self.masks.contains(&0) {
            return Err("masks must be non-zero".into());
        }
        if self.random_edits == 0 {
            return Err("--random-edits must be > 0".into());
        }
        Ok(())
    }

    /// Offsets in `[from, to)` for a file of `size` bytes.
    pub fn offsets(&self, size: u64) -> Vec<u64> {
        let (a, b) = (self.from.resolve(size), self.to.resolve(size));
        if a >= b {
            return Vec::new();
        }
        let span = b - a;
        let mut v: Vec<u64> = match self.positions {
            Positions::Step(s) => (a..b).step_by(s as usize).collect(),
            Positions::Even(n) => (0..n.min(span)).map(|i| a + i * span / n.min(span)).collect(),
            Positions::Random(n) => {
                let mut r = Rng::new(self.seed);
                let mut set = std::collections::BTreeSet::new();
                let want = n.min(span);
                while (set.len() as u64) < want {
                    set.insert(a + r.below(span));
                }
                set.into_iter().collect()
            }
        };
        v.dedup();
        v
    }

    /// Every mutation selected for a file of `size` bytes, grouped by kind.
    pub fn generate(&self, size: u64) -> Vec<Mutation> {
        let offsets = self.offsets(size);
        let mut out = Vec::new();
        for kind in &self.kinds {
            if kind == "random" {
                for i in 0..self.random_count {
                    out.push(Mutation::Random { seed: self.seed.wrapping_add(i), edits: self.random_edits });
                }
                continue;
            }
            for &at in &offsets {
                let len = self.len;
                match kind.as_str() {
                    "truncate" => out.push(Mutation::Truncate { at }),
                    "flip" => out.extend(self.masks.iter().map(|&mask| Mutation::Flip { at, mask })),
                    "set" => out.push(Mutation::Set { at, value: self.value }),
                    "zero" => out.push(Mutation::Zero { at, len }),
                    "delete" => out.push(Mutation::Delete { at, len }),
                    "insert" => {
                        let fill = if self.insert_random { Fill::Random } else { Fill::Byte(self.value) };
                        out.push(Mutation::Insert { at, len, fill })
                    }
                    "dup" => out.push(Mutation::Dup { at, len }),
                    _ => unreachable!("validated"),
                }
            }
        }
        if let Some(max) = self.max.filter(|m| out.len() > *m) {
            let n = out.len();
            out = (0..max).map(|i| out[i * n / max]).collect();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_and_ranges() {
        assert_eq!(Offset::parse("-128").unwrap().resolve(1000), 872);
        assert_eq!(Offset::parse("50%").unwrap().resolve(1001), 500);
        assert_eq!(Offset::parse("0x10").unwrap().resolve(5), 5, "clamped");
        assert!(Offset::parse("150%").is_err());
        let s = Selection {
            from: Offset::Start(10),
            to: Offset::End(10),
            positions: Positions::Step(20),
            ..Default::default()
        };
        assert_eq!(s.offsets(100), [10, 30, 50, 70]);
        let s = Selection { positions: Positions::Even(4), ..Default::default() };
        assert_eq!(s.offsets(100), [0, 25, 50, 75]);
        let s = Selection { positions: Positions::Random(5), seed: 3, ..Default::default() };
        let o = s.offsets(1000);
        assert_eq!(o.len(), 5);
        assert_eq!(o, s.offsets(1000), "deterministic");
        assert!(Selection { from: Offset::Start(50), to: Offset::Start(10), ..Default::default() }
            .offsets(100)
            .is_empty());
    }

    #[test]
    fn generation() {
        let s = Selection {
            kinds: vec!["flip".into(), "delete".into(), "random".into()],
            positions: Positions::Even(2),
            masks: vec![0x01, 0x80],
            len: 3,
            random_count: 2,
            seed: 7,
            ..Default::default()
        };
        let v: Vec<String> = s.generate(10).iter().map(ToString::to_string).collect();
        assert_eq!(
            v,
            [
                "flip@0:0x01",
                "flip@0:0x80",
                "flip@5:0x01",
                "flip@5:0x80",
                "delete@0+3",
                "delete@5+3",
                "random@7x3",
                "random@8x3"
            ]
        );
        let capped = Selection { max: Some(3), ..s.clone() }.generate(10);
        assert_eq!(capped.len(), 3);
        assert!(Selection { kinds: vec!["bogus".into()], ..Default::default() }.validate().is_err());
        assert!(Selection { masks: vec![0], ..Default::default() }.validate().is_err());
    }
}
