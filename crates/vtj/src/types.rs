//! Typed records of the `.vtj` v1 format and their canonical serialization.
//!
//! Every record serializes its fields in the order of the specification's
//! tables, omits absent optional fields and never writes `null`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::base64;
use crate::json::{self, Obj, Value, MAX_SAFE_INT};

/// Value of the header `format` field.
pub const FORMAT: &str = "vmkv-parser-output";
/// Value of the header `version` field.
pub const VERSION: i64 = 1;

/// A rational `[num, den]`; both terms must be > 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rational {
    pub num: i64,
    pub den: i64,
}

impl Rational {
    pub const fn new(num: i64, den: i64) -> Self {
        Rational { num, den }
    }

    fn write(&self, out: &mut String) {
        let _ = write!(out, "[{},{}]", self.num, self.den);
    }
}

/// One piece of a data chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chunk {
    /// `["src", source, offset, length]`
    Src { source: u64, offset: u64, length: u64 },
    /// `["inline", "<base64>"]`
    Inline(Vec<u8>),
}

impl Chunk {
    pub fn src(source: u64, offset: u64, length: u64) -> Self {
        Chunk::Src { source, offset, length }
    }

    pub fn inline(bytes: impl Into<Vec<u8>>) -> Self {
        Chunk::Inline(bytes.into())
    }
}

/// Bytes formed by concatenating chunks in order. May be empty.
pub type DataChain = Vec<Chunk>;

fn write_chain(out: &mut String, chain: &[Chunk]) {
    out.push('[');
    for (i, c) in chain.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        match c {
            Chunk::Src { source, offset, length } => {
                let _ = write!(out, "[\"src\",{source},{offset},{length}]");
            }
            Chunk::Inline(b) => {
                out.push_str("[\"inline\",");
                json::write_str(out, &base64::encode(b));
                out.push(']');
            }
        }
    }
    out.push(']');
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParserInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub id: u64,
    pub size: u64,
    /// Lowercase hexadecimal SHA-256 of the content.
    pub sha256: Option<String>,
    /// Informational only.
    pub path: Option<String>,
}

/// Value of an external parameter recorded in `header.params`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamValue {
    Int(i64),
    Rational(Rational),
    String(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub parser: ParserInfo,
    pub sources: Vec<Source>,
    /// Serialized with keys in ascending byte order; omitted when empty.
    pub params: BTreeMap<String, ParamValue>,
}

/// Unit flags. Serialized in the order of the flag table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Flag {
    RandomAccess,
    Invisible,
    DurationRequired,
}

impl Flag {
    pub const ALL: [Flag; 3] = [Flag::RandomAccess, Flag::Invisible, Flag::DurationRequired];

    pub fn as_str(self) -> &'static str {
        match self {
            Flag::RandomAccess => "random_access",
            Flag::Invisible => "invisible",
            Flag::DurationRequired => "duration_required",
        }
    }

    pub fn parse(s: &str) -> Option<Flag> {
        Flag::ALL.into_iter().find(|f| f.as_str() == s)
    }
}

/// A set of flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags(u8);

impl Flags {
    pub const NONE: Flags = Flags(0);

    pub fn with(mut self, f: Flag) -> Self {
        self.insert(f);
        self
    }

    pub fn insert(&mut self, f: Flag) {
        self.0 |= 1 << f as u8;
    }

    pub fn contains(self, f: Flag) -> bool {
        self.0 & (1 << f as u8) != 0
    }

    pub fn iter(self) -> impl Iterator<Item = Flag> {
        Flag::ALL.into_iter().filter(move |f| self.contains(*f))
    }
}

impl FromIterator<Flag> for Flags {
    fn from_iter<I: IntoIterator<Item = Flag>>(iter: I) -> Self {
        let mut s = Flags::NONE;
        for f in iter {
            s.insert(f);
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockAddition {
    pub id: u64,
    pub data: DataChain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub pts_ns: i64,
    /// `-1` when unknown.
    pub duration_ns: i64,
    pub flags: Flags,
    pub payload: DataChain,
    pub codec_state: Option<DataChain>,
    pub discard_padding_ns: Option<i64>,
    /// Omitted when empty.
    pub block_additions: Vec<BlockAddition>,
}

impl Unit {
    pub fn new(pts_ns: i64, duration_ns: i64, flags: Flags, payload: DataChain) -> Self {
        Unit {
            pts_ns,
            duration_ns,
            flags,
            payload,
            codec_state: None,
            discard_padding_ns: None,
            block_additions: Vec::new(),
        }
    }
}

macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $text),+ }
            }

            pub fn parse(s: &str) -> Option<Self> {
                match s { $($text => Some($name::$variant),)+ _ => None }
            }
        }
    };
}

