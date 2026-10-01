//! Annex B byte stream scanning (ITU-T H.264 Annex B.1): a sequence of NAL
//! units, each introduced by a start code `0x000001`, optionally preceded
//! by extra `0x00` stuffing bytes that belong to neither NAL.
//!
//! The scanner never loads the whole source into memory: it streams
//! through it in bounded chunks, carrying over only the handful of bytes a
//! start code could straddle across a chunk boundary.

use vtj::cli::ParseError;
use vtj::source::SourceFile;

/// One NAL unit's extent in the source: the start code prefix and any zero
/// byte stuffing around it excluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nal {
    pub offset: u64,
    pub length: u64,
}

const CHUNK: usize = 1 << 20;

/// Every NAL unit's extent, in file order.
pub fn scan(src: &mut SourceFile) -> Result<Vec<Nal>, ParseError> {
    scan_with_chunk_size(src, CHUNK)
}

fn scan_with_chunk_size(src: &mut SourceFile, chunk: usize) -> Result<Vec<Nal>, ParseError> {
    let size = src.size();
    // Absolute offset of each NAL's first byte: right after its start
    // code's `0x000001`, start code and leading zero stuffing excluded.
    let mut starts: Vec<u64> = Vec::new();
    let mut carry: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; chunk.max(3)];
    let mut stream = src.stream_from(0)?;
    loop {
        let n = stream.read_up_to(&mut buf)?;
        if n == 0 {
            break;
        }
        let base = stream.position() - n as u64 - carry.len() as u64;
        let hay_len = carry.len() + n;
        let at = |i: usize| -> u8 {
            if i < carry.len() {
                carry[i]
            } else {
                buf[i - carry.len()]
            }
        };
        let mut i = 0;
        while i + 2 < hay_len {
            if at(i) == 0 && at(i + 1) == 0 && at(i + 2) == 1 {
                starts.push(base + i as u64 + 3);
                i += 3;
            } else {
                i += 1;
            }
        }
        carry = buf[n.saturating_sub(2)..n].to_vec();
    }
    if starts.is_empty() {
        return Err(ParseError::invalid("no Annex B start code found"));
    }

    let mut nals = Vec::with_capacity(starts.len());
    for (idx, &s) in starts.iter().enumerate() {
        // The next start code begins 3 bytes before the next NAL's content
        // (or the file ends here); trim the zero-byte stuffing right
        // before it, which belongs to neither NAL.
        let bound = if idx + 1 < starts.len() { starts[idx + 1] - 3 } else { size };
        let mut e = bound;
        while e > s {
            let mut b = [0u8];
            src.read_at(e - 1, &mut b)?;
            if b[0] != 0 {
                break;
            }
            e -= 1;
        }
        if e <= s {
            return Err(ParseError::invalid(format!("empty NAL unit at byte {s}")));
        }
        nals.push(Nal { offset: s, length: e - s });
    }
    Ok(nals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vmkv-h264-annexb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    fn scan_bytes(bytes: &[u8], chunk: usize) -> Result<Vec<Nal>, ParseError> {
        let p = temp(&format!("{:x}-{chunk}.h264", bytes.len()), bytes);
        let mut src = SourceFile::open(0, &p).unwrap();
        scan_with_chunk_size(&mut src, chunk)
    }

    /// Runs `scan` with every chunk size from 1 up to the whole buffer, so a
    /// start code is forced to straddle a chunk boundary at every possible
    /// position at least once.
    fn scan_all_chunk_sizes(bytes: &[u8]) -> Vec<Nal> {
        let reference = scan_bytes(bytes, bytes.len().max(1)).unwrap();
        for chunk in 1..=bytes.len() {
            let got = scan_bytes(bytes, chunk).unwrap();
            assert_eq!(got, reference, "chunk size {chunk} disagrees with a single whole-buffer read");
        }
        reference
    }

    #[test]
    fn three_nals_three_and_four_byte_start_codes() {
        let bytes = [
            0, 0, 0, 1, 0xAA, 1, 2, 3, // 4-byte start code, NAL "AA 01 02 03"
            0, 0, 1, 0xBB, 4, 5, // 3-byte start code, NAL "BB 04 05"
            0, 0, 1, 0xCC, 6, // last NAL, to EOF
        ];
        let nals = scan_all_chunk_sizes(&bytes);
        assert_eq!(nals, [Nal { offset: 4, length: 4 }, Nal { offset: 11, length: 3 }, Nal { offset: 17, length: 2 }]);
    }

    #[test]
    fn trailing_zero_stuffing_is_excluded_from_both_sides() {
        let bytes = [
            0, 0, 1, 0xAA, 1, 2, 0, 0, // NAL "AA 01 02", then stuffing
            0, 0, 1, 0xBB, 3, // next start code, then its NAL
        ];
        let nals = scan_all_chunk_sizes(&bytes);
        assert_eq!(nals, [Nal { offset: 3, length: 3 }, Nal { offset: 11, length: 2 }]);
    }

    #[test]
    fn a_nal_entirely_of_zero_bytes_is_rejected() {
        let bytes = [0, 0, 1, 0, 0, 0, 1, 0xAA, 1];
        assert!(scan_bytes(&bytes, bytes.len()).unwrap_err().message.contains("empty NAL"));
    }

    #[test]
    fn no_start_code_at_all_is_rejected() {
        let bytes = [1, 2, 3, 4, 5];
        assert!(scan_bytes(&bytes, bytes.len()).unwrap_err().message.contains("no Annex B start code"));
    }

    /// The real libx264 fixture used throughout this parser's tests: its
    /// first four NAL offsets/lengths were independently found with a
    /// one-off byte scan when the fixture was generated, cross-checked
    /// against `ffprobe`'s reported stream info (not reproduced here).
    #[test]
    fn real_encoder_fixture_first_four_nals() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/media/h264_sample.h264");
        let mut src = SourceFile::open(0, &path).unwrap();
        let nals = scan(&mut src).unwrap();
        assert_eq!(nals.len(), 23, "SPS, PPS, SEI, 20 slices (1 IDR + 19 inter)");
        assert_eq!(nals[0], Nal { offset: 4, length: 23 }, "SPS");
        assert_eq!(nals[1], Nal { offset: 31, length: 4 }, "PPS");
        assert_eq!(nals[2], Nal { offset: 38, length: 678 }, "SEI");
        assert_eq!(nals[3], Nal { offset: 719, length: 4511 }, "IDR slice");
    }

    #[test]
    fn realistic_mix_of_lengths_across_many_chunk_boundaries() {
        // A longer, more realistic mix: short and long NALs, both start
        // code widths, exercised at every chunk size as above.
        let mut bytes = Vec::new();
        bytes.extend([0, 0, 0, 1, 0x67]);
        bytes.extend(vec![0xABu8; 20]);
        bytes.extend([0, 0, 1, 0x68]);
        bytes.extend(vec![0xCDu8; 3]);
        bytes.extend([0, 0, 0, 1, 0x65]);
        bytes.extend(vec![0xEFu8; 50]);
        let nals = scan_all_chunk_sizes(&bytes);
        assert_eq!(nals.len(), 3);
        assert_eq!(nals[0].length, 21);
        assert_eq!(nals[1].length, 4);
        assert_eq!(nals[2].length, 51);
    }
}
