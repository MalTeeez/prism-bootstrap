//! The component model - pure, serde-derived data shared across stages.
//!
//! - [`pack`]: `mmc-pack.json` (which patches to load).
//! - [`patch`]: one `patches/<uid>.json` (the component schema).
//! - [`profile`]: the merged result the downstream stages consume.
//!
//! Some fields are parsed for completeness but not yet read anywhere; the
//! module-level allow below keeps them rather than dropping them from the model.
#![allow(dead_code)]

pub mod artifact;
pub mod pack;
pub mod patch;
pub mod profile;