string_enum!(TrackType {
    Video => "video",
    Audio => "audio",
    Subtitle => "subtitle",
    Complex => "complex",
    Logo => "logo",
    Buttons => "buttons",
    Control => "control",
    Metadata => "metadata",
});

string_enum!(DisplayUnit {
    Pixels => "pixels",
    Centimeters => "centimeters",
    Inches => "inches",
    DisplayAspectRatio => "display_aspect_ratio",
    Unknown => "unknown",
});

string_enum!(Interlace {
    Undetermined => "undetermined",
    Interlaced => "interlaced",
    Progressive => "progressive",
});

string_enum!(ProjectionType {
    Rectangular => "rectangular",
    Equirectangular => "equirectangular",
    Cubemap => "cubemap",
    Mesh => "mesh",
});

string_enum!(
    /// Error codes of the `error` line.
    ErrorCode {
        InvalidBitstream => "INVALID_BITSTREAM",
        TruncatedBitstream => "TRUNCATED_BITSTREAM",
        UnsupportedCodecVariant => "UNSUPPORTED_CODEC_VARIANT",
        UnsupportedProfile => "UNSUPPORTED_PROFILE",
        UnsupportedFeature => "UNSUPPORTED_FEATURE",
        TimingRequired => "TIMING_REQUIRED",
        MissingInitializationData => "MISSING_INITIALIZATION_DATA",
        InconsistentTrackParameters => "INCONSISTENT_TRACK_PARAMETERS",
        UnrepresentableInVmkv => "UNREPRESENTABLE_IN_VMKV",
    }
);

#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub sampling_frequency: Rational,
    pub channels: u64,
    pub output_sampling_frequency: Option<Rational>,
    pub bit_depth: Option<u64>,
}

impl Audio {
    pub fn new(sampling_frequency: Rational, channels: u64) -> Self {
        Audio { sampling_frequency, channels, output_sampling_frequency: None, bit_depth: None }
    }
}

/// SMPTE 2086 mastering metadata, real (not encoded) values.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mastering {
    pub primary_r_chromaticity_x: Option<f64>,
    pub primary_r_chromaticity_y: Option<f64>,
    pub primary_g_chromaticity_x: Option<f64>,
    pub primary_g_chromaticity_y: Option<f64>,
    pub primary_b_chromaticity_x: Option<f64>,
    pub primary_b_chromaticity_y: Option<f64>,
    pub white_point_chromaticity_x: Option<f64>,
    pub white_point_chromaticity_y: Option<f64>,
    pub luminance_max: Option<f64>,
    pub luminance_min: Option<f64>,
}

