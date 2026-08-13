//! Test support shared by the library and CLI test binaries.
//!
//! Each file in `tests/` is its own binary, so anything unused by a given one
//! would otherwise warn.
#![allow(dead_code)]

pub mod mock_bulb;
pub mod udp;
