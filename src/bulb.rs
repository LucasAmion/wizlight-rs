//! A handle to one bulb.

use std::net::{IpAddr, SocketAddr};

use crate::error::{Error, Result};
use crate::protocol::{
    ModelConfig, Pilot, PilotBuilder, Power, Request, Response, Success, SystemConfig, UserConfig,
};
use crate::transport::{RetryPolicy, Transport};

/// The UDP port every WiZ device listens on.
pub const PORT: u16 = 38899;

/// One bulb, and the socket used to talk to it.
///
/// A `Bulb` owns its socket, so two handles to the same device are independent
/// — and a single handle is `Send + Sync`, so it can be shared across tasks
/// instead. Requests from concurrent callers are serialised and paced.
///
/// ```no_run
/// use std::net::{IpAddr, Ipv4Addr};
/// use wizlight::Bulb;
///
/// # async fn example() -> Result<(), wizlight::Error> {
/// let bulb = Bulb::connect(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 5))).await?;
/// let pilot = bulb.get_pilot().await?;
/// println!("{:?}", pilot.state);
/// # Ok(())
/// # }
/// ```
pub struct Bulb {
    addr: SocketAddr,
    transport: Transport,
    policy: RetryPolicy,
}

impl Bulb {
    /// Opens a socket for the bulb at `ip`, on the standard [`PORT`].
    ///
    /// Nothing is sent, so this succeeds whether or not anything is listening;
    /// the first [`request`](Bulb::request) is what finds out.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) if the local socket cannot be
    /// bound.
    pub async fn connect(ip: IpAddr) -> Result<Self> {
        Self::connect_to(SocketAddr::new(ip, PORT)).await
    }

    /// Opens a socket for a bulb reachable at some other port — a test double,
    /// or something behind a forwarder.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) if the local socket cannot be
    /// bound.
    pub async fn connect_to(addr: SocketAddr) -> Result<Self> {
        Ok(Self {
            addr,
            transport: Transport::bind().await?,
            policy: RetryPolicy::default(),
        })
    }

    /// Replaces the [`RetryPolicy`], whose defaults suit a bulb on the same
    /// LAN. Something reached over a VPN, or a whole shelf of bulbs being
    /// polled at once, may want more patience.
    #[must_use]
    pub fn with_policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Where the bulb is.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Which local address the bulb's replies arrive on.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) if the socket cannot report it.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.transport.local_addr()
    }

    /// The policy in force.
    pub fn policy(&self) -> &RetryPolicy {
        &self.policy
    }

    /// Reads the bulb's current pilot state.
    ///
    /// # Errors
    ///
    /// See [`request`](Bulb::request).
    pub async fn get_pilot(&self) -> Result<Pilot> {
        self.request(&Request::new("getPilot"))
            .await?
            .parse_result()
    }

    /// Applies a pilot built with [`PilotBuilder`] via `setPilot`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidParam`] if the builder is empty, and otherwise
    /// whatever [`request`](Bulb::request) returns.
    pub async fn set_pilot(&self, pilot: &PilotBuilder) -> Result<Success> {
        self.request(&pilot.set_pilot()?).await?.parse_result()
    }

    /// Applies a pilot built with [`PilotBuilder`] via `setState`.
    ///
    /// Same params shape as [`set_pilot`](Bulb::set_pilot). On measured
    /// firmware this still turns the bulb on when colour, temperature or a
    /// scene is present.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidParam`] if the builder is empty, and otherwise
    /// whatever [`request`](Bulb::request) returns.
    pub async fn set_state(&self, pilot: &PilotBuilder) -> Result<Success> {
        self.request(&pilot.set_state()?).await?.parse_result()
    }

    /// Reads `getSystemConfig`.
    ///
    /// # Errors
    ///
    /// See [`request`](Bulb::request).
    pub async fn get_system_config(&self) -> Result<SystemConfig> {
        self.request(&Request::new("getSystemConfig"))
            .await?
            .parse_result()
    }

    /// Reads `getModelConfig`.
    ///
    /// Older firmware answers with `-32601`; see
    /// [`kelvin_range`](Bulb::kelvin_range) for the fallback path.
    ///
    /// # Errors
    ///
    /// See [`request`](Bulb::request).
    pub async fn get_model_config(&self) -> Result<ModelConfig> {
        self.request(&Request::new("getModelConfig"))
            .await?
            .parse_result()
    }

    /// Reads `getUserConfig`.
    ///
    /// # Errors
    ///
    /// See [`request`](Bulb::request).
    pub async fn get_user_config(&self) -> Result<UserConfig> {
        self.request(&Request::new("getUserConfig"))
            .await?
            .parse_result()
    }

    /// Reads `getPower` when the firmware implements it.
    ///
    /// On the measured `ESP25_SHRGB_01` the method exists and always returns
    /// `0`. Treat the number as opaque until a given model is characterised.
    ///
    /// # Errors
    ///
    /// See [`request`](Bulb::request).
    pub async fn get_power(&self) -> Result<Power> {
        self.request(&Request::new("getPower"))
            .await?
            .parse_result()
    }

    /// Asks the bulb to reboot.
    ///
    /// # Errors
    ///
    /// See [`request`](Bulb::request).
    pub async fn reboot(&self) -> Result<Success> {
        self.request(&Request::new("reboot")).await?.parse_result()
    }

    /// Factory-resets the bulb.
    ///
    /// # Errors
    ///
    /// See [`request`](Bulb::request).
    pub async fn reset(&self) -> Result<Success> {
        self.request(&Request::new("reset")).await?.parse_result()
    }

    /// The bulb's usable Kelvin range.
    ///
    /// Tries `getModelConfig` first (firmware after 1.22). If that method is
    /// missing, falls back to `getUserConfig`'s `extRange` / `whiteRange`.
    /// Returns `None` only when neither source reports a range.
    ///
    /// # Errors
    ///
    /// Propagates transport and parse failures. A missing method is not an
    /// error here — it is the reason to try the next source.
    pub async fn kelvin_range(&self) -> Result<Option<(u16, u16)>> {
        match self.get_model_config().await {
            Ok(config) => {
                if let Some(range) = config.kelvin_range() {
                    return Ok(Some(range));
                }
            }
            Err(Error::NotSupported { .. }) => {}
            Err(err) => return Err(err),
        }

        match self.get_user_config().await {
            Ok(config) => Ok(config.kelvin_range()),
            Err(Error::NotSupported { .. }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Sends a request and waits for the bulb's answer.
    ///
    /// Retries per the [`RetryPolicy`]. Replies from anywhere else, and
    /// answers to any other method, are ignored while waiting.
    ///
    /// A retry is a plain retransmission of the same datagram, which is safe
    /// for everything the protocol offers: `setPilot` sets absolute values, so
    /// applying one twice is the same as applying it once.
    ///
    /// # Errors
    ///
    /// - [`Error::Timeout`](crate::Error::Timeout) if no attempt was answered
    /// - [`Error::NotSupported`](crate::Error::NotSupported) if the firmware
    ///   has no such method
    /// - [`Error::InvalidParam`](crate::Error::InvalidParam) if it refused the
    ///   params
    /// - [`Error::Device`](crate::Error::Device) for any other refusal
    /// - [`Error::Json`](crate::Error::Json) if the reply was not the envelope
    /// - [`Error::Io`](crate::Error::Io) if the socket failed
    pub async fn request(&self, request: &Request) -> Result<Response> {
        self.transport
            .exchange(self.addr, request, &self.policy)
            .await
    }
}

impl std::fmt::Debug for Bulb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bulb")
            .field("addr", &self.addr)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}
