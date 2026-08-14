//! The request half of the envelope.

use serde::Serialize;
use serde_json::{Value, json};

use crate::error::Result;

/// A request to a bulb: a method name and its params.
///
/// `params` is always sent, as an empty object when there is nothing to say —
/// that is what the official app does (`{"method":"getPilot","params":{}}` in
/// the recorded traffic). Reads do not care either way; writes very much do,
/// and not in the direction you would guess: a `setPilot` with no `params` key
/// is refused with `-32602 Invalid params`, while the same request carrying
/// `"params":{}` is refused with `-32600 Invalid Request`. Both are meaningless
/// requests, so neither is worth building, but it does mean the two spellings
/// are not interchangeable.
///
/// There is deliberately no envelope-level `id`. The one `id` in the protocol
/// belongs to `registration`'s params, and no reply ever echoes it back, so it
/// cannot be used to match responses to requests.
///
/// ```
/// use wizlight::Request;
///
/// let request = Request::new("getPilot");
/// assert_eq!(request.to_string(), r#"{"method":"getPilot","params":{}}"#);
///
/// let request = Request::with_params("setPilot", &serde_json::json!({"state": true}))?;
/// assert_eq!(request.to_string(), r#"{"method":"setPilot","params":{"state":true}}"#);
/// # Ok::<(), wizlight::Error>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Request {
    /// The method to call, e.g. `getPilot` or `setPilot`.
    pub method: String,
    /// The params object. Empty rather than absent when unused.
    pub params: Value,
}

impl Request {
    /// A request with no params.
    pub fn new(method: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            params: json!({}),
        }
    }

    /// A request whose params are whatever `params` serialises to.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Json`](crate::Error::Json) if `params` cannot be
    /// serialised.
    pub fn with_params<P: Serialize>(method: impl Into<String>, params: &P) -> Result<Self> {
        Ok(Self {
            method: method.into(),
            params: serde_json::to_value(params)?,
        })
    }
}

impl std::fmt::Display for Request {
    /// Renders the datagram exactly as it goes on the wire.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match serde_json::to_string(self) {
            Ok(json) => f.write_str(&json),
            Err(_) => Err(std::fmt::Error),
        }
    }
}