/// Matroska `Colour` fields, in Matroska element order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Colour {
    pub matrix_coefficients: Option<u64>,
    pub bits_per_channel: Option<u64>,
    pub chroma_subsampling_horz: Option<u64>,
    pub chroma_subsampling_vert: Option<u64>,
    pub cb_subsampling_horz: Option<u64>,
    pub cb_subsampling_vert: Option<u64>,
    pub chroma_siting_horz: Option<u64>,
    pub chroma_siting_vert: Option<u64>,
    pub range: Option<u64>,
    pub transfer_characteristics: Option<u64>,
    pub primaries: Option<u64>,
    pub max_cll: Option<u64>,
    pub max_fall: Option<u64>,
    pub mastering: Option<Mastering>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Projection {
    pub kind: ProjectionType,
    pub private: Option<DataChain>,
    pub yaw: Option<f64>,
    pub pitch: Option<f64>,
    pub roll: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Video {
    pub pixel_width: u64,
    pub pixel_height: u64,
    pub pixel_crop_left: Option<u64>,
    pub pixel_crop_top: Option<u64>,
    pub pixel_crop_right: Option<u64>,
    pub pixel_crop_bottom: Option<u64>,
    pub display_width: Option<u64>,
    pub display_height: Option<u64>,
    pub display_unit: Option<DisplayUnit>,
    pub interlace: Option<Interlace>,
    pub field_order: Option<u64>,
    pub stereo_mode: Option<u64>,
    pub alpha_mode: Option<u64>,
    pub nominal_frame_rate: Option<Rational>,
    pub default_decoded_field_duration_ns: Option<i64>,
    pub uncompressed_fourcc: Option<[u8; 4]>,
    pub colour: Option<Colour>,
    pub projection: Option<Projection>,
}

impl Video {
    pub fn new(pixel_width: u64, pixel_height: u64) -> Self {
        Video { pixel_width, pixel_height, ..Default::default() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockAdditionMapping {
    pub id_value: Option<u64>,
    pub name: Option<String>,
    pub kind: u64,
    pub extra_data: Option<DataChain>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub track_type: TrackType,
    pub codec_id: String,
    pub codec_private: Option<DataChain>,
    pub codec_delay_ns: Option<i64>,
    pub seek_preroll_ns: Option<i64>,
    pub video: Option<Video>,
    pub audio: Option<Audio>,
    /// Omitted when empty.
    pub block_addition_mappings: Vec<BlockAdditionMapping>,
    /// Written only when `true`.
    pub requires_lacing: bool,
}

impl Track {
    pub fn new(track_type: TrackType, codec_id: impl Into<String>) -> Self {
        Track {
            track_type,
            codec_id: codec_id.into(),
            codec_private: None,
            codec_delay_ns: None,
            seek_preroll_ns: None,
            video: None,
            audio: None,
            block_addition_mappings: Vec::new(),
            requires_lacing: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct End {
    pub unit_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorLine {
    pub code: ErrorCode,
    pub message: String,
}

/// Any line of a `.vtj` file.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Line {
    Header(Header),
    Unit(Unit),
    Track(Track),
    End(End),
    Error(ErrorLine),
}

impl Line {
    pub fn type_name(&self) -> &'static str {
        match self {
            Line::Header(_) => "header",
            Line::Unit(_) => "unit",
            Line::Track(_) => "track",
            Line::End(_) => "end",
            Line::Error(_) => "error",
        }
    }

    /// Canonical serialization, without the trailing LF.
    pub fn to_canonical(&self) -> String {
        let mut out = String::new();
        let mut o = Obj::new(&mut out);
        o.str("type", self.type_name());
        match self {
            Line::Header(h) => write_header(&mut o, h),
            Line::Unit(u) => write_unit(&mut o, u),
            Line::Track(t) => write_track(&mut o, t),
            Line::End(e) => o.uint("unit_count", e.unit_count),
            Line::Error(e) => {
                o.str("code", e.code.as_str());
                o.str("message", &e.message);
            }
        }
        o.end();
        out
    }
}

fn write_header(o: &mut Obj, h: &Header) {
    o.str("format", FORMAT);
    o.int("version", VERSION);
    {
        let mut p = Obj::new(o.key("parser"));
        p.str("name", &h.parser.name);
        p.str("version", &h.parser.version);
        p.end();
    }
    let out = o.key("sources");
    out.push('[');
    for (i, s) in h.sources.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let mut so = Obj::new(out);
        so.uint("id", s.id);
        so.uint("size", s.size);
        if let Some(h) = &s.sha256 {
            so.str("sha256", h);
        }
        if let Some(p) = &s.path {
            so.str("path", p);
        }
        so.end();
    }
    out.push(']');
    if !h.params.is_empty() {
        let mut po = Obj::new(o.key("params"));
        for (k, v) in &h.params {
            match v {
                ParamValue::Int(i) => po.int(k, *i),
                ParamValue::Rational(r) => r.write(po.key(k)),
                ParamValue::String(s) => po.str(k, s),
            }
        }
        po.end();
    }
}

fn write_unit(o: &mut Obj, u: &Unit) {
    o.int("pts_ns", u.pts_ns);
    o.int("duration_ns", u.duration_ns);
    {
        let out = o.key("flags");
        out.push('[');
        for (i, f) in u.flags.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            json::write_str(out, f.as_str());
        }
        out.push(']');
    }
    write_chain(o.key("payload"), &u.payload);
    if let Some(c) = &u.codec_state {
        write_chain(o.key("codec_state"), c);
    }
    if let Some(d) = u.discard_padding_ns {
        o.int("discard_padding_ns", d);
    }
    if !u.block_additions.is_empty() {
        let out = o.key("block_additions");
        out.push('[');
        for (i, b) in u.block_additions.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let mut bo = Obj::new(out);
            bo.uint("id", b.id);
            write_chain(bo.key("data"), &b.data);
            bo.end();
        }
        out.push(']');
    }
}

fn opt_uint(o: &mut Obj, k: &str, v: Option<u64>) {
    if let Some(v) = v {
        o.uint(k, v);
    }
}

fn opt_real(o: &mut Obj, k: &str, v: Option<f64>) {
    if let Some(v) = v {
        o.real(k, v);
    }
}

fn write_track(o: &mut Obj, t: &Track) {
    o.str("track_type", t.track_type.as_str());
    o.str("codec_id", &t.codec_id);
    if let Some(c) = &t.codec_private {
        write_chain(o.key("codec_private"), c);
    }
    if let Some(d) = t.codec_delay_ns {
        o.int("codec_delay_ns", d);
    }
    if let Some(d) = t.seek_preroll_ns {
        o.int("seek_preroll_ns", d);
    }
    if let Some(v) = &t.video {
        let mut vo = Obj::new(o.key("video"));
        write_video(&mut vo, v);
        vo.end();
    }
    if let Some(a) = &t.audio {
        let mut ao = Obj::new(o.key("audio"));
        a.sampling_frequency.write(ao.key("sampling_frequency"));
        ao.uint("channels", a.channels);
        if let Some(r) = a.output_sampling_frequency {
            r.write(ao.key("output_sampling_frequency"));
        }
        opt_uint(&mut ao, "bit_depth", a.bit_depth);
        ao.end();
    }
    if !t.block_addition_mappings.is_empty() {
        let out = o.key("block_addition_mappings");
        out.push('[');
        for (i, m) in t.block_addition_mappings.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let mut mo = Obj::new(out);
            opt_uint(&mut mo, "id_value", m.id_value);
            if let Some(n) = &m.name {
                mo.str("name", n);
            }
            mo.uint("type", m.kind);
            if let Some(e) = &m.extra_data {
                write_chain(mo.key("extra_data"), e);
            }
            mo.end();
        }
        out.push(']');
    }
    if t.requires_lacing {
        o.bool("requires_lacing", true);
    }
}

fn write_video(o: &mut Obj, v: &Video) {
    o.uint("pixel_width", v.pixel_width);
    o.uint("pixel_height", v.pixel_height);
    opt_uint(o, "pixel_crop_left", v.pixel_crop_left);
    opt_uint(o, "pixel_crop_top", v.pixel_crop_top);
    opt_uint(o, "pixel_crop_right", v.pixel_crop_right);
    opt_uint(o, "pixel_crop_bottom", v.pixel_crop_bottom);
    opt_uint(o, "display_width", v.display_width);
    opt_uint(o, "display_height", v.display_height);
    if let Some(u) = v.display_unit {
        o.str("display_unit", u.as_str());
    }
    if let Some(i) = v.interlace {
        o.str("interlace", i.as_str());
    }
    opt_uint(o, "field_order", v.field_order);
    opt_uint(o, "stereo_mode", v.stereo_mode);
    opt_uint(o, "alpha_mode", v.alpha_mode);
    if let Some(r) = v.nominal_frame_rate {
        r.write(o.key("nominal_frame_rate"));
    }
    if let Some(d) = v.default_decoded_field_duration_ns {
        o.int("default_decoded_field_duration_ns", d);
    }
    if let Some(f) = &v.uncompressed_fourcc {
        o.str("uncompressed_fourcc", &base64::encode(f));
    }
    if let Some(c) = &v.colour {
        let mut co = Obj::new(o.key("colour"));
        opt_uint(&mut co, "matrix_coefficients", c.matrix_coefficients);
        opt_uint(&mut co, "bits_per_channel", c.bits_per_channel);
        opt_uint(&mut co, "chroma_subsampling_horz", c.chroma_subsampling_horz);
        opt_uint(&mut co, "chroma_subsampling_vert", c.chroma_subsampling_vert);
        opt_uint(&mut co, "cb_subsampling_horz", c.cb_subsampling_horz);
        opt_uint(&mut co, "cb_subsampling_vert", c.cb_subsampling_vert);
        opt_uint(&mut co, "chroma_siting_horz", c.chroma_siting_horz);
        opt_uint(&mut co, "chroma_siting_vert", c.chroma_siting_vert);
        opt_uint(&mut co, "range", c.range);
        opt_uint(&mut co, "transfer_characteristics", c.transfer_characteristics);
        opt_uint(&mut co, "primaries", c.primaries);
        opt_uint(&mut co, "max_cll", c.max_cll);
        opt_uint(&mut co, "max_fall", c.max_fall);
        if let Some(m) = &c.mastering {
            let mut mo = Obj::new(co.key("mastering"));
            opt_real(&mut mo, "primary_r_chromaticity_x", m.primary_r_chromaticity_x);
            opt_real(&mut mo, "primary_r_chromaticity_y", m.primary_r_chromaticity_y);
            opt_real(&mut mo, "primary_g_chromaticity_x", m.primary_g_chromaticity_x);
            opt_real(&mut mo, "primary_g_chromaticity_y", m.primary_g_chromaticity_y);
            opt_real(&mut mo, "primary_b_chromaticity_x", m.primary_b_chromaticity_x);
            opt_real(&mut mo, "primary_b_chromaticity_y", m.primary_b_chromaticity_y);
            opt_real(&mut mo, "white_point_chromaticity_x", m.white_point_chromaticity_x);
            opt_real(&mut mo, "white_point_chromaticity_y", m.white_point_chromaticity_y);
            opt_real(&mut mo, "luminance_max", m.luminance_max);
            opt_real(&mut mo, "luminance_min", m.luminance_min);
            mo.end();
        }
        co.end();
    }
    if let Some(p) = &v.projection {
        let mut po = Obj::new(o.key("projection"));
        po.str("type", p.kind.as_str());
        if let Some(c) = &p.private {
            write_chain(po.key("private"), c);
        }
        opt_real(&mut po, "yaw", p.yaw);
        opt_real(&mut po, "pitch", p.pitch);
        opt_real(&mut po, "roll", p.roll);
        po.end();
    }
}

// ---------------------------------------------------------------------------
// Decoding from parsed JSON. Decoding checks JSON types only; value
// constraints live in `check`.

/// Reads the members of one object, rejecting `null`, unknown and missing fields.
struct Fields<'a> {
    ctx: String,
    members: &'a [(String, Value)],
    used: Vec<bool>,
}

type R<T> = Result<T, String>;

impl<'a> Fields<'a> {
    fn new(ctx: impl Into<String>, v: &'a Value) -> R<Self> {
        let ctx = ctx.into();
        match v {
            Value::Object(m) => Ok(Fields { ctx, used: vec![false; m.len()], members: m }),
            other => Err(format!("{ctx}: expected object, found {}", other.kind())),
        }
    }

    fn path(&self, k: &str) -> String {
        if self.ctx.is_empty() {
            k.to_string()
        } else {
            format!("{}.{k}", self.ctx)
        }
    }

    fn opt(&mut self, k: &str) -> R<Option<&'a Value>> {
        match self.members.iter().position(|(mk, _)| mk == k) {
            None => Ok(None),
            Some(i) => {
                self.used[i] = true;
                match &self.members[i].1 {
                    Value::Null => Err(format!("{}: null is not allowed; omit unknown fields", self.path(k))),
                    v => Ok(Some(v)),
                }
            }
        }
    }

    fn req(&mut self, k: &str) -> R<&'a Value> {
        self.opt(k)?.ok_or_else(|| format!("{}: missing required field", self.path(k)))
    }

    fn finish(self) -> R<()> {
        match self.used.iter().position(|u| !u) {
            None => Ok(()),
            Some(i) => Err(format!("{}: unknown field", self.path(&self.members[i].0))),
        }
    }

    fn opt_with<T>(&mut self, k: &str, f: impl FnOnce(&str, &'a Value) -> R<T>) -> R<Option<T>> {
        let p = self.path(k);
        self.opt(k)?.map(|v| f(&p, v)).transpose()
    }

    fn req_with<T>(&mut self, k: &str, f: impl FnOnce(&str, &'a Value) -> R<T>) -> R<T> {
        let p = self.path(k);
        let v = self.req(k)?;
        f(&p, v)
    }
}

pub(crate) fn as_int(p: &str, v: &Value) -> R<i64> {
    let Value::Number(n) = v else { return Err(format!("{p}: expected integer, found {}", v.kind())) };
    if n.contains(['.', 'e', 'E']) {
        return Err(format!("{p}: expected integer without fraction or exponent, found {n}"));
    }
    let i: i64 = n.parse().map_err(|_| format!("{p}: integer {n} outside ±(2^53−1)"))?;
    if !(-MAX_SAFE_INT..=MAX_SAFE_INT).contains(&i) {
        return Err(format!("{p}: integer {n} outside ±(2^53−1)"));
    }
    Ok(i)
}

fn as_uint(p: &str, v: &Value) -> R<u64> {
    let i = as_int(p, v)?;
    u64::try_from(i).map_err(|_| format!("{p}: must be ≥ 0, found {i}"))
}

fn as_real(p: &str, v: &Value) -> R<f64> {
    let Value::Number(n) = v else { return Err(format!("{p}: expected number, found {}", v.kind())) };
    let f: f64 = n.parse().map_err(|_| format!("{p}: invalid number {n}"))?;
    if !f.is_finite() {
        return Err(format!("{p}: number {n} out of range"));
    }
    Ok(f)
}

fn as_str<'a>(p: &str, v: &'a Value) -> R<&'a str> {
    match v {
        Value::String(s) => Ok(s),
        _ => Err(format!("{p}: expected string, found {}", v.kind())),
    }
}

