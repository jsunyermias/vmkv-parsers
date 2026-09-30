//! Value constraints of the format, shared by the writer and the validator.
//!
//! Each function returns every problem found, as `path: message` strings.

use std::collections::{BTreeMap, BTreeSet};

use crate::json::MAX_SAFE_INT;
use crate::types::{Chunk, Colour, Flag, Header, ParamValue, ProjectionType, Rational, Track, TrackType, Unit, Video};

/// Source id → size, built from the header.
pub type SourceSizes = BTreeMap<u64, u64>;

pub fn source_sizes(h: &Header) -> SourceSizes {
    h.sources.iter().map(|s| (s.id, s.size)).collect()
}

/// Every integer in the format must lie within ±(2^53 − 1). The decoder
/// enforces it on JSON it reads; these checks enforce it on records built in
/// memory, so the writer cannot emit a file the validator would reject.
fn safe(p: &str, v: impl Into<i128>, out: &mut Vec<String>) {
    let v = v.into();
    if v.abs() > MAX_SAFE_INT as i128 {
        out.push(format!("{p}: {v} is outside ±(2^53−1)"));
    }
}

fn safe_opt<T: Into<i128>>(p: &str, v: Option<T>, out: &mut Vec<String>) {
    if let Some(v) = v {
        safe(p, v, out);
    }
}

fn rational(p: &str, r: Rational, out: &mut Vec<String>) {
    if r.num <= 0 || r.den <= 0 {
        out.push(format!("{p}: rational terms must be > 0, found [{},{}]", r.num, r.den));
    }
    safe(&format!("{p}[0]"), r.num, out);
    safe(&format!("{p}[1]"), r.den, out);
}

fn positive(p: &str, v: Option<u64>, out: &mut Vec<String>) {
    if v == Some(0) {
        out.push(format!("{p}: must be > 0"));
    }
}

fn non_negative(p: &str, v: Option<i64>, out: &mut Vec<String>) {
    if let Some(v) = v.filter(|v| *v < 0) {
        out.push(format!("{p}: must be ≥ 0, found {v}"));
    }
}

/// Checks a data chain against the declared sources.
pub fn chain(p: &str, c: &[Chunk], sources: &SourceSizes, out: &mut Vec<String>) {
    for (i, ch) in c.iter().enumerate() {
        if let Chunk::Src { source, offset, length } = *ch {
            safe(&format!("{p}[{i}][1]"), source, out);
            safe(&format!("{p}[{i}][2]"), offset, out);
            safe(&format!("{p}[{i}][3]"), length, out);
            match sources.get(&source) {
                None => out.push(format!("{p}[{i}]: unknown source id {source}")),
                Some(&size) => {
                    if offset.checked_add(length).is_none_or(|end| end > size) {
                        out.push(format!(
                            "{p}[{i}]: offset {offset} + length {length} exceeds size {size} of source {source}"
                        ));
                    }
                }
            }
        }
    }
}

pub fn header(h: &Header) -> Vec<String> {
    let mut out = Vec::new();
    if h.parser.name.is_empty() {
        out.push("parser.name: must not be empty".into());
    }
    if h.parser.version.is_empty() {
        out.push("parser.version: must not be empty".into());
    }
    if h.sources.is_empty() {
        out.push("sources: at least one source is required".into());
    }
    for (i, s) in h.sources.iter().enumerate() {
        safe(&format!("sources[{i}].id"), s.id, &mut out);
        safe(&format!("sources[{i}].size"), s.size, &mut out);
        if i > 0 && s.id <= h.sources[i - 1].id {
            let what = if s.id == h.sources[i - 1].id { "duplicate id" } else { "ids must be in increasing order" };
            out.push(format!("sources[{i}].id: {what} ({})", s.id));
        }
        if let Some(sha) = &s.sha256 {
            if sha.len() != 64 || !sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                out.push(format!("sources[{i}].sha256: must be 64 lowercase hexadecimal digits"));
            }
        }
    }
    for (k, v) in &h.params {
        if k.is_empty() {
            out.push("params: empty parameter name".into());
        }
        match v {
            ParamValue::Rational(r) => rational(&format!("params.{k}"), *r, &mut out),
            ParamValue::Int(n) => safe(&format!("params.{k}"), *n, &mut out),
            ParamValue::String(_) => {}
        }
    }
    out
}

