#![doc = include_str!("../README.md")]
#![deny(missing_docs)]

mod bulb;
mod error;
mod transport;

pub mod protocol;

#[cfg(feature = "cli")]
pub mod cli;

pub use bulb::{Bulb, PORT};
pub use error::{Error, Result};
pub use protocol::{DeviceError, Request, Response};
pub use transport::RetryPolicy;