fn as_array<'a>(p: &str, v: &'a Value) -> R<&'a [Value]> {
    match v {
        Value::Array(a) => Ok(a),
        _ => Err(format!("{p}: expected array, found {}", v.kind())),
    }
}

fn as_rational(p: &str, v: &Value) -> R<Rational> {
    match as_array(p, v)? {
        [n, d] => Ok(Rational { num: as_int(&format!("{p}[0]"), n)?, den: as_int(&format!("{p}[1]"), d)? }),
        _ => Err(format!("{p}: rational must be [numerator, denominator]")),
    }
}

fn as_enum<T>(p: &str, v: &Value, parse: fn(&str) -> Option<T>) -> R<T> {
    let s = as_str(p, v)?;
    parse(s).ok_or_else(|| format!("{p}: unknown value \"{s}\""))
}

fn as_bytes(p: &str, v: &Value) -> R<Vec<u8>> {
    base64::decode(as_str(p, v)?).ok_or_else(|| format!("{p}: invalid or non-canonical base64"))
}

fn as_chain(p: &str, v: &Value) -> R<DataChain> {
    as_array(p, v)?
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let cp = format!("{p}[{i}]");
            match as_array(&cp, c)? {
                [Value::String(k), s, o, l] if k == "src" => Ok(Chunk::Src {
                    source: as_uint(&format!("{cp}[1]"), s)?,
                    offset: as_uint(&format!("{cp}[2]"), o)?,
                    length: as_uint(&format!("{cp}[3]"), l)?,
                }),
                [Value::String(k), b] if k == "inline" => Ok(Chunk::Inline(as_bytes(&format!("{cp}[1]"), b)?)),
                [Value::String(k), ..] if k == "xform" => {
                    Err(format!("{cp}: \"xform\" chunks are reserved and not allowed in v1"))
                }
                [Value::String(k), ..] if k == "src" || k == "inline" => {
                    Err(format!("{cp}: wrong number of elements for \"{k}\" chunk"))
                }
                _ => Err(format!("{cp}: unknown chunk form")),
            }
        })
        .collect()
}