pub fn unit(u: &Unit, sources: &SourceSizes) -> Vec<String> {
    let mut out = Vec::new();
    safe("pts_ns", u.pts_ns, &mut out);
    safe("duration_ns", u.duration_ns, &mut out);
    safe_opt("discard_padding_ns", u.discard_padding_ns, &mut out);
    for (i, b) in u.block_additions.iter().enumerate() {
        safe(&format!("block_additions[{i}].id"), b.id, &mut out);
    }
    if u.duration_ns < -1 {
        out.push(format!("duration_ns: must be ≥ -1, found {}", u.duration_ns));
    }
    if u.flags.contains(Flag::DurationRequired) && u.duration_ns < 0 {
        out.push("duration_ns: flag duration_required requires duration_ns ≥ 0".into());
    }
    chain("payload", &u.payload, sources, &mut out);
    if let Some(c) = &u.codec_state {
        chain("codec_state", c, sources, &mut out);
    }
    let mut ids = BTreeSet::new();
    for (i, b) in u.block_additions.iter().enumerate() {
        if b.id < 1 {
            out.push(format!("block_additions[{i}].id: must be ≥ 1"));
        }
        if !ids.insert(b.id) {
            out.push(format!("block_additions[{i}].id: duplicate id {} in this unit", b.id));
        }
        chain(&format!("block_additions[{i}].data"), &b.data, sources, &mut out);
    }
    out
}

/// `used_addition_ids` are the `block_additions` ids ≥ 2 used by any unit;
/// each needs a mapping with that `id_value`.
pub fn track(t: &Track, sources: &SourceSizes, used_addition_ids: &BTreeSet<u64>) -> Vec<String> {
    let mut out = Vec::new();
    track_ranges(t, &mut out);
    if t.codec_id.is_empty() {
        out.push("codec_id: must not be empty".into());
    }
    if let Some(c) = &t.codec_private {
        chain("codec_private", c, sources, &mut out);
    }
    non_negative("codec_delay_ns", t.codec_delay_ns, &mut out);
    non_negative("seek_preroll_ns", t.seek_preroll_ns, &mut out);

    match (t.track_type == TrackType::Video, &t.video) {
        (true, None) => out.push("video: required when track_type is video".into()),
        (false, Some(_)) => out.push("video: only allowed when track_type is video".into()),
        _ => {}
    }
    match (t.track_type == TrackType::Audio, &t.audio) {
        (true, None) => out.push("audio: required when track_type is audio".into()),
        (false, Some(_)) => out.push("audio: only allowed when track_type is audio".into()),
        _ => {}
    }

    if let Some(a) = &t.audio {
        rational("audio.sampling_frequency", a.sampling_frequency, &mut out);
        positive("audio.channels", Some(a.channels), &mut out);
        if let Some(r) = a.output_sampling_frequency {
            rational("audio.output_sampling_frequency", r, &mut out);
        }
        positive("audio.bit_depth", a.bit_depth, &mut out);
    }

    if let Some(v) = &t.video {
        positive("video.pixel_width", Some(v.pixel_width), &mut out);
        positive("video.pixel_height", Some(v.pixel_height), &mut out);
        positive("video.display_width", v.display_width, &mut out);
        positive("video.display_height", v.display_height, &mut out);
        if let Some(r) = v.nominal_frame_rate {
            rational("video.nominal_frame_rate", r, &mut out);
        }
        if let Some(d) = v.default_decoded_field_duration_ns.filter(|d| *d <= 0) {
            out.push(format!("video.default_decoded_field_duration_ns: must be > 0, found {d}"));
        }
        if let (Some(l), Some(r)) = (v.pixel_crop_left, v.pixel_crop_right) {
            if l.saturating_add(r) >= v.pixel_width && v.pixel_width > 0 {
                out.push("video.pixel_crop_left/right: crop leaves no pixels".into());
            }
        }
        if let (Some(tp), Some(b)) = (v.pixel_crop_top, v.pixel_crop_bottom) {
            if tp.saturating_add(b) >= v.pixel_height && v.pixel_height > 0 {
                out.push("video.pixel_crop_top/bottom: crop leaves no pixels".into());
            }
        }
        if let Some(p) = &v.projection {
            match (p.kind, &p.private) {
                (ProjectionType::Rectangular, Some(_)) => {
                    out.push("video.projection.private: must be absent with type rectangular".into())
                }
                (k, None) if k != ProjectionType::Rectangular => {
                    out.push(format!("video.projection.private: required with type {}", k.as_str()))
                }
                (_, Some(c)) => chain("video.projection.private", c, sources, &mut out),
                _ => {}
            }
        }
    }

    let mut mapped = BTreeSet::new();
    for (i, m) in t.block_addition_mappings.iter().enumerate() {
        let p = format!("block_addition_mappings[{i}]");
        if let Some(id) = m.id_value {
            if id < 2 {
                out.push(format!("{p}.id_value: must be ≥ 2, found {id}"));
            }
            if !mapped.insert(id) {
                out.push(format!("{p}.id_value: duplicate id_value {id}"));
            }
        }
        if m.kind == 0 {
            out.push(format!("{p}.type: must not be 0"));
        }
        if let Some(e) = &m.extra_data {
            chain(&format!("{p}.extra_data"), e, sources, &mut out);
        }
    }
    for id in used_addition_ids.iter().filter(|id| **id >= 2) {
        if !mapped.contains(id) {
            out.push(format!("block_addition_mappings: no mapping with id_value {id}, used by block_additions"));
        }
    }
    out
}

