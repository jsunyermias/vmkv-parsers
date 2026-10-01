//! Multistream Opus packet framing: RFC 6716 §3.2 ("Frame Packing") and
//! Appendix B ("Self-Delimiting Framing").
//!
//! A narrowed, faithful port of libopus's `opus_packet_parse_impl`
//! (xiph/opus, `src/opus.c`): a demuxer splitting a multistream Ogg packet
//! into its `stream_count` per-stream Opus packets only needs how many
//! bytes each one occupies (its frames and any trailing padding) and its
//! total duration; it never needs each frame's own offset, so — unlike
//! libopus, which a decoder also calls — this does not report one.

const MAX_FRAMES: usize = 48;
/// RFC 6716 §3.2: a packet's total duration (frame size × frame count) may
/// not exceed 120 ms at 48 kHz.
const MAX_PACKET_SAMPLES: i64 = 5760;

/// `(bytes consumed, value)` of one frame-length field (1 or 2 bytes,
/// RFC 6716 §3.1); `None` if `data` is too short.
fn parse_size(data: &[u8]) -> Option<(i64, i64)> {
    let &first = data.first()?;
    if first < 252 {
        Some((1, first as i64))
    } else {
        let &second = data.get(1)?;
        Some((2, 4 * second as i64 + first as i64))
    }
}

/// Samples per frame from the TOC byte's config field (RFC 6716 §3.1): SILK
/// (configs 0-11: 10/20/40/60 ms, repeating every 4 configs for NB/MB/WB),
/// Hybrid (12-15: 10/20 ms), CELT (16-31: 2.5/5/10/20 ms), all at 48 kHz.
fn frame_samples(toc: u8) -> i64 {
    let config = toc >> 3;
    match config {
        0..=11 => [480, 960, 1920, 2880][(config % 4) as usize],
        12..=15 => [480, 960][(config % 2) as usize],
        _ => [120, 240, 480, 960][(config % 4) as usize],
    }
}