/// Decodes one parsed line into a typed record. Checks JSON types, required
/// and unknown fields and `null`; value constraints are checked by `check`.
pub fn decode_line(v: &Value) -> R<Line> {
    let mut f = Fields::new("", v)?;
    let ty = f.req_with("type", as_str)?;
    let line = match ty {
        "header" => Line::Header(decode_header(&mut f)?),
        "unit" => Line::Unit(decode_unit(&mut f)?),
        "track" => Line::Track(decode_track(&mut f)?),
        "end" => Line::End(End { unit_count: f.req_with("unit_count", as_uint)? }),
        "error" => Line::Error(ErrorLine {
            code: f.req_with("code", |p, v| as_enum(p, v, ErrorCode::parse))?,
            message: f.req_with("message", as_str)?.to_string(),
        }),
        other => return Err(format!("type: unknown line type \"{other}\"")),
    };
    f.finish()?;
    Ok(line)
}

fn decode_header(f: &mut Fields) -> R<Header> {
    let format = f.req_with("format", as_str)?;
    if format != FORMAT {
        return Err(format!("format: expected \"{FORMAT}\", found \"{format}\""));
    }
    let version = f.req_with("version", as_int)?;
    if version != VERSION {
        return Err(format!("version: unsupported version {version}"));
    }
    let mut pf = Fields::new("parser", f.req("parser")?)?;
    let parser =
        ParserInfo { name: pf.req_with("name", as_str)?.into(), version: pf.req_with("version", as_str)?.into() };
    pf.finish()?;
    let sources = f.req_with("sources", as_array)?;
    let sources = sources
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut sf = Fields::new(format!("sources[{i}]"), s)?;
            let src = Source {
                id: sf.req_with("id", as_uint)?,
                size: sf.req_with("size", as_uint)?,
                sha256: sf.opt_with("sha256", as_str)?.map(str::to_string),
                path: sf.opt_with("path", as_str)?.map(str::to_string),
            };
            sf.finish()?;
            Ok(src)
        })
        .collect::<R<Vec<_>>>()?;
    let mut params = BTreeMap::new();
    if let Some(pv) = f.opt("params")? {
        let Value::Object(members) = pv else { return Err(format!("params: expected object, found {}", pv.kind())) };
        for (k, v) in members {
            let p = format!("params.{k}");
            let pv = match v {
                Value::Number(_) => ParamValue::Int(as_int(&p, v)?),
                Value::Array(_) => ParamValue::Rational(as_rational(&p, v)?),
                Value::String(s) => ParamValue::String(s.clone()),
                other => {
                    return Err(format!(
                        "{p}: parameter values must be integer, rational or string, found {}",
                        other.kind()
                    ))
                }
            };
            params.insert(k.clone(), pv);
        }
    }
    Ok(Header { parser, sources, params })
}