fn track_ranges(t: &Track, out: &mut Vec<String>) {
    safe_opt("codec_delay_ns", t.codec_delay_ns, out);
    safe_opt("seek_preroll_ns", t.seek_preroll_ns, out);
    if let Some(a) = &t.audio {
        safe("audio.channels", a.channels, out);
        safe_opt("audio.bit_depth", a.bit_depth, out);
    }
    if let Some(v) = &t.video {
        video_ranges(v, out);
    }
    for (i, m) in t.block_addition_mappings.iter().enumerate() {
        safe_opt(&format!("block_addition_mappings[{i}].id_value"), m.id_value, out);
        safe(&format!("block_addition_mappings[{i}].type"), m.kind, out);
    }
}

fn video_ranges(v: &Video, out: &mut Vec<String>) {
    safe("video.pixel_width", v.pixel_width, out);
    safe("video.pixel_height", v.pixel_height, out);
    for (name, value) in [
        ("pixel_crop_left", v.pixel_crop_left),
        ("pixel_crop_top", v.pixel_crop_top),
        ("pixel_crop_right", v.pixel_crop_right),
        ("pixel_crop_bottom", v.pixel_crop_bottom),
        ("display_width", v.display_width),
        ("display_height", v.display_height),
        ("field_order", v.field_order),
        ("stereo_mode", v.stereo_mode),
        ("alpha_mode", v.alpha_mode),
    ] {
        safe_opt(&format!("video.{name}"), value, out);
    }
    safe_opt("video.default_decoded_field_duration_ns", v.default_decoded_field_duration_ns, out);
    if let Some(c) = &v.colour {
        colour_ranges(c, out);
    }
    if let Some(p) = &v.projection {
        for (name, value) in [("yaw", p.yaw), ("pitch", p.pitch), ("roll", p.roll)] {
            if value.is_some_and(|x| !x.is_finite()) {
                out.push(format!("video.projection.{name}: must be a finite number"));
            }
        }
    }
}

fn colour_ranges(c: &Colour, out: &mut Vec<String>) {
    for (name, value) in [
        ("matrix_coefficients", c.matrix_coefficients),
        ("bits_per_channel", c.bits_per_channel),
        ("chroma_subsampling_horz", c.chroma_subsampling_horz),
        ("chroma_subsampling_vert", c.chroma_subsampling_vert),
        ("cb_subsampling_horz", c.cb_subsampling_horz),
        ("cb_subsampling_vert", c.cb_subsampling_vert),
        ("chroma_siting_horz", c.chroma_siting_horz),
        ("chroma_siting_vert", c.chroma_siting_vert),
        ("range", c.range),
        ("transfer_characteristics", c.transfer_characteristics),
        ("primaries", c.primaries),
        ("max_cll", c.max_cll),
        ("max_fall", c.max_fall),
    ] {
        safe_opt(&format!("video.colour.{name}"), value, out);
    }
    if let Some(m) = &c.mastering {
        for (name, value) in [
            ("primary_r_chromaticity_x", m.primary_r_chromaticity_x),
            ("primary_r_chromaticity_y", m.primary_r_chromaticity_y),
            ("primary_g_chromaticity_x", m.primary_g_chromaticity_x),
            ("primary_g_chromaticity_y", m.primary_g_chromaticity_y),
            ("primary_b_chromaticity_x", m.primary_b_chromaticity_x),
            ("primary_b_chromaticity_y", m.primary_b_chromaticity_y),
            ("white_point_chromaticity_x", m.white_point_chromaticity_x),
            ("white_point_chromaticity_y", m.white_point_chromaticity_y),
            ("luminance_max", m.luminance_max),
            ("luminance_min", m.luminance_min),
        ] {
            if value.is_some_and(|x| !x.is_finite()) {
                out.push(format!("video.colour.mastering.{name}: must be a finite number"));
            }
        }
    }
}

/// What a codec's Matroska mapping requires from the track line.
#[derive(Debug, Clone, Copy, Default)]
pub struct CodecRequirements {
    pub codec_private: bool,
    pub codec_delay: bool,
    pub seek_preroll: bool,
    pub uncompressed_fourcc: bool,
}

