//! The component model - pure, serde-derived data shared across phases.
//!
//! - [`pack`]: `mmc-pack.json` (which patches to load).
//! - [`patch`]: one `patches/<uid>.json` (the component schema).
//! - [`profile`]: the merged result every later phase consumes.
//!
//! Many fields are parsed here but only *read* by later phases (rules in
//! phase 2, urls/sha1 in phases 3-4, ...). We model the full schema up front, so
//! the not-yet-consumed fields are deliberately allowed rather than removed.
#![allow(dead_code)]

pub mod pack;
pub mod patch;
pub mod profile;
