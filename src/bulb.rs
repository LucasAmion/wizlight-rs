//! A handle to one bulb.

use std::net::{IpAddr, SocketAddr};

use crate::error::{Error, Result};
use crate::protocol::{
    BulbData, BulbType, KelvinRange, ModelConfig, Pilot, PilotBuilder, Power, Request, Response,
    Scene, Success, SystemConfig, UserConfig,
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
    /// Returns [`Error::InvalidParam`] if the builder set no field or two
    /// conflicting colour modes, [`Error::Device`] if the bulb acknowledged
    /// with `success: false`, and otherwise whatever
    /// [`request`](Bulb::request) returns.
    pub async fn set_pilot(&self, pilot: &PilotBuilder) -> Result<()> {
        self.write("setPilot", &pilot.set_pilot()?).await
    }

    /// Applies a pilot built with [`PilotBuilder`] via `setState`.
    ///
    /// Same params shape as [`set_pilot`](Bulb::set_pilot). On measured
    /// firmware this still turns the bulb on when colour, temperature or a
    /// scene is present.
    ///
    /// # Errors
    ///
    /// As [`set_pilot`](Bulb::set_pilot).
    pub async fn set_state(&self, pilot: &PilotBuilder) -> Result<()> {
        self.write("setState", &pilot.set_state()?).await
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
    /// **Measured on `ESP25_SHRGB_01` fw 1.38.0: this does not work.** The
    /// bulb refuses it with `-32600 Invalid Request` — with `params: {}`, with
    /// `params: null`, and with no `params` key at all — and carries on
    /// running. So expect [`Error::Device`] from that model rather than a
    /// reboot. The code matters: `-32601 Method not found` would mean the
    /// firmware lacked the method, and it does not.
    ///
    /// The method is kept because other models and firmware may well
    /// implement it — `pywizlight` exposes it, though it sends it and ignores
    /// the reply, which is presumably why the refusal went unnoticed.
    ///
    /// **Fire and forget** as far as silence goes: a device that really did
    /// reboot has an obvious reason not to answer, so a timeout is treated as
    /// success. An explicit refusal is still an error.
    ///
    /// # Errors
    ///
    /// See [`request`](Bulb::request), less [`Error::Timeout`].
    pub async fn reboot(&self) -> Result<()> {
        self.fire_and_forget("reboot").await
    }

    /// Factory-resets the bulb, unpairing it and clearing its Wi-Fi
    /// credentials. There is no way to undo this over the network.
    ///
    /// **Never measured, and deliberately not**: finding out what it returns
    /// costs a bulb that has to be paired again from the app. Given
    /// [`reboot`](Bulb::reboot) is refused outright on the hardware here, do
    /// not assume this one works either.
    ///
    /// Fire and forget, for the reasons on [`reboot`](Bulb::reboot).
    ///
    /// # Errors
    ///
    /// See [`request`](Bulb::request), less [`Error::Timeout`].
    pub async fn reset(&self) -> Result<()> {
        self.fire_and_forget("reset").await
    }

    /// The bulb's usable Kelvin range.
    ///
    /// Tries `getModelConfig` first (firmware after 1.22). If that method is
    /// missing, falls back to `getUserConfig`'s `extRange` / `whiteRange`.
    /// Returns `None` only when neither source reports a range.
    ///
    /// This is the range that means something. The wire accepts far more —
    /// measured on `ESP25_SHRGB_01` fw 1.38.0, `temp: 12000` is taken and
    /// clamped by a bulb that reports 2200–6500.
    ///
    /// # Errors
    ///
    /// Propagates transport and parse failures. A missing method is not an
    /// error here — it is the reason to try the next source.
    pub async fn kelvin_range(&self) -> Result<Option<KelvinRange>> {
        Ok(self.capabilities().await?.kelvin_range)
    }

    /// What kind of device this is, and what it can be asked to do.
    ///
    /// Reads `getSystemConfig` for the `moduleName` the capabilities are
    /// derived from, then whichever of `getModelConfig` and `getUserConfig`
    /// the firmware answers for the ranges the name cannot carry.
    ///
    /// Nothing is cached: a caller that needs this more than once should hold
    /// on to it, since it costs at least two round trips.
    ///
    /// ```no_run
    /// # use std::net::{IpAddr, Ipv4Addr};
    /// # use wizlight::Bulb;
    /// # async fn example() -> Result<(), wizlight::Error> {
    /// let bulb = Bulb::connect(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 5))).await?;
    /// let bulb_type = bulb.bulb_type().await?;
    /// if bulb_type.features.color {
    ///     println!("{} does colour", bulb_type.class);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// [`Error::UnknownModel`] if the bulb cannot describe itself — see
    /// [`BulbType::from_data`] — and otherwise whatever
    /// [`request`](Bulb::request) returns.
    pub async fn bulb_type(&self) -> Result<BulbType> {
        let system = self.get_system_config().await?;
        let capabilities = self.capabilities().await?;

        BulbType::from_data(&BulbData {
            module_name: system.module_name.as_deref(),
            type_id: system.type_id,
            fw_version: system.fw_version.as_deref(),
            kelvin_range: capabilities.kelvin_range,
            // `getModelConfig` wins where it exists; `drvConf` is what
            // firmware before 1.22 offers instead.
            white_channels: capabilities.white_channels.or(system.white_channels()),
            white_to_color_ratio: capabilities
                .white_to_color_ratio
                .or(system.white_to_color_ratio()),
            fan_speed_range: capabilities.fan_speed_range,
        })
    }

    /// Every scene this bulb can play, in id order.
    ///
    /// Derived from the bulb's class, so it costs what
    /// [`bulb_type`](Bulb::bulb_type) costs — at least two round trips. A caller
    /// that already knows the class wants [`BulbType::scenes`], and one
    /// building a picker before any bulb is chosen wants
    /// [`Scene::all`](crate::protocol::Scene::all): the table is a `const` and
    /// needs no device.
    ///
    /// ```no_run
    /// # use std::net::{IpAddr, Ipv4Addr};
    /// # use wizlight::Bulb;
    /// # async fn example() -> Result<(), wizlight::Error> {
    /// let bulb = Bulb::connect(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 5))).await?;
    /// for scene in bulb.scenes().await? {
    ///     println!("{:>3}  {}", scene.id().get(), scene.name());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// As [`bulb_type`](Bulb::bulb_type).
    pub async fn scenes(&self) -> Result<Vec<Scene>> {
        Ok(self.bulb_type().await?.scenes().collect())
    }

    /// Reads whichever config method the firmware answers.
    ///
    /// One helper because the fallback is the same question every time: after
    /// 1.22 the answers live in `getModelConfig`, before it in
    /// `getUserConfig`, and a bulb may implement the newer method without the
    /// range being in it. A method the firmware lacks is not an error here —
    /// it is the reason to ask the next one.
    async fn capabilities(&self) -> Result<Capabilities> {
        let model = match self.get_model_config().await {
            Ok(config) => Some(config),
            Err(Error::NotSupported { .. }) => None,
            Err(err) => return Err(err),
        };
        let capabilities = Capabilities::from_model(model.as_ref());
        if capabilities.kelvin_range.is_some() {
            return Ok(capabilities);
        }

        let user = match self.get_user_config().await {
            Ok(config) => Some(config),
            Err(Error::NotSupported { .. }) => None,
            Err(err) => return Err(err),
        };
        Ok(Capabilities {
            kelvin_range: user.as_ref().and_then(UserConfig::kelvin_range),
            fan_speed_range: user
                .and_then(|config| config.fan_speed)
                .or(capabilities.fan_speed_range),
            ..capabilities
        })
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

    /// Sends a write and insists the bulb actually accepted it.
    ///
    /// `{"success": false}` has not been observed on any bulb here, but it is
    /// the shape the protocol allows for a refusal that is not an `error`
    /// envelope, and a write that silently did nothing is the one outcome a
    /// caller must not miss.
    async fn write(&self, method: &str, request: &Request) -> Result<()> {
        let ack: Success = self.request(request).await?.parse_result()?;
        if ack.success {
            Ok(())
        } else {
            Err(Error::Device {
                method: method.to_owned(),
                code: 0,
                message: "bulb acknowledged with `success: false`".to_owned(),
            })
        }
    }

    /// Sends a request whose reply, if any, is not worth waiting for.
    async fn fire_and_forget(&self, method: &str) -> Result<()> {
        match self.request(&Request::new(method)).await {
            Ok(_) | Err(Error::Timeout { .. }) => Ok(()),
            Err(err) => Err(err),
        }
    }
}

/// The parts of a [`BulbType`] that only the config methods can answer, from
/// whichever of them the firmware implements.
#[derive(Clone, Copy, Debug, Default)]
struct Capabilities {
    kelvin_range: Option<KelvinRange>,
    fan_speed_range: Option<u32>,
    white_channels: Option<u32>,
    white_to_color_ratio: Option<u32>,
}

impl Capabilities {
    fn from_model(model: Option<&ModelConfig>) -> Self {
        let Some(model) = model else {
            return Self::default();
        };
        Self {
            kelvin_range: model.kelvin_range(),
            fan_speed_range: model.fan_speed,
            white_channels: model.nowc,
            white_to_color_ratio: model.wcr,
        }
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