/// One Opus packet (RFC 6716 §3.2), optionally self-delimited (Appendix B:
/// an explicit length field is added for what would otherwise be the
/// packet's implicit last frame, so its total size is known without
/// reaching its end by other means). Returns `(bytes consumed — frame data
/// and any trailing padding, total samples)`.
pub fn parse_subpacket(data: &[u8], self_delimited: bool) -> Result<(usize, u32), String> {
    let &toc = data.first().ok_or("empty Opus packet")?;
    let frame_size = frame_samples(toc);
    let total = data.len() as i64;
    let mut pos: i64 = 1;
    let mut len = total - pos;
    let mut sizes = [0i64; MAX_FRAMES];
    let mut cbr = false;
    let mut last_size = len;
    let mut pad: i64 = 0;
    let count: usize;

    let byte_at = |p: i64| -> Option<u8> { data.get(usize::try_from(p).ok()?).copied() };
    let slice_from = |p: i64| -> Option<&[u8]> { data.get(usize::try_from(p).ok()?..) };
    let read_size = |p: i64| -> Option<(i64, i64)> { parse_size(slice_from(p)?) };

    match toc & 3 {
        // One frame: normal framing leaves its length implicit (the rest of
        // the packet); self-delimited framing gives it explicitly, below.
        0 => count = 1,
        // Two frames of equal size.
        1 => {
            count = 2;
            cbr = true;
            if !self_delimited {
                if len % 2 != 0 {
                    return Err("code 1 packet has an odd number of bytes".into());
                }
                last_size = len / 2;
                sizes[0] = last_size;
            }
        }
        // Two frames of independent sizes: the first is always explicit.
        2 => {
            count = 2;
            let (used, size0) = read_size(pos).ok_or("code 2 packet: frame 1 length is cut off")?;
            len -= used;
            if size0 < 0 || size0 > len {
                return Err("code 2 packet: frame 1 is longer than the packet".into());
            }
            pos += used;
            sizes[0] = size0;
            last_size = len - size0;
        }
        // An explicit frame count, with optional padding and VBR sizes.
        _ => {
            let ch = byte_at(pos).ok_or("code 3 packet: no frame count byte")?;
            pos += 1;
            len -= 1;
            let n = (ch & 0x3f) as usize;
            if n == 0 || frame_size * n as i64 > MAX_PACKET_SAMPLES {
                return Err(format!("code 3 packet declares {n} frames, out of range"));
            }
            if n > MAX_FRAMES {
                return Err(format!("code 3 packet declares {n} frames, more than {MAX_FRAMES}"));
            }
            count = n;
            if ch & 0x40 != 0 {
                loop {
                    if len <= 0 {
                        return Err("code 3 packet: padding length is cut off".into());
                    }
                    let p = byte_at(pos).ok_or("code 3 packet: padding length is cut off")?;
                    pos += 1;
                    len -= 1;
                    let tmp = if p == 255 { 254 } else { p as i64 };
                    len -= tmp;
                    pad += tmp;
                    if p != 255 {
                        break;
                    }
                }
            }
            if len < 0 {
                return Err("code 3 packet: padding is longer than the packet".into());
            }
            cbr = ch & 0x80 == 0;
            if !cbr {
                last_size = len;
                for size in sizes.iter_mut().take(count - 1) {
                    let (used, s) = read_size(pos).ok_or("code 3 packet: a frame length is cut off")?;
                    len -= used;
                    if s < 0 || s > len {
                        return Err("code 3 packet: a frame is longer than the packet".into());
                    }
                    pos += used;
                    *size = s;
                    last_size -= used + s;
                }
                if last_size < 0 {
                    return Err("code 3 packet: frame lengths exceed the packet".into());
                }
            } else if !self_delimited {
                if len % count as i64 != 0 {
                    return Err("code 3 CBR packet does not divide evenly into its frames".into());
                }
                last_size = len / count as i64;
                for size in sizes.iter_mut().take(count - 1) {
                    *size = last_size;
                }
            }
        }
    }

    // Appendix B: an extra, explicit length for the last frame.
    if self_delimited {
        let (used, size_last) = read_size(pos).ok_or("self-delimited packet: last frame length is cut off")?;
        len -= used;
        if size_last < 0 || size_last > len {
            return Err("self-delimited packet: last frame length exceeds the packet".into());
        }
        pos += used;
        sizes[count - 1] = size_last;
        if cbr {
            if size_last * count as i64 > len {
                return Err("self-delimited CBR packet: frames do not fit the packet".into());
            }
            for size in sizes.iter_mut().take(count - 1) {
                *size = size_last;
            }
        } else if used + size_last > last_size {
            return Err("self-delimited packet: last frame length is inconsistent with the others".into());
        }
    } else {
        if last_size > 1275 {
            return Err("a frame is longer than 1275 bytes".into());
        }
        sizes[count - 1] = last_size;
    }

    let frame_bytes: i64 = sizes[..count].iter().sum();
    let consumed = pos + frame_bytes + pad;
    if consumed < 0 || consumed > total {
        return Err("packet frames and padding exceed the buffer".into());
    }
    let samples = frame_size * count as i64;
    if samples > MAX_PACKET_SAMPLES {
        return Err(format!("packet lasts {samples} samples, more than 120 ms"));
    }
    Ok((consumed as usize, samples as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A code 0 packet (single frame, config 0 = SILK NB 10 ms = 480 samples).
    fn code0(frame: &[u8]) -> Vec<u8> {
        let mut p = vec![0x00];
        p.extend_from_slice(frame);
        p
    }

    fn encode_size(n: i64, out: &mut Vec<u8>) {
        if n < 252 {
            out.push(n as u8);
        } else {
            out.push(252 + (n & 3) as u8);
            out.push(((n - (252 + (n & 3))) / 4) as u8);
        }
    }

    #[test]
    fn size_field_roundtrips() {
        // A frame-length field caps at 1275 (4*255+255): first byte in
        // 252..=255 leaves only a second byte's 0..=255 range to add.
        for n in [0i64, 1, 100, 251, 252, 300, 1000, 1275] {
            let mut buf = Vec::new();
            encode_size(n, &mut buf);
            buf.push(0xAA); // trailing byte, to prove only the field itself is consumed
            let (used, v) = parse_size(&buf).unwrap();
            assert_eq!(v, n, "n={n}");
            assert_eq!(used, buf.len() as i64 - 1, "n={n}");
        }
        assert_eq!(parse_size(&[]), None);
        assert_eq!(parse_size(&[255]), None, "a second byte is needed");
    }

    #[test]
    fn code0_normal_consumes_everything() {
        let p = code0(&[1, 2, 3]);
        let (consumed, samples) = parse_subpacket(&p, false).unwrap();
        assert_eq!(consumed, p.len());
        assert_eq!(samples, 480);
    }

    #[test]
    fn code0_self_delimited_has_an_explicit_length_and_leaves_the_rest() {
        let mut p = vec![0x00, 3]; // TOC, explicit length 3
        p.extend_from_slice(&[9, 9, 9]); // this frame's data
        p.extend_from_slice(&[7, 7]); // bytes belonging to the next sub-packet
        let (consumed, samples) = parse_subpacket(&p, true).unwrap();
        assert_eq!(consumed, 5, "TOC + length byte + 3 frame bytes");
        assert_eq!(samples, 480);

        let short = vec![0x00, 10, 1, 2]; // claims 10 bytes, only 2 follow
        assert!(parse_subpacket(&short, true).is_err());
    }

    #[test]
    fn code1_two_equal_frames() {
        let mut p = vec![0x01]; // TOC, code 1
        p.extend_from_slice(&[1, 2, 3, 4]); // 4 bytes: two 2-byte frames
        let (consumed, _) = parse_subpacket(&p, false).unwrap();
        assert_eq!(consumed, p.len());

        let odd = vec![0x01, 1, 2, 3];
        assert!(parse_subpacket(&odd, false).unwrap_err().contains("odd"));

        // Self-delimited: one explicit size shared by both frames.
        let mut sd = vec![0x01, 2]; // TOC, shared length 2
        sd.extend_from_slice(&[1, 2, 3, 4]); // both frames, 2 bytes each
        sd.extend_from_slice(&[9]); // next sub-packet
        let (consumed, _) = parse_subpacket(&sd, true).unwrap();
        assert_eq!(consumed, 6, "TOC + length byte + 2*2 frame bytes");
    }

    #[test]
    fn code2_two_independent_frames() {
        let mut p = vec![0x02, 2]; // TOC, frame 1 length 2
        p.extend_from_slice(&[1, 2]); // frame 1
        p.extend_from_slice(&[3, 4, 5]); // frame 2: whatever remains
        let (consumed, _) = parse_subpacket(&p, false).unwrap();
        assert_eq!(consumed, p.len());

        let mut too_long = vec![0x02, 200];
        too_long.extend_from_slice(&[0u8; 5]);
        assert!(parse_subpacket(&too_long, false).is_err());

        // Self-delimited: two explicit lengths, both frames' data follow them.
        let mut sd = vec![0x02, 2, 3]; // TOC, len(frame1)=2, len(frame2)=3
        sd.extend_from_slice(&[1, 2]); // frame 1
        sd.extend_from_slice(&[3, 4, 5]); // frame 2
        sd.extend_from_slice(&[9]); // next sub-packet
        let (consumed, _) = parse_subpacket(&sd, true).unwrap();
        assert_eq!(consumed, 8, "TOC + 2 length bytes + 2 + 3 frame bytes");
    }

    #[test]
    fn code3_cbr_and_vbr() {
        // CBR, 3 frames of 2 bytes each, no padding.
        let mut cbr = vec![0x03, 3]; // TOC, count byte: 3 frames, no padding, CBR (bit7=0)
        cbr.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        let (consumed, samples) = parse_subpacket(&cbr, false).unwrap();
        assert_eq!(consumed, cbr.len());
        assert_eq!(samples, 480 * 3);

        // VBR, 2 frames: an explicit length for frame 1, frame 2 implicit.
        let mut vbr = vec![0x03, 2 | 0x80]; // 2 frames, VBR
        vbr.push(2); // frame 1 length
        vbr.extend_from_slice(&[1, 2]); // frame 1
        vbr.extend_from_slice(&[3, 4, 5]); // frame 2 (implicit length)
        let (consumed, _) = parse_subpacket(&vbr, false).unwrap();
        assert_eq!(consumed, vbr.len());

        // CBR self-delimited: one explicit shared length for all 3 frames.
        let mut cbr_sd = vec![0x03, 3, 2]; // TOC, count byte, shared length 2
        cbr_sd.extend_from_slice(&[1, 2, 3, 4, 5, 6]); // 3*2 bytes
        cbr_sd.push(9); // next sub-packet
        let (consumed, _) = parse_subpacket(&cbr_sd, true).unwrap();
        assert_eq!(consumed, 9, "TOC + count byte + shared length byte + 6 frame bytes");

        assert!(parse_subpacket(&[0x03, 0], false).unwrap_err().contains("0 frames"));
    }

    #[test]
    fn code3_padding_is_excluded_from_frame_data_but_included_in_consumed() {
        // 1 frame, padding flag set, padding length 5 (one indicator byte < 255).
        let mut p = vec![0x03, 1 | 0x40 | 0x80]; // 1 frame, padding, VBR(no-op for 1 frame)
        p.push(5); // padding length indicator (< 255: this is the total)
        p.extend_from_slice(&[1, 2, 3]); // the single (implicit-length) frame
        p.extend_from_slice(&[0u8; 5]); // the padding bytes themselves
        let (consumed, samples) = parse_subpacket(&p, false).unwrap();
        assert_eq!(consumed, p.len(), "frame + padding, all accounted for");
        assert_eq!(samples, 480);
    }

    #[test]
    fn a_two_stream_group_splits_into_two_equal_duration_packets() {
        // Stream 0 self-delimited (code 0, explicit length), stream 1 normal.
        let mut group = vec![0x00, 3];
        group.extend_from_slice(&[1, 2, 3]);
        group.push(0x00); // stream 1's TOC
        group.extend_from_slice(&[4, 5]); // stream 1's frame, to the end
        let (c0, s0) = parse_subpacket(&group, true).unwrap();
        assert_eq!(c0, 5);
        let (c1, s1) = parse_subpacket(&group[c0..], false).unwrap();
        assert_eq!(c1, group.len() - c0);
        assert_eq!(s0, s1, "RFC 7845 requires equal duration across streams");
    }
}
