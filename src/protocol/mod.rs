//! The wire format: what goes into a datagram and what comes back.
//!
//! Every exchange with a bulb is one JSON object out and one JSON object back
//! on UDP port [`PORT`](crate::PORT). The envelope is small — a method name,
//! some params, and either a `result` or an `error`:
//!
//! ```json
//! {"method":"getPilot","params":{}}
//! {"method":"getPilot","env":"pro","result":{"mac":"9877d5230f0a","state":true,"dimming":100}}
//! {"method":"getWifiConfig","env":"pro","error":{"code":-32601,"message":"Method not found"}}
//! ```
//!
//! # Forward compatibility
//!
//! Nothing here rejects a field it does not know. Firmware revisions add
//! fields — the `ESP25_SHRGB_01` on 1.38.0 that this crate was written against
//! returns seven `getModelConfig` fields that no `pywizlight` fixture has — so
//! `deny_unknown_fields` anywhere in the parse path would turn a firmware
//! update into a broken client.
//!
//! The same rule applies to *values*, which is why results carry plain
//! integers while requests carry validated newtypes like [`Dimming`]. The
//! write-side bounds are what one firmware was measured to accept or usefully
//! honour, and they are not a promise about what any bulb will *report*: the
//! hardware here accepts `dimming: 0`, clamps it, and answers `success`, so a
//! model that reported the unclamped value back would be entirely within the
//! protocol. A result type enforcing the write bound would turn that into a
//! parse failure, and a firmware update into a broken client.

mod config;
mod model;
mod pilot;
mod request;
mod response;
mod scene;
mod types;

pub use config::{ModelConfig, Power, SystemConfig, UserConfig};
pub use model::{
    BulbClass, BulbData, BulbType, Derivation, Features, Heads, KelvinRange, ModuleName,
};
pub use pilot::{Pilot, PilotBuilder, PilotParams, Success};
pub use request::Request;
pub use response::{DeviceError, Response};
pub use scene::{Adjustable, Scene};
pub use types::{Channel, Devices, Dimming, Kelvin, Ratio, SceneId, Speed};