fn decode_unit(f: &mut Fields) -> R<Unit> {
    let pts_ns = f.req_with("pts_ns", as_int)?;
    let duration_ns = f.req_with("duration_ns", as_int)?;
    let mut flags = Flags::NONE;
    for (i, fl) in f.req_with("flags", as_array)?.iter().enumerate() {
        let p = format!("flags[{i}]");
        let s = as_str(&p, fl)?;
        let flag = Flag::parse(s).ok_or_else(|| format!("{p}: unknown flag \"{s}\""))?;
        if flags.contains(flag) {
            return Err(format!("{p}: repeated flag \"{s}\""));
        }
        flags.insert(flag);
    }
    let payload = f.req_with("payload", as_chain)?;
    let codec_state = f.opt_with("codec_state", as_chain)?;
    let discard_padding_ns = f.opt_with("discard_padding_ns", as_int)?;
    let block_additions = f
        .opt_with("block_additions", |p, v| {
            as_array(p, v)?
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    let mut bf = Fields::new(format!("{p}[{i}]"), b)?;
                    let ba = BlockAddition { id: bf.req_with("id", as_uint)?, data: bf.req_with("data", as_chain)? };
                    bf.finish()?;
                    Ok(ba)
                })
                .collect::<R<Vec<_>>>()
        })?
        .unwrap_or_default();
    Ok(Unit { pts_ns, duration_ns, flags, payload, codec_state, discard_padding_ns, block_additions })
}

