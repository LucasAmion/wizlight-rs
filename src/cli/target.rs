//! Turning a `<target>` argument into the bulbs a command acts on.
//!
//! A target is an address or a MAC. An address is routable and is used as it
//! stands; a MAC is not, so it is resolved by a discovery run that stops as
//! soon as that bulb answers.
//!
//! The MAC form is the one worth having. DHCP moves a bulb's address with no
//! warning — the two this crate was developed against have changed address
//! three times in a fortnight with nothing reconfigured — while the MAC is the
//! only stable identity the protocol exposes.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::time::Duration;

use tokio::time::{Instant, timeout_at};

use crate::{Bulb, Discovery, Result, RetryPolicy};

/// A `<target>` argument: what was typed, and what it turned out to mean.
///
/// The original spelling is kept so that output can echo the argument back
/// exactly as given. A script that fans out over targets it supplied should
/// not have to recognise its own input in a normalised form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetSpec {
    raw: String,
    kind: Kind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Address(SocketAddr),
    Mac(String),
}

/// A `<target>` that is neither an address nor a MAC.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("`{0}` is neither an IP address nor a MAC")]
pub struct BadTarget(pub String);

impl TargetSpec {
    /// The argument as it was typed.
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// The address to send to, when the target named one.
    #[must_use]
    pub fn address(&self) -> Option<SocketAddr> {
        match &self.kind {
            Kind::Address(addr) => Some(*addr),
            Kind::Mac(_) => None,
        }
    }

    /// The MAC to look for, normalised, when the target named one.
    #[must_use]
    pub fn mac(&self) -> Option<&str> {
        match &self.kind {
            Kind::Mac(mac) => Some(mac),
            Kind::Address(_) => None,
        }
    }
}

impl fmt::Display for TargetSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl FromStr for TargetSpec {
    type Err = BadTarget;

    /// Accepts `192.168.0.7`, `192.168.0.7:38899`, `9877d5230f0a` and
    /// `98:77:d5:23:0f:0a`.
    ///
    /// The two forms cannot be confused: an address has dots or colons around
    /// decimal digits, and a MAC is exactly twelve hex digits once its
    /// separators are dropped.
    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        let kind = if let Ok(addr) = input.parse::<SocketAddr>() {
            Kind::Address(addr)
        } else if let Ok(ip) = input.parse::<IpAddr>() {
            Kind::Address(SocketAddr::new(ip, crate::PORT))
        } else {
            Kind::Mac(normalise_mac(input).ok_or_else(|| BadTarget(input.to_owned()))?)
        };
        Ok(Self {
            raw: input.to_owned(),
            kind,
        })
    }
}

/// Lowercases a MAC and drops its separators, or `None` if it is not one.
///
/// `98:77:d5:23:0f:0a`, `98-77-d5-23-0f-0a` and `9877d5230f0a` are the same
/// bulb. The protocol spells it the last way, so that is what everything
/// downstream compares against.
fn normalise_mac(input: &str) -> Option<String> {
    let digits: String = input.chars().filter(|c| !matches!(c, ':' | '-')).collect();
    (digits.len() == 12 && digits.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| digits.to_ascii_lowercase())
}

/// Nothing answered to what was asked for.
///
/// Distinct from a bulb that was found and then stopped talking, because the
/// two call for different reactions: this one wants a re-scan, the other wants
/// a retry. They exit with different codes for the same reason.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum NotFound {
    /// No bulb with that MAC answered the scan.
    #[error("no bulb with MAC {mac} answered in {}s — it may be powered off, or on another subnet", .wait.as_secs_f32())]
    Mac {
        /// The MAC that was looked for.
        mac: String,
        /// How long the scan ran.
        wait: Duration,
    },
    /// `--all` found nothing to act on.
    #[error("no bulbs answered in {}s, so there was nothing to act on", .wait.as_secs_f32())]
    Nothing {
        /// How long the scan ran.
        wait: Duration,
    },
}

/// A bulb a command is about to act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    /// Where to send requests.
    pub addr: SocketAddr,
    /// Its MAC, when it came from a scan. A bulb named by address is talked to
    /// without first asking who it is.
    pub mac: Option<String>,
}

impl Resolved {
    /// How output refers to this bulb: its MAC if that is known, else its
    /// address.
    #[must_use]
    pub fn label(&self) -> String {
        match &self.mac {
            Some(mac) => mac.clone(),
            None => self.addr.ip().to_string(),
        }
    }

    /// Opens a connection to it.
    ///
    /// # Errors
    ///
    /// [`Error::Io`](crate::Error::Io) if the local socket cannot be bound.
    pub async fn connect(&self, policy: &RetryPolicy) -> Result<Bulb> {
        Ok(Bulb::connect_to(self.addr)
            .await?
            .with_policy(policy.clone()))
    }
}

/// Resolves one target, scanning for it if it was named by MAC.
///
/// # Errors
///
/// [`NotFound::Mac`] if the scan ends without that bulb answering, or
/// whatever starting the scan failed with.
pub async fn resolve(
    spec: &TargetSpec,
    discovery: &Discovery,
    wait: Duration,
) -> anyhow::Result<Resolved> {
    if let Some(addr) = spec.address() {
        return Ok(Resolved { addr, mac: None });
    }
    let mac = spec.mac().expect("a target is an address or a MAC");

    // Stops at the bulb it was looking for rather than running the full
    // window: a MAC names one bulb, and on the hardware measured here it
    // answers in about 100 ms. Waiting the remaining five seconds to hear
    // nothing new would make every command feel broken.
    let mut stream = discovery.stream().await?;
    let deadline = Instant::now() + wait;
    while let Ok(Some(bulb)) = timeout_at(deadline, stream.recv()).await {
        if bulb.mac == mac {
            tracing::debug!(%mac, addr = %bulb.addr, "resolved");
            return Ok(Resolved {
                addr: bulb.addr,
                mac: Some(bulb.mac),
            });
        }
    }
    Err(NotFound::Mac {
        mac: mac.to_owned(),
        wait,
    }
    .into())
}

/// Every bulb a scan finds, for `--all`.
///
/// # Errors
///
/// [`NotFound::Nothing`] if the scan finds none — unlike `discover`, where an
/// empty list is the answer, a fan-out with nothing to fan out to did not do
/// what was asked.
pub async fn resolve_all(discovery: &Discovery, wait: Duration) -> anyhow::Result<Vec<Resolved>> {
    let found = discovery.collect(wait).await?;
    if found.is_empty() {
        return Err(NotFound::Nothing { wait }.into());
    }
    let mut bulbs: Vec<Resolved> = found
        .into_iter()
        .map(|bulb| Resolved {
            addr: bulb.addr,
            mac: Some(bulb.mac),
        })
        .collect();
    // Answering order is arrival order, which is effectively random. Sorting
    // by MAC keeps a fan-out's output stable between runs, so two of them can
    // be diffed.
    bulbs.sort_by(|a, b| a.mac.cmp(&b.mac));
    Ok(bulbs)
}
