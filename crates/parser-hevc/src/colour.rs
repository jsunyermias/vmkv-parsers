//! Matroska `Colour` from the sequence parameter set, its VUI and the
//! mastering display (137) and content light level (144) SEI messages
//! (decision 75). Colour code points are those of ITU-T H.273, which both
//! the codec and Matroska use. A field the stream does not state, or that
//! would equal its Matroska default, is left out.

use vtj::{Colour, Mastering};

/// VUI `video_signal_type`: full range, and `(colour_primaries,
/// transfer_characteristics, matrix_coeffs)` when described.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signal {
    pub full_range: bool,
    pub description: Option<(u8, u8, u8)>,
}

/// Static HDR metadata found in SEI messages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hdr {
    /// `display_primaries` in G, B, R order (x, y), white point (x, y), in
    /// 0.00002 units; max and min luminance in 0.0001 cd/m².
    pub mastering: Option<([u16; 8], u32, u32)>,
    /// `(max_content_light_level, max_pic_average_light_level)`.
    pub light_level: Option<(u16, u16)>,
}

const MASTERING_DISPLAY: u32 = 137;
const CONTENT_LIGHT_LEVEL: u32 = 144;

/// Reads the messages of an SEI RBSP (NAL header stripped) and records the
/// static HDR ones; a second copy must carry the same values.
pub fn read_sei(rbsp: &[u8], hdr: &mut Hdr) -> Result<(), String> {
    let mut p = 0;
    // Messages until the RBSP trailing bits (a single 0x80 byte).
    while p < rbsp.len() && !(p + 1 == rbsp.len() && rbsp[p] == 0x80) {
        let mut read_value = || -> Result<u32, String> {
            let mut v = 0u32;
            loop {
                let b = *rbsp.get(p).ok_or("SEI message header runs past the NAL unit")?;
                p += 1;
                v = v.checked_add(b as u32).ok_or("SEI payload type or size overflows")?;
                if b != 0xff {
                    return Ok(v);
                }
            }
        };
        let kind = read_value()?;
        let size = read_value()? as usize;
        let payload = rbsp.get(p..p + size).ok_or("SEI payload runs past the NAL unit")?;
        p += size;
        match kind {
            MASTERING_DISPLAY if size >= 24 => {
                let u16_at = |i: usize| u16::from_be_bytes([payload[i], payload[i + 1]]);
                let u32_at = |i: usize| u32::from_be_bytes(payload[i..i + 4].try_into().expect("4 bytes"));
                let mut xy = [0u16; 8];
                for (i, v) in xy.iter_mut().enumerate() {
                    *v = u16_at(2 * i);
                }
                record(&mut hdr.mastering, (xy, u32_at(16), u32_at(20)), "mastering display")?;
            }
            CONTENT_LIGHT_LEVEL if size >= 4 => {
                let v = (u16::from_be_bytes([payload[0], payload[1]]), u16::from_be_bytes([payload[2], payload[3]]));
                record(&mut hdr.light_level, v, "content light level")?;
            }
            MASTERING_DISPLAY | CONTENT_LIGHT_LEVEL => return Err(format!("SEI message {kind} is {size} bytes")),
            _ => {}
        }
    }
    Ok(())
}

fn record<T: PartialEq>(slot: &mut Option<T>, value: T, what: &str) -> Result<(), String> {
    match slot {
        Some(v) if *v != value => Err(format!("the {what} metadata changes mid-stream")),
        Some(_) => Ok(()),
        None => {
            *slot = Some(value);
            Ok(())
        }
    }
}

