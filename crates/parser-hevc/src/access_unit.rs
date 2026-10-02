//! Access unit assembly and picture order count (ITU-T H.265 §7.4.2.4.4,
//! §7.3.6.1 up to `slice_pic_order_cnt_lsb`, and §8.3.1).

use crate::bits::BitReader;
use crate::params::{ue_max, Pps, Sps};

pub const VPS: u8 = 32;
pub const SPS: u8 = 33;
pub const PPS: u8 = 34;
pub const AUD: u8 = 35;
pub const EOS: u8 = 36;
pub const EOB: u8 = 37;
pub const FD: u8 = 38;
pub const PREFIX_SEI: u8 = 39;
pub const SUFFIX_SEI: u8 = 40;

pub fn is_vcl(t: u8) -> bool {
    t < 32
}

/// Random access (IRAP) pictures: BLA, IDR, CRA and their reserved range.
pub fn is_irap(t: u8) -> bool {
    (16..=23).contains(&t)
}

fn is_idr(t: u8) -> bool {
    t == 19 || t == 20
}

fn is_rasl(t: u8) -> bool {
    t == 8 || t == 9
}

fn is_radl(t: u8) -> bool {
    t == 6 || t == 7
}

/// Sub-layer non-reference pictures: the even types up to 14.
fn is_sub_layer_non_reference(t: u8) -> bool {
    t <= 14 && t.is_multiple_of(2)
}

/// One coded picture, in decode order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessUnit {
    pub nals: Vec<(u64, u64)>,
    pub irap: bool,
    /// Starts a new presentation period: an IRAP picture with
    /// NoRaslOutputFlag, after which POC restarts.
    pub new_period: bool,
    pub poc: i64,
}

#[derive(Default)]
pub struct AuBuilder {
    /// `(PicOrderCntMsb, slice_pic_order_cnt_lsb)` of the previous TemporalId
    /// 0 picture that is not RASL, RADL or sub-layer non-reference.
    prev_tid0: (i64, i64),
    /// The next IRAP picture gets NoRaslOutputFlag (start of the stream, or
    /// after an end of sequence).
    next_irap_resets: bool,
    started: bool,
    /// RASL pictures that follow an IRAP with NoRaslOutputFlag cannot be
    /// decoded.
    rasl_undecodable: bool,
    current: Option<(bool, bool, i64)>,
    pending: Vec<(u64, u64)>,
}

impl AuBuilder {
    pub fn new() -> Self {
        AuBuilder { next_irap_resets: true, ..Default::default() }
    }

    /// Feeds a VCL NAL unit. `rbsp` starts after the 2-byte NAL header.
    #[allow(clippy::too_many_arguments)]
    pub fn feed_vcl(
        &mut self,
        nal: (u64, u64),
        nal_type: u8,
        temporal_id: u8,
        rbsp: &[u8],
        lookup: impl FnOnce(u32) -> Option<(Sps, Pps)>,
    ) -> Result<Option<AccessUnit>, String> {
        if (22..=23).contains(&nal_type) || (10..=15).contains(&nal_type) || nal_type >= 24 {
            return Err(format!("reserved VCL NAL unit type {nal_type} is not supported"));
        }
        let mut r = BitReader::new(rbsp);
        let first_in_pic = r.u1()?;
        if !first_in_pic {
            if self.current.is_none() {
                return Err("a slice segment continues a picture that never started".into());
            }
            self.pending.push(nal);
            return Ok(None);
        }
        let completed = self.flush();
        if is_irap(nal_type) {
            r.u1()?; // no_output_of_prior_pics_flag
        }
        let pps_id = ue_max(&mut r, 63, "slice_pic_parameter_set_id")?;
        let (sps, pps) = lookup(pps_id).ok_or_else(|| format!("slice refers to unknown PPS {pps_id}"))?;
        // The first slice segment of a picture has no address and is never
        // dependent.
        for _ in 0..pps.num_extra_slice_header_bits {
            r.u1()?;
        }
        let slice_type = r.ue()?;
        if slice_type > 2 {
            return Err(format!("slice_type {slice_type} is out of range"));
        }
        if pps.output_flag_present && !r.u1()? {
            return Err("pictures that are not output (pic_output_flag = 0) are not supported".into());
        }
        if sps.separate_colour_plane {
            r.u(2)?;
        }
        let lsb = if is_idr(nal_type) { 0 } else { r.u(sps.log2_max_poc_lsb)? as i64 };

        let no_rasl_output = is_irap(nal_type) && (is_idr(nal_type) || nal_type <= 18 || self.next_irap_resets);
        if !self.started && !is_irap(nal_type) {
            return Err("the stream does not start with a random access (IRAP) picture".into());
        }
        self.started = true;
        if is_irap(nal_type) {
            self.next_irap_resets = false;
            self.rasl_undecodable = no_rasl_output;
        }
        if is_rasl(nal_type) && self.rasl_undecodable {
            return Err("RASL pictures of a leading CRA picture cannot be decoded and are not supported".into());
        }
        let max_lsb = 1i64 << sps.log2_max_poc_lsb;
        let msb = if no_rasl_output {
            0
        } else {
            let (prev_msb, prev_lsb) = self.prev_tid0;
            if lsb < prev_lsb && prev_lsb - lsb >= max_lsb / 2 {
                prev_msb + max_lsb
            } else if lsb > prev_lsb && lsb - prev_lsb > max_lsb / 2 {
                prev_msb - max_lsb
            } else {
                prev_msb
            }
        };
        if temporal_id == 0 && !is_rasl(nal_type) && !is_radl(nal_type) && !is_sub_layer_non_reference(nal_type) {
            self.prev_tid0 = (msb, lsb);
        }
        self.current = Some((is_irap(nal_type), no_rasl_output, msb + lsb));
        self.pending.push(nal);
        Ok(completed)
    }

    /// A non-VCL NAL unit that precedes the next picture (VPS, SPS, PPS,
    /// AUD, prefix SEI): it ends the picture in progress. Prefix SEI is
    /// kept for the next access unit; parameter sets and delimiters are not.
    pub fn feed_prefix(&mut self, nal: Option<(u64, u64)>) -> Option<AccessUnit> {
        let completed = self.flush();
        if let Some(n) = nal {
            self.pending.push(n);
        }
        completed
    }

    /// A suffix SEI belongs to the picture in progress.
    pub fn feed_suffix(&mut self, nal: (u64, u64)) -> Result<(), String> {
        if self.current.is_none() {
            return Err("suffix SEI outside a picture".into());
        }
        self.pending.push(nal);
        Ok(())
    }

    /// End of sequence: the next IRAP picture restarts POC.
    pub fn end_of_sequence(&mut self) -> Option<AccessUnit> {
        let completed = self.flush();
        self.next_irap_resets = true;
        completed
    }

    pub fn flush(&mut self) -> Option<AccessUnit> {
        let (irap, new_period, poc) = self.current.take()?;
        Some(AccessUnit { nals: std::mem::take(&mut self.pending), irap, new_period, poc })
    }
}
