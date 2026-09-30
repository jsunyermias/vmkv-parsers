//! Exact timestamp arithmetic (rules 2 and 3).
//!
//! Instants are exact rationals `p/q` seconds and are rounded exactly once,
//! with the normative formula `floor((2·p·10^9 + q) / (2·q))` evaluated in
//! 128-bit integers with floor division. Rounded values are never summed.

use crate::json::MAX_SAFE_INT;
use crate::types::Rational;

/// Arithmetic that cannot be represented in the format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeError {
    /// A denominator or rate term was ≤ 0.
    InvalidRational,
    /// An intermediate product overflowed 128 bits.
    Overflow,
    /// The result lies outside ±(2^53 − 1) ns.
    OutOfRange,
    /// The end instant precedes the last presentation instant.
    EndBeforeLastUnit,
}

impl std::fmt::Display for TimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TimeError::InvalidRational => "rational term must be > 0",
            TimeError::Overflow => "time arithmetic overflow",
            TimeError::OutOfRange => "time outside ±(2^53-1) ns",
            TimeError::EndBeforeLastUnit => "end instant precedes the last presentation instant",
        })
    }
}

impl std::error::Error for TimeError {}

const NS_PER_S: i128 = 1_000_000_000;

fn in_range(v: i128) -> Result<i64, TimeError> {
    if (-(MAX_SAFE_INT as i128)..=MAX_SAFE_INT as i128).contains(&v) {
        Ok(v as i64)
    } else {
        Err(TimeError::OutOfRange)
    }
}

/// Rounds the exact instant `p/q` seconds to the nearest nanosecond, ties
/// towards +∞: `floor((2·p·10^9 + q) / (2·q))`.
pub fn round_ns(p: i128, q: i128) -> Result<i64, TimeError> {
    if q <= 0 {
        return Err(TimeError::InvalidRational);
    }
    let num = p.checked_mul(2 * NS_PER_S).and_then(|v| v.checked_add(q)).ok_or(TimeError::Overflow)?;
    let den = q.checked_mul(2).ok_or(TimeError::Overflow)?;
    in_range(num.div_euclid(den))
}

/// Converts `ticks` of a clock running at `rate` ticks per second (for
/// example a sampling frequency `[44100,1]` or a frame rate `[24000,1001]`)
/// to rounded nanoseconds: `ticks · rate.den / rate.num` seconds.
pub fn ticks_to_ns(ticks: i128, rate: Rational) -> Result<i64, TimeError> {
    if rate.num <= 0 || rate.den <= 0 {
        return Err(TimeError::InvalidRational);
    }
    let p = ticks.checked_mul(rate.den as i128).ok_or(TimeError::Overflow)?;
    round_ns(p, rate.num as i128)
}

/// Timing for units stored in presentation order with a known length each
/// (typical for audio). The running position is kept exact in ticks, so each
/// `pts_ns` is rounded from the exact instant and each `duration_ns` is the
/// difference between two rounded instants (rule 3).
#[derive(Debug, Clone)]
pub struct Timeline {
    rate: Rational,
    position: i128,
}

impl Timeline {
    /// `start_ticks` may be negative (for example minus the Opus pre-skip).
    pub fn new(rate: Rational, start_ticks: i128) -> Result<Self, TimeError> {
        if rate.num <= 0 || rate.den <= 0 {
            return Err(TimeError::InvalidRational);
        }
        Ok(Timeline { rate, position: start_ticks })
    }

    pub fn rate(&self) -> Rational {
        self.rate
    }

    /// Exact position of the next unit, in ticks.
    pub fn position(&self) -> i128 {
        self.position
    }

    /// Advances by a unit of `length_ticks` and returns its `(pts_ns, duration_ns)`.
    pub fn advance(&mut self, length_ticks: i128) -> Result<(i64, i64), TimeError> {
        let start = ticks_to_ns(self.position, self.rate)?;
        let end_ticks = self.position.checked_add(length_ticks).ok_or(TimeError::Overflow)?;
        let end = ticks_to_ns(end_ticks, self.rate)?;
        self.position = end_ticks;
        Ok((start, end - start))
    }
}