/// The `Colour` of the track. `chroma_loc` is the top field
/// `chroma_sample_loc_type`.
pub fn colour(
    chroma_format_idc: u32,
    bit_depth_luma_minus8: u32,
    signal: Option<Signal>,
    chroma_loc: Option<u32>,
    hdr: &Hdr,
) -> Colour {
    let mut c = Colour { bits_per_channel: Some(bit_depth_luma_minus8 as u64 + 8), ..Default::default() };
    // Chroma samples removed for each one kept: 4:2:0, 4:2:2, 4:4:4.
    (c.chroma_subsampling_horz, c.chroma_subsampling_vert) = match chroma_format_idc {
        1 => (Some(1), Some(1)),
        2 => (Some(1), Some(0)),
        3 => (Some(0), Some(0)),
        _ => (None, None),
    };
    // H.273 chroma location: left / half horizontally, top / half
    // vertically. Types 4 and 5 (bottom) have no Matroska value.
    if chroma_format_idc == 1 {
        (c.chroma_siting_horz, c.chroma_siting_vert) = match chroma_loc {
            Some(0) => (Some(1), Some(2)),
            Some(1) => (Some(2), Some(2)),
            Some(2) => (Some(1), Some(1)),
            Some(3) => (Some(2), Some(1)),
            _ => (None, None),
        };
    }
    if let Some(s) = signal {
        c.range = Some(if s.full_range { 2 } else { 1 });
        if let Some((primaries, transfer, matrix)) = s.description {
            // 2 is "unspecified", Matroska's default: left out.
            let stated = |v: u8| (v != 2).then_some(v as u64);
            c.primaries = stated(primaries);
            c.transfer_characteristics = stated(transfer);
            c.matrix_coefficients = stated(matrix);
        }
    }
    // 0 means "unknown" in CTA-861.3: no information, left out.
    if let Some((cll, fall)) = hdr.light_level {
        c.max_cll = (cll != 0).then_some(cll as u64);
        c.max_fall = (fall != 0).then_some(fall as u64);
    }
    if let Some((xy, max, min)) = hdr.mastering {
        let chroma = |v: u16| Some(v as f64 / 50000.0);
        c.mastering = Some(Mastering {
            primary_r_chromaticity_x: chroma(xy[4]),
            primary_r_chromaticity_y: chroma(xy[5]),
            primary_g_chromaticity_x: chroma(xy[0]),
            primary_g_chromaticity_y: chroma(xy[1]),
            primary_b_chromaticity_x: chroma(xy[2]),
            primary_b_chromaticity_y: chroma(xy[3]),
            white_point_chromaticity_x: chroma(xy[6]),
            white_point_chromaticity_y: chroma(xy[7]),
            luminance_max: Some(max as f64 / 10000.0),
            luminance_min: Some(min as f64 / 10000.0),
        });
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdr10_sei_messages() {
        // Mastering display: G (8500, 39850), B (6550, 2300), R (35400,
        // 14600), white (15635, 16450), 1000 and 0.005 cd/m²; then content
        // light level 1000/400; then the trailing bits.
        let mut sei = vec![137, 24];
        for v in [8500u16, 39850, 6550, 2300, 35400, 14600, 15635, 16450] {
            sei.extend(v.to_be_bytes());
        }
        sei.extend(10_000_000u32.to_be_bytes());
        sei.extend(50u32.to_be_bytes());
        sei.extend([144, 4, 0x03, 0xe8, 0x01, 0x90, 0x80]);
        let mut hdr = Hdr::default();
        read_sei(&sei, &mut hdr).unwrap();
        let c = colour(1, 2, Some(Signal { full_range: false, description: Some((9, 16, 9)) }), Some(2), &hdr);
        assert_eq!(
            (c.bits_per_channel, c.range, c.primaries, c.transfer_characteristics),
            (Some(10), Some(1), Some(9), Some(16))
        );
        assert_eq!(
            (c.chroma_siting_horz, c.chroma_siting_vert, c.max_cll, c.max_fall),
            (Some(1), Some(1), Some(1000), Some(400))
        );
        let m = c.mastering.unwrap();
        assert_eq!((m.primary_r_chromaticity_x, m.primary_g_chromaticity_y), (Some(0.708), Some(0.797)));
        assert_eq!(
            (m.white_point_chromaticity_x, m.luminance_max, m.luminance_min),
            (Some(0.3127), Some(1000.0), Some(0.005))
        );
        // The same values again are fine; different ones are not.
        read_sei(&sei, &mut hdr).unwrap();
        let mut other = sei.clone();
        other[2] ^= 1;
        assert!(read_sei(&other, &mut hdr).unwrap_err().contains("changes mid-stream"));
        // Unspecified code points are Matroska's defaults: left out.
        let c = colour(1, 0, Some(Signal { full_range: true, description: Some((2, 2, 2)) }), None, &Hdr::default());
        assert_eq!((c.range, c.primaries, c.matrix_coefficients, c.chroma_siting_horz), (Some(2), None, None, None));
    }
}