fn decode_track(f: &mut Fields) -> R<Track> {
    let track_type = f.req_with("track_type", |p, v| as_enum(p, v, TrackType::parse))?;
    let codec_id = f.req_with("codec_id", as_str)?.to_string();
    let codec_private = f.opt_with("codec_private", as_chain)?;
    let codec_delay_ns = f.opt_with("codec_delay_ns", as_int)?;
    let seek_preroll_ns = f.opt_with("seek_preroll_ns", as_int)?;
    let video = f.opt_with("video", decode_video)?;
    let audio = f.opt_with("audio", |p, v| {
        let mut af = Fields::new(p, v)?;
        let a = Audio {
            sampling_frequency: af.req_with("sampling_frequency", as_rational)?,
            channels: af.req_with("channels", as_uint)?,
            output_sampling_frequency: af.opt_with("output_sampling_frequency", as_rational)?,
            bit_depth: af.opt_with("bit_depth", as_uint)?,
        };
        af.finish()?;
        Ok(a)
    })?;
    let block_addition_mappings = f
        .opt_with("block_addition_mappings", |p, v| {
            as_array(p, v)?
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let mut mf = Fields::new(format!("{p}[{i}]"), m)?;
                    let bm = BlockAdditionMapping {
                        id_value: mf.opt_with("id_value", as_uint)?,
                        name: mf.opt_with("name", as_str)?.map(str::to_string),
                        kind: mf.req_with("type", as_uint)?,
                        extra_data: mf.opt_with("extra_data", as_chain)?,
                    };
                    mf.finish()?;
                    Ok(bm)
                })
                .collect::<R<Vec<_>>>()
        })?
        .unwrap_or_default();
    let requires_lacing = f
        .opt_with("requires_lacing", |p, v| match v {
            Value::Bool(b) => Ok(*b),
            _ => Err(format!("{p}: expected boolean, found {}", v.kind())),
        })?
        .unwrap_or(false);
    Ok(Track {
        track_type,
        codec_id,
        codec_private,
        codec_delay_ns,
        seek_preroll_ns,
        video,
        audio,
        block_addition_mappings,
        requires_lacing,
    })
}

