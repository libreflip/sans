//! Shared test support for driving the Sans application headlessly.

#![allow(dead_code)]

pub mod cameras;
/// The core crate's fake Fibre board and commissioned lift-axis profile.
#[path = "../../../sans-core/tests/support/mod.rs"]
pub mod core_support;
pub mod fixtures;
pub mod pixels;
pub mod proofs;
pub mod session;
