//! Ogg demuxer (RFC 3533) used by this parser.
//!
//! Packets are returned as lists of source extents, never copied, so a packet
//! split across pages becomes several `src` chunks. Every page's CRC and
//! sequence number is checked. Only a single logical stream is supported:
//! multiplexed or chained streams fail with `UNSUPPORTED_FEATURE`.
//!
//! The demuxer is part of this parser on purpose: parsers share no container
//! or codec logic, so a change here can only change this parser's output,
//! which is versioned by this crate.

use vtj::cli::ParseError;
use vtj::source::SourceFile;
use vtj::{Chunk, ErrorCode};

const HEADER_LEN: u64 = 27;

/// A complete packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// `(offset, length)` pieces in the source, in order.
    pub extents: Vec<(u64, u64)>,
    /// Index of the packet in the logical stream, from 0.
    pub index: u64,
    /// Set when this is the last packet completed on its page.
    pub page_end: Option<PageEnd>,
}

/// Information of the page on which a packet is the last one to complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageEnd {
    pub granule: i64,
    /// Page sequence number.
    pub sequence: u32,
    /// The page has the end-of-stream flag.
    pub eos: bool,
}

impl Packet {
    pub fn len(&self) -> u64 {
        self.extents.iter().map(|e| e.1).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The packet as a data chain of `src` chunks for `source`.
    pub fn chain(&self, source: u64) -> Vec<Chunk> {
        self.extents.iter().map(|&(o, l)| Chunk::src(source, o, l)).collect()
    }

    /// Reads the first `n` bytes (fewer if the packet is shorter).
    /// `Vec::with_capacity` is not used directly: `n` ultimately comes from
    /// a packet's own (untrusted) length, which can span arbitrarily many
    /// pages, so an allocation failure is reported as an error instead of
    /// aborting the process.
    pub fn prefix(&self, src: &mut SourceFile, n: usize) -> Result<Vec<u8>, ParseError> {
        let mut out = Vec::new();
        out.try_reserve_exact(n).map_err(|_| {
            ParseError::new(ErrorCode::SourceUnreadable, format!("cannot allocate {n} bytes to read a packet"))
        })?;
        for &(o, l) in &self.extents {
            if out.len() >= n {
                break;
            }
            let take = (n - out.len()).min(l as usize);
            let mut buf = vec![0u8; take];
            src.read_at(o, &mut buf)?;
            out.extend(buf);
        }
        Ok(out)
    }

    /// Reads the whole packet. `usize::try_from`, not `as usize`: on a
    /// 32-bit target a packet longer than `usize::MAX` (already an absurd
    /// size by the time anything gets here) would otherwise silently
    /// truncate instead of failing.
    pub fn read(&self, src: &mut SourceFile) -> Result<Vec<u8>, ParseError> {
        let n = usize::try_from(self.len()).map_err(|_| {
            ParseError::new(
                ErrorCode::SourceUnreadable,
                format!("packet {} is {} bytes, too large to address on this platform", self.index, self.len()),
            )
        })?;
        self.prefix(src, n)
    }
}

/// CRC-32 of Ogg pages: polynomial 0x04c11db7, not reflected, initial 0.
pub fn crc32(data: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut r = (i as u32) << 24;
            for _ in 0..8 {
                r = if r & 0x8000_0000 != 0 { (r << 1) ^ 0x04c1_1db7 } else { r << 1 };
            }
            *e = r;
        }
        t
    });
    data.iter().fold(0u32, |crc, &b| (crc << 8) ^ t[((crc >> 24) as u8 ^ b) as usize])
}

/// Sequential packet reader over one source.
pub struct OggReader {
    pos: u64,
    end: u64,
    serial: Option<u32>,
    next_sequence: u32,
    eos_seen: bool,
    partial: Vec<(u64, u64)>,
    ready: std::collections::VecDeque<Packet>,
    packets: u64,
    /// Granule position of the last page that completed at least one
    /// packet; a later nil end-of-stream page must repeat it.
    last_granule: Option<i64>,
}

impl OggReader {
    pub fn new(src: &SourceFile) -> Self {
        OggReader {
            pos: 0,
            end: src.size(),
            serial: None,
            next_sequence: 0,
            eos_seen: false,
            partial: Vec::new(),
            ready: Default::default(),
            packets: 0,
            last_granule: None,
        }
    }