fn decode_video(p: &str, v: &Value) -> R<Video> {
    let mut f = Fields::new(p, v)?;
    let video = Video {
        pixel_width: f.req_with("pixel_width", as_uint)?,
        pixel_height: f.req_with("pixel_height", as_uint)?,
        pixel_crop_left: f.opt_with("pixel_crop_left", as_uint)?,
        pixel_crop_top: f.opt_with("pixel_crop_top", as_uint)?,
        pixel_crop_right: f.opt_with("pixel_crop_right", as_uint)?,
        pixel_crop_bottom: f.opt_with("pixel_crop_bottom", as_uint)?,
        display_width: f.opt_with("display_width", as_uint)?,
        display_height: f.opt_with("display_height", as_uint)?,
        display_unit: f.opt_with("display_unit", |p, v| as_enum(p, v, DisplayUnit::parse))?,
        interlace: f.opt_with("interlace", |p, v| as_enum(p, v, Interlace::parse))?,
        field_order: f.opt_with("field_order", as_uint)?,
        stereo_mode: f.opt_with("stereo_mode", as_uint)?,
        alpha_mode: f.opt_with("alpha_mode", as_uint)?,
        nominal_frame_rate: f.opt_with("nominal_frame_rate", as_rational)?,
        default_decoded_field_duration_ns: f.opt_with("default_decoded_field_duration_ns", as_int)?,
        uncompressed_fourcc: f.opt_with("uncompressed_fourcc", |p, v| {
            let b = as_bytes(p, v)?;
            <[u8; 4]>::try_from(b.as_slice()).map_err(|_| format!("{p}: must encode exactly 4 bytes"))
        })?,
        colour: f.opt_with("colour", decode_colour)?,
        projection: f.opt_with("projection", |p, v| {
            let mut pf = Fields::new(p, v)?;
            let pr = Projection {
                kind: pf.req_with("type", |p, v| as_enum(p, v, ProjectionType::parse))?,
                private: pf.opt_with("private", as_chain)?,
                yaw: pf.opt_with("yaw", as_real)?,
                pitch: pf.opt_with("pitch", as_real)?,
                roll: pf.opt_with("roll", as_real)?,
            };
            pf.finish()?;
            Ok(pr)
        })?,
    };
    f.finish()?;
    Ok(video)
}

fn decode_colour(p: &str, v: &Value) -> R<Colour> {
    let mut f = Fields::new(p, v)?;
    let c = Colour {
        matrix_coefficients: f.opt_with("matrix_coefficients", as_uint)?,
        bits_per_channel: f.opt_with("bits_per_channel", as_uint)?,
        chroma_subsampling_horz: f.opt_with("chroma_subsampling_horz", as_uint)?,
        chroma_subsampling_vert: f.opt_with("chroma_subsampling_vert", as_uint)?,
        cb_subsampling_horz: f.opt_with("cb_subsampling_horz", as_uint)?,
        cb_subsampling_vert: f.opt_with("cb_subsampling_vert", as_uint)?,
        chroma_siting_horz: f.opt_with("chroma_siting_horz", as_uint)?,
        chroma_siting_vert: f.opt_with("chroma_siting_vert", as_uint)?,
        range: f.opt_with("range", as_uint)?,
        transfer_characteristics: f.opt_with("transfer_characteristics", as_uint)?,
        primaries: f.opt_with("primaries", as_uint)?,
        max_cll: f.opt_with("max_cll", as_uint)?,
        max_fall: f.opt_with("max_fall", as_uint)?,
        mastering: f.opt_with("mastering", |p, v| {
            let mut mf = Fields::new(p, v)?;
            let m = Mastering {
                primary_r_chromaticity_x: mf.opt_with("primary_r_chromaticity_x", as_real)?,
                primary_r_chromaticity_y: mf.opt_with("primary_r_chromaticity_y", as_real)?,
                primary_g_chromaticity_x: mf.opt_with("primary_g_chromaticity_x", as_real)?,
                primary_g_chromaticity_y: mf.opt_with("primary_g_chromaticity_y", as_real)?,
                primary_b_chromaticity_x: mf.opt_with("primary_b_chromaticity_x", as_real)?,
                primary_b_chromaticity_y: mf.opt_with("primary_b_chromaticity_y", as_real)?,
                white_point_chromaticity_x: mf.opt_with("white_point_chromaticity_x", as_real)?,
                white_point_chromaticity_y: mf.opt_with("white_point_chromaticity_y", as_real)?,
                luminance_max: mf.opt_with("luminance_max", as_real)?,
                luminance_min: mf.opt_with("luminance_min", as_real)?,
            };
            mf.finish()?;
            Ok(m)
        })?,
    };
    f.finish()?;
    Ok(c)
}
