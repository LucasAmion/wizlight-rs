//! What talking to a bulb can fail with.

use std::net::SocketAddr;
use std::time::Duration;

use crate::protocol::DeviceError;

/// A [`Result`](std::result::Result) with this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Everything `wizlight` can fail with.
///
/// Each failure mode is a distinct variant, because callers react to them
/// differently: a [`Timeout`](Error::Timeout) is worth retrying later, a
/// [`NotSupported`](Error::NotSupported) never is.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// No reply arrived before the last attempt ran out of patience.
    ///
    /// The request may still have been acted on: WiZ acknowledges a `setPilot`
    /// after applying it, so a lost acknowledgement is indistinguishable from a
    /// lost request.
    #[error("no reply from {addr} to `{method}` after {attempts} attempts in {elapsed:?}")]
    Timeout {
        /// The method that went unanswered.
        method: String,
        /// The bulb that did not answer.
        addr: SocketAddr,
        /// How many datagrams were sent.
        attempts: u32,
        /// How long the whole exchange took before giving up.
        elapsed: Duration,
    },

    /// The socket failed.
    #[error("udp error: {0}")]
    Io(#[from] std::io::Error),

    /// A reply could not be parsed, or a request could not be serialised.
    #[error("malformed json: {0}")]
    Json(#[from] serde_json::Error),

    /// The bulb answered with an `error` envelope that has no more specific
    /// variant here.
    #[error("bulb rejected `{method}`: {message} (code {code})")]
    Device {
        /// The method that was rejected.
        method: String,
        /// The JSON-RPC style code the bulb returned.
        code: i64,
        /// The bulb's own description of the problem.
        message: String,
    },

    /// The bulb does not have the requested capability.
    ///
    /// Returned for the bulb's own `-32601 Method not found` — older firmware
    /// has no `getModelConfig`, and only sockets answer `getPower` — and by
    /// client-side capability checks.
    #[error("`{method}` is not supported by this bulb")]
    NotSupported {
        /// The method, or the feature, that is unavailable.
        method: String,
    },

    /// A parameter was out of range or otherwise unacceptable.
    ///
    /// Returned for the bulb's `-32602 Invalid params` and by client-side
    /// validation. The latter is the important one: the bulb is not a reliable
    /// validator, and silently clamps an out-of-range `dimming` while reporting
    /// success.
    #[error("invalid parameter: {message}")]
    InvalidParam {
        /// What was wrong with it.
        message: String,
    },
}

impl Error {
    /// Turns a bulb's `error` envelope into the most specific variant for it.
    pub(crate) fn from_device(method: &str, error: DeviceError) -> Self {
        match error.code {
            -32601 => Self::NotSupported {
                method: method.to_owned(),
            },
            -32602 => Self::InvalidParam {
                message: format!("bulb rejected the params of `{method}`: {}", error.message),
            },
            code => Self::Device {
                method: method.to_owned(),
                code,
                message: error.message,
            },
        }
    }
}