    /// Next complete packet, or `None` at the end of the stream.
    pub fn next_packet(&mut self, src: &mut SourceFile) -> Result<Option<Packet>, ParseError> {
        while self.ready.is_empty() {
            if self.pos >= self.end {
                if !self.partial.is_empty() {
                    return Err(ParseError::truncated(format!(
                        "packet {} is incomplete at the end of the file",
                        self.packets
                    )));
                }
                // A logical stream that began (a BOS page was seen) must end
                // in a page with the end-of-stream flag (RFC 3533 §4). Losing
                // that page entirely, with the cut landing exactly on a page
                // boundary, would otherwise leave no partial packet behind
                // and look like a clean end of stream.
                if self.serial.is_some() && !self.eos_seen {
                    return Err(ParseError::truncated(format!(
                        "stream ends at byte {} without an end-of-stream page",
                        self.end
                    )));
                }
                return Ok(None);
            }
            self.read_page(src)?;
        }
        Ok(self.ready.pop_front())
    }

    fn read_page(&mut self, src: &mut SourceFile) -> Result<(), ParseError> {
        let start = self.pos;
        let avail = self.end - start;
        if avail < HEADER_LEN {
            return Err(ParseError::truncated(format!("page header cut at byte {}", self.end)));
        }
        let mut h = [0u8; HEADER_LEN as usize];
        src.read_at(start, &mut h)?;
        if &h[..4] != b"OggS" {
            return Err(ParseError::invalid(format!("no Ogg page at byte {start}")));
        }
        if h[4] != 0 {
            return Err(ParseError::new(
                vtj::ErrorCode::UnsupportedCodecVariant,
                format!("Ogg version {} at byte {start}", h[4]),
            ));
        }
        let flags = h[5];
        let (continued, bos, eos) = (flags & 1 != 0, flags & 2 != 0, flags & 4 != 0);
        let granule = i64::from_le_bytes(h[6..14].try_into().expect("8 bytes"));
        let serial = u32::from_le_bytes(h[14..18].try_into().expect("4 bytes"));
        let sequence = u32::from_le_bytes(h[18..22].try_into().expect("4 bytes"));
        let stored_crc = u32::from_le_bytes(h[22..26].try_into().expect("4 bytes"));
        let nsegs = h[26] as u64;
        if avail < HEADER_LEN + nsegs {
            return Err(ParseError::truncated(format!("page at byte {start} cut at byte {}", self.end)));
        }
        let mut lacing = vec![0u8; nsegs as usize];
        src.read_at(start + HEADER_LEN, &mut lacing)?;
        let body_len: u64 = lacing.iter().map(|&l| l as u64).sum();
        let page_len = HEADER_LEN + nsegs + body_len;
        if avail < page_len {
            return Err(ParseError::truncated(format!("page {sequence} cut at byte {}", self.end)));
        }
        // `h` and `lacing` are already in hand; only the body needs a fresh read.
        let mut page = Vec::with_capacity(page_len as usize);
        page.extend_from_slice(&h);
        page.extend_from_slice(&lacing);
        page.resize(page_len as usize, 0);
        src.read_at(start + HEADER_LEN + nsegs, &mut page[(HEADER_LEN + nsegs) as usize..])?;
        page[22..26].fill(0);
        if crc32(&page) != stored_crc {
            return Err(ParseError::invalid(format!("CRC mismatch in page {sequence} at byte {start}")));
        }

        match self.serial {
            None => {
                if !bos {
                    return Err(ParseError::invalid("the first page is not a beginning-of-stream page"));
                }
                self.serial = Some(serial);
                self.next_sequence = sequence;
            }
            Some(s) if s != serial || bos || self.eos_seen => {
                return Err(ParseError::unsupported(format!(
                    "page at byte {start} belongs to another logical stream; multiplexed and chained Ogg streams are not supported"
                )));
            }
            Some(_) => {}
        }
        if sequence != self.next_sequence {
            return Err(ParseError::invalid(format!(
                "page sequence jumps from {} to {sequence} at byte {start}",
                self.next_sequence
            )));
        }
        self.next_sequence = sequence.wrapping_add(1);
        if continued == self.partial.is_empty() {
            return Err(ParseError::invalid(format!(
                "continuation flag of page {sequence} does not match the previous page"
            )));
        }
        self.eos_seen = eos;

        let mut offset = start + HEADER_LEN + nsegs;
        let mut run_start = offset;
        let mut completed: Vec<Packet> = Vec::new();
        for &l in &lacing {
            offset += l as u64;
            if l < 255 {
                if offset > run_start {
                    self.partial.push((run_start, offset - run_start));
                }
                completed.push(Packet {
                    extents: std::mem::take(&mut self.partial),
                    index: self.packets,
                    page_end: None,
                });
                self.packets += 1;
                run_start = offset;
            }
        }
        if offset > run_start {
            self.partial.push((run_start, offset - run_start));
        }
        if let Some(last) = completed.last_mut() {
            last.page_end = Some(PageEnd { granule, sequence, eos });
            self.last_granule = Some(granule);
        } else if nsegs == 0 && eos {
            // A nil end-of-stream page (RFC 3533 §4): no lacing entries at
            // all, so nothing to complete, but it may still close the
            // stream at the position the last completed packet already
            // established. Distinct from a page whose lacing table (nsegs >
            // 0) only continues a packet into `self.partial`: that is
            // caught below as ending inside a packet, not accepted here.
            if granule != -1 && Some(granule) != self.last_granule {
                return Err(ParseError::invalid(format!(
                    "nil end-of-stream page {sequence} has granule position {granule}, inconsistent with the last known position"
                )));
            }
        } else if granule != -1 {
            return Err(ParseError::invalid(format!(
                "page {sequence} completes no packet but has granule position {granule}"
            )));
        }
        if eos && !self.partial.is_empty() {
            return Err(ParseError::invalid(format!("end-of-stream page {sequence} ends inside a packet")));
        }
        self.ready.extend(completed);
        self.pos = start + page_len;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_known_value() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0x89a1_897f);
    }

    /// Builds a page with a correct CRC.
    pub fn page(flags: u8, granule: i64, serial: u32, seq: u32, lacing: &[u8], body: &[u8]) -> Vec<u8> {
        let mut p = b"OggS".to_vec();
        p.push(0);
        p.push(flags);
        p.extend(granule.to_le_bytes());
        p.extend(serial.to_le_bytes());
        p.extend(seq.to_le_bytes());
        p.extend([0; 4]);
        p.push(lacing.len() as u8);
        p.extend(lacing);
        p.extend(body);
        let c = crc32(&p);
        p[22..26].copy_from_slice(&c.to_le_bytes());
        p
    }

    fn read_all(bytes: &[u8]) -> Result<Vec<Packet>, ParseError> {
        let dir = std::env::temp_dir().join(format!("vmkv-ogg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{:x}.ogg", crc32(bytes)));
        std::fs::write(&p, bytes).unwrap();
        let mut src = SourceFile::open(0, &p).unwrap();
        let mut r = OggReader::new(&src);
        let mut v = Vec::new();
        while let Some(pk) = r.next_packet(&mut src)? {
            v.push(pk);
        }
        Ok(v)
    }

    #[test]
    fn packets_across_pages() {
        let mut f = page(2, 0, 7, 0, &[3], b"abc");
        let a = f.len() as u64;
        f.extend(page(0, -1, 7, 1, &[255], &[1u8; 255]));
        let b = f.len() as u64;
        f.extend(page(1 | 4, 1000, 7, 2, &[10, 0, 2], &[2u8; 12]));
        let v = read_all(&f).unwrap();
        assert_eq!(v.len(), 4);
        assert_eq!(v[0].extents, vec![(28, 3)]);
        assert_eq!(v[0].page_end, Some(PageEnd { granule: 0, sequence: 0, eos: false }));
        assert_eq!(v[1].extents, vec![(a + 28, 255), (b + 30, 10)], "split packet becomes two extents");
        assert_eq!(v[1].len(), 265);
        assert_eq!(v[1].page_end, None);
        assert_eq!(v[2].len(), 0);
        assert_eq!(v[3].extents, vec![(b + 40, 2)]);
        assert_eq!(v[3].page_end, Some(PageEnd { granule: 1000, sequence: 2, eos: true }));
        assert_eq!(v.iter().map(|p| p.index).collect::<Vec<_>>(), [0, 1, 2, 3]);
    }

    #[test]
    fn structural_errors() {
        let good = page(2, 0, 7, 0, &[3], b"abc");
        let mut bad_crc = good.clone();
        bad_crc[30] ^= 1;
        assert!(read_all(&bad_crc).unwrap_err().message.contains("CRC mismatch"));
        let mut gap = good.clone();
        gap.extend(page(0, 5, 7, 2, &[1], b"x"));
        assert!(read_all(&gap).unwrap_err().message.contains("sequence jumps from 1 to 2"));
        let mut other = good.clone();
        other.extend(page(0, 5, 8, 1, &[1], b"x"));
        assert_eq!(read_all(&other).unwrap_err().code, vtj::ErrorCode::UnsupportedFeature);
        let mut chained = page(2 | 4, 0, 7, 0, &[3], b"abc");
        chained.extend(page(2, 0, 7, 1, &[1], b"x"));
        assert_eq!(read_all(&chained).unwrap_err().code, vtj::ErrorCode::UnsupportedFeature);
        let open = page(2, -1, 7, 0, &[255], &[0u8; 255]);
        assert_eq!(read_all(&open).unwrap_err().code, vtj::ErrorCode::TruncatedBitstream);
        assert_eq!(read_all(&good[..20]).unwrap_err().code, vtj::ErrorCode::TruncatedBitstream);
        assert_eq!(
            read_all(&page(0, 0, 7, 0, &[1], b"x")).unwrap_err().message,
            "the first page is not a beginning-of-stream page"
        );
        let mut junk = good.clone();
        junk.extend(b"junk-junk-junk-junk-junk-junk-junk");
        assert!(read_all(&junk).unwrap_err().message.starts_with("no Ogg page at byte"));
        let mut cont = good.clone();
        cont.extend(page(1, 5, 7, 1, &[1], b"x"));
        assert!(read_all(&cont).unwrap_err().message.contains("continuation flag"));
    }

    #[test]
    fn nil_end_of_stream_page() {
        let good = page(2, 0, 7, 0, &[3], b"abc");

        // No lacing entries at all, `eos` set, granule matching the last
        // completed packet's position: a valid RFC 3533 §4 nil EOS page.
        let mut matching = good.clone();
        matching.extend(page(4, 0, 7, 1, &[], &[]));
        let v = read_all(&matching).unwrap();
        assert_eq!(v.len(), 1, "the nil page completes no packet of its own");
        assert_eq!(v[0].page_end, Some(PageEnd { granule: 0, sequence: 0, eos: false }), "unchanged by the nil page");

        // granule -1 on a nil EOS page needs no prior position at all.
        let lone_nil = page(2 | 4, -1, 7, 0, &[], &[]);
        assert_eq!(read_all(&lone_nil).unwrap(), vec![]);

        // A granule that contradicts the last completed packet's position
        // is not a clean end of stream; it must still fail.
        let mut mismatched = good.clone();
        mismatched.extend(page(4, 999, 7, 1, &[], &[]));
        assert!(
            read_all(&mismatched).unwrap_err().message.contains("nil end-of-stream page 1 has granule position 999"),
            "{:?}",
            read_all(&mismatched)
        );

        // nsegs > 0 that only continues a packet is not a nil page, even if
        // it completes nothing: it must still be rejected as ending inside
        // a packet, not accepted as a clean nil EOS.
        let mut mid_packet = page(2, -1, 7, 0, &[255], &[7u8; 255]);
        mid_packet.extend(page(5, -1, 7, 1, &[255], &[7u8; 255]));
        assert!(read_all(&mid_packet).unwrap_err().message.contains("ends inside a packet"));

        // A page after a nil EOS page is still rejected, same as after any
        // other end-of-stream page.
        let mut after_nil = good.clone();
        after_nil.extend(page(4, 0, 7, 1, &[], &[]));
        after_nil.extend(page(0, 1, 7, 2, &[1], b"x"));
        assert_eq!(read_all(&after_nil).unwrap_err().code, vtj::ErrorCode::UnsupportedFeature);

        // Wrong serial on a nil EOS page is rejected like on any other page.
        let mut wrong_serial = good.clone();
        wrong_serial.extend(page(4, 0, 8, 1, &[], &[]));
        assert_eq!(read_all(&wrong_serial).unwrap_err().code, vtj::ErrorCode::UnsupportedFeature);
    }
}
