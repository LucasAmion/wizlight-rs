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

mod request;
mod response;

pub use request::Request;
pub use response::{DeviceError, Response};