/// CodecIDs whose Matroska mapping requires `CodecPrivate`.
const NEEDS_PRIVATE: &[&str] = &[
    "A_AAC",
    "A_ALAC",
    "A_FLAC",
    "A_MS/ACM",
    "A_OPUS",
    "A_QUICKTIME/QDM2",
    "A_QUICKTIME/QDMC",
    "A_REAL/ATRC",
    "A_REAL/COOK",
    "A_REAL/RALF",
    "A_REAL/SIPR",
    "A_VORBIS",
    "S_ASS",
    "S_KATE",
    "S_SSA",
    "S_TEXT/ASS",
    "S_TEXT/SSA",
    "S_VOBSUB",
    "V_AV1",
    "V_MPEG4/ISO/AVC",
    "V_MPEGH/ISO/HEVC",
    "V_MPEGI/ISO/VVC",
    "V_MS/VFW/FOURCC",
    "V_QUICKTIME",
    "V_REAL/RV10",
    "V_REAL/RV20",
    "V_REAL/RV30",
    "V_REAL/RV40",
    "V_THEORA",
];

/// Known CodecIDs whose mapping requires none of the checked fields.
const NEEDS_NOTHING: &[&str] = &[
    "A_AC3",
    "A_DTS",
    "A_DTS/EXPRESS",
    "A_DTS/LOSSLESS",
    "A_EAC3",
    "A_MLP",
    "A_MPEG/L1",
    "A_MPEG/L2",
    "A_MPEG/L3",
    "A_PCM/FLOAT/IEEE",
    "A_PCM/INT/BIG",
    "A_PCM/INT/LIT",
    "A_TRUEHD",
    "A_WAVPACK4",
    "S_ARIBSUB",
    "S_DVBSUB",
    "S_HDMV/PGS",
    "S_HDMV/TEXTST",
    "S_IMAGE/BMP",
    "S_TEXT/USF",
    "S_TEXT/UTF8",
    "S_TEXT/WEBVTT",
    "V_FFV1",
    "V_MPEG1",
    "V_MPEG2",
    "V_MPEG4/ISO/AP",
    "V_MPEG4/ISO/ASP",
    "V_MPEG4/ISO/SP",
    "V_PRORES",
    "V_VP8",
    "V_VP9",
];

/// Requirements for known CodecIDs; `None` for codecs this table does not know.
pub fn codec_requirements(codec_id: &str) -> Option<CodecRequirements> {
    let opus = codec_id == "A_OPUS";
    if NEEDS_PRIVATE.contains(&codec_id) {
        Some(CodecRequirements {
            codec_private: true,
            codec_delay: opus,
            seek_preroll: opus,
            uncompressed_fourcc: false,
        })
    } else if codec_id == "V_UNCOMPRESSED" {
        Some(CodecRequirements { uncompressed_fourcc: true, ..Default::default() })
    } else if NEEDS_NOTHING.contains(&codec_id) {
        Some(CodecRequirements::default())
    } else {
        None
    }
}

/// Checks the codec-dependent fields (`--codec-aware`).
pub fn codec_aware(t: &Track) -> Vec<String> {
    let mut out = Vec::new();
    let prefix_type = match t.codec_id.get(..2) {
        Some("V_") => Some(TrackType::Video),
        Some("A_") => Some(TrackType::Audio),
        Some("S_") => Some(TrackType::Subtitle),
        Some("B_") => Some(TrackType::Buttons),
        _ => None,
    };
    if let Some(expected) = prefix_type {
        if expected != t.track_type {
            out.push(format!(
                "track_type: codec_id {} implies {}, found {}",
                t.codec_id,
                expected.as_str(),
                t.track_type.as_str()
            ));
        }
    }
    let fourcc = t.video.as_ref().is_some_and(|v| v.uncompressed_fourcc.is_some());
    if fourcc && t.codec_id != "V_UNCOMPRESSED" {
        out.push("video.uncompressed_fourcc: only allowed with codec_id V_UNCOMPRESSED".into());
    }
    let Some(req) = codec_requirements(&t.codec_id) else {
        return out;
    };
    if req.codec_private && t.codec_private.as_ref().is_none_or(|c| c.is_empty()) {
        out.push(format!("codec_private: required by the Matroska mapping of {}", t.codec_id));
    }
    if req.codec_delay && t.codec_delay_ns.is_none() {
        out.push(format!("codec_delay_ns: required by the Matroska mapping of {}", t.codec_id));
    }
    if req.seek_preroll && t.seek_preroll_ns.is_none() {
        out.push(format!("seek_preroll_ns: required by the Matroska mapping of {}", t.codec_id));
    }
    if req.uncompressed_fourcc && !fourcc {
        out.push(format!("video.uncompressed_fourcc: required by the Matroska mapping of {}", t.codec_id));
    }
    out
}
