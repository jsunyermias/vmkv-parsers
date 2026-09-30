//! Common library for VMKV parsers: the `.vtj` v1 output format.
//!
//! - [`types`]: typed records and their canonical serialization.
//! - [`writer`]: the canonical writer that enforces line order.
//! - [`time`]: exact timestamp arithmetic and the normative rounding.
//! - [`check`] and [`validate`]: the specification's checklist.
//! - [`cli`] and [`source`]: the common parser contract.

pub mod base64;
pub mod check;
pub mod cli;
pub mod json;
pub mod source;
pub mod time;
pub mod types;
pub mod validate;
pub mod writer;

pub use cli::{Context, ParamKind, ParamSpec, ParseError, Parser};
pub use time::{durations_from_pts, round_ns, ticks_to_ns, TimeError, Timeline};
pub use types::*;
pub use writer::{VtjWriter, WriteError};