/// Durations for units given in decode (file) order whose presentation order
/// may differ (B-frames). Each duration is the next `pts_ns` in presentation
/// order minus this one; the last unit in presentation order uses `end_ns`
/// (already rounded from the exact end instant) or `-1` when unknown.
///
/// Units with equal `pts_ns` keep their decode order and all but the last get
/// duration 0.
pub fn durations_from_pts(pts_in_decode_order: &[i64], end_ns: Option<i64>) -> Result<Vec<i64>, TimeError> {
    let mut order: Vec<usize> = (0..pts_in_decode_order.len()).collect();
    order.sort_by_key(|&i| pts_in_decode_order[i]);
    let mut out = vec![-1; pts_in_decode_order.len()];
    for w in order.windows(2) {
        out[w[0]] = pts_in_decode_order[w[1]] - pts_in_decode_order[w[0]];
    }
    if let (Some(&last), Some(end)) = (order.last(), end_ns) {
        let d = end - pts_in_decode_order[last];
        if d < 0 {
            return Err(TimeError::EndBeforeLastUnit);
        }
        out[last] = d;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normative_rounding() {
        assert_eq!(round_ns(3 * 1001, 24000), Ok(125_125_000));
        assert_eq!(round_ns(3, 2_000_000_000), Ok(2), "+1.5 ns ties up");
        assert_eq!(round_ns(-3, 2_000_000_000), Ok(-1), "-1.5 ns ties towards +inf");
        assert_eq!(round_ns(-5, 2_000_000_000), Ok(-2), "-2.5 ns ties towards +inf");
        assert_eq!(round_ns(-1, 2_000_000_000), Ok(0), "-0.5 ns ties towards +inf");
        assert_eq!(round_ns(-7, 4_000_000_000), Ok(-2), "-1.75 ns");
        assert_eq!(round_ns(-5, 4_000_000_000), Ok(-1), "-1.25 ns");
        assert_eq!(round_ns(1, 0), Err(TimeError::InvalidRational));
        assert_eq!(round_ns(i128::MAX / 2, 1), Err(TimeError::Overflow));
        assert_eq!(round_ns(104 * 86_400 + 1, 1), Ok(8_985_601_000_000_000));
        assert_eq!(round_ns(105 * 86_400, 1), Err(TimeError::OutOfRange));
    }

    #[test]
    fn spec_example_timestamps() {
        let mp3: Vec<i64> = (0..3).map(|n| ticks_to_ns(1152 * n, Rational::new(44100, 1)).unwrap()).collect();
        assert_eq!(mp3, [0, 26_122_449, 52_244_898]);
        assert_eq!(ticks_to_ns(-312, Rational::new(48000, 1)), Ok(-6_500_000));
        assert_eq!(ticks_to_ns(1, Rational::new(24000, 1001)), Ok(41_708_333));
        assert_eq!(ticks_to_ns(2, Rational::new(24000, 1001)), Ok(83_416_667));
    }

    #[test]
    fn timeline_does_not_accumulate_rounding() {
        let mut t = Timeline::new(Rational::new(24000, 1001), 0).unwrap();
        let frames = 24 * 3600 * 3;
        let mut naive = 0i64;
        for n in 0..frames {
            let (pts, dur) = t.advance(1).unwrap();
            assert_eq!(pts, ticks_to_ns(n, Rational::new(24000, 1001)).unwrap());
            assert!(dur == 41_708_333 || dur == 41_708_334);
            naive += 41_708_333;
        }
        assert_eq!(t.position(), frames);
        let exact = ticks_to_ns(frames, Rational::new(24000, 1001)).unwrap();
        assert_eq!(exact - naive, 86_400, "summing rounded durations drifts by 86.4 us in 3 h");
    }

    #[test]
    fn aac_and_opus_examples() {
        let mut t = Timeline::new(Rational::new(44100, 1), 0).unwrap();
        assert_eq!(t.advance(1024).unwrap(), (0, 23_219_955));
        assert_eq!(t.advance(1024).unwrap(), (23_219_955, 23_219_954));
        let mut o = Timeline::new(Rational::new(48000, 1), -312).unwrap();
        assert_eq!(o.advance(960).unwrap(), (-6_500_000, 20_000_000));
        assert_eq!(o.advance(960).unwrap(), (13_500_000, 20_000_000));
    }

    #[test]
    fn presentation_order_durations() {
        let rate = Rational::new(25, 1);
        let pts: Vec<i64> = [0, 3, 1, 2].iter().map(|&n| ticks_to_ns(n, rate).unwrap()).collect();
        let end = ticks_to_ns(4, rate).unwrap();
        assert_eq!(durations_from_pts(&pts, Some(end)).unwrap(), [40_000_000; 4]);
        assert_eq!(durations_from_pts(&pts, None).unwrap(), [40_000_000, -1, 40_000_000, 40_000_000]);
        assert_eq!(durations_from_pts(&pts, Some(0)), Err(TimeError::EndBeforeLastUnit));
        assert!(durations_from_pts(&[], None).unwrap().is_empty());
    }
}
