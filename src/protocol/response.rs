//! The response half of the envelope.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

/// A reply from a bulb.
///
/// Every field is optional: a reply carries either a `result` or an `error`,
/// the `-32700` parse error carries no `method`, and unknown fields are
/// ignored rather than rejected.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct Response {
    /// The method being answered. Absent when the bulb could not parse the
    /// request far enough to know.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// The bulb's environment, in practice always `pro`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    /// The payload, shaped by the method.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Why the bulb refused.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<DeviceError>,
}

impl Response {
    /// Deserialises `result` into a typed payload.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Json`](crate::Error::Json) if the reply carried no
    /// result, or one that does not fit `T`.
    pub fn parse_result<T: DeserializeOwned>(&self) -> Result<T> {
        match &self.result {
            Some(result) => Ok(serde_json::from_value(result.clone())?),
            None => {
                use serde::de::Error as _;
                Err(serde_json::Error::custom(format!(
                    "reply to `{}` carried no result",
                    self.method.as_deref().unwrap_or("?")
                ))
                .into())
            }
        }
    }
}

/// The `error` object of a refused request.
///
/// Codes follow JSON-RPC: `-32601` for a method the firmware does not
/// implement, `-32602` for params it will not accept, `-32700` for something
/// that was not a request at all.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct DeviceError {
    /// The error code.
    pub code: i64,
    /// The bulb's description of the problem, e.g. `Invalid params`.
    #[serde(default)]
    pub message: String,
}
