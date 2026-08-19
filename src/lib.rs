#![doc = include_str!("../README.md")]
#![deny(missing_docs)]

mod bulb;
mod discovery;
mod error;
mod transport;

pub mod protocol;

#[cfg(feature = "cli")]
pub mod cli;

pub use bulb::{Bulb, PORT};
pub use discovery::{
    BROADCAST, DEFAULT_INTERVAL, DEFAULT_WAIT, Discovered, Discovery, DiscoveryStream,
};
pub use error::{Error, Result};
pub use protocol::{
    Adjustable, BulbClass, BulbData, BulbType, Channel, Derivation, DeviceError, Devices, Dimming,
    Features, Heads, Kelvin, KelvinRange, ModelConfig, ModuleName, Pilot, PilotBuilder,
    PilotParams, Power, Ratio, Request, Response, Scene, SceneId, Speed, Success, SystemConfig,
    UserConfig,
};
pub use transport::RetryPolicy;
