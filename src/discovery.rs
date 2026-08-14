//! Finding bulbs on the LAN.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use serde_json::json;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout_at};

use crate::bulb::{Bulb, PORT};
use crate::error::Result;
use crate::protocol::{Request, Response};
use crate::transport::RetryPolicy;

/// Where discovery shouts by default: the all-subnets broadcast address, on the
/// standard [`PORT`].
///
/// `255.255.255.255` reaches every bulb on the directly attached network and
/// nothing beyond it, because routers do not forward it. It is the safe
/// default; a host with several interfaces should name each subnet's own
/// broadcast address with [`Discovery::target`] instead, since the kernel picks
/// only one route for `255.255.255.255`.
pub const BROADCAST: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255)), PORT);

/// How often the registration goes out again, absent [`Discovery::interval`].
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(1);

/// A sensible span to run [`Discovery::collect`] for.
///
/// Measured, not inherited — though it agrees with `pywizlight`'s five seconds.
/// Over 20 broadcasts a second apart to two `ESP25_SHRGB_01` on 1.38.0, one
/// answered 19 and the other 11, the misses falling in runs of up to four
/// consecutive broadcasts. Both answer *unicast* requests with no loss at all,
/// so this is broadcast reception specifically, and it is why a scan shorter
/// than about five seconds will sooner or later come back a bulb short.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(5);

/// The `phoneIp` sent when the local address cannot be worked out.
///
/// It does not matter what it says. `register: false` tells the bulb to *drop*
/// any push registration rather than create one, so nothing is ever sent to
/// this address — `pywizlight` hardcodes the same value.
const UNKNOWN_PHONE_IP: &str = "1.2.3.4";

/// A bulb that answered a discovery broadcast.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Discovered {
    /// Where the reply came from, which is where requests should go.
    pub addr: SocketAddr,
    /// The bulb's MAC, lowercase hex with no separators, e.g. `9877d5230f0a`.
    ///
    /// This is the only stable identity a bulb has: DHCP moves the address
    /// around, and nothing else in the reply distinguishes two bulbs of the
    /// same model.
    pub mac: String,
    /// The reply to the follow-up `getSystemConfig`, if
    /// [`Discovery::system_config`] asked for one and the bulb answered it.
    pub system_config: Option<Response>,
}

impl Discovered {
    /// The bulb's address, without the port.
    pub fn ip(&self) -> IpAddr {
        self.addr.ip()
    }

    /// Opens a [`Bulb`] talking to it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) if the local socket cannot be
    /// bound.
    pub async fn connect(&self) -> Result<Bulb> {
        Bulb::connect_to(self.addr).await
    }
}

/// A discovery run, configured and then started.
///
/// Every WiZ device answers a `registration` broadcast with its MAC, and that
/// is the whole of the discovery protocol. There is no service record, no
/// multicast group and nothing to query: you shout at the subnet and see who
/// answers.
///
/// ```no_run
/// use std::time::Duration;
///
/// use wizlight::Discovery;
///
/// # async fn example() -> Result<(), wizlight::Error> {
/// for bulb in Discovery::new().collect(Duration::from_secs(5)).await? {
///     println!("{} at {}", bulb.mac, bulb.addr);
/// }
/// # Ok(())
/// # }
/// ```
///
/// # Why it keeps broadcasting
///
/// A single broadcast is not enough, and not by a small margin: one of the two
/// bulbs this was measured against ignores broadcasts in runs of up to four
/// seconds at a time while answering unicast requests perfectly — see
/// [`DEFAULT_WAIT`]. A bulb mid-reboot hears nothing either, and one plugged in
/// a second from now has not heard anything yet. So the registration goes out
/// again every [`interval`](Discovery::interval) for as long as the run lasts.
///
/// That is also why answers are [streamed](Discovery::stream) rather than
/// returned in a batch: a UI lists bulbs as they arrive instead of showing
/// nothing for five seconds.
#[derive(Clone, Debug)]
pub struct Discovery {
    targets: Vec<SocketAddr>,
    interval: Duration,
    system_config: bool,
    policy: RetryPolicy,
}

impl Default for Discovery {
    fn default() -> Self {
        Self::new()
    }
}

impl Discovery {
    /// A run that broadcasts to [`BROADCAST`] once a second.
    pub fn new() -> Self {
        Self {
            targets: Vec::new(),
            interval: DEFAULT_INTERVAL,
            system_config: false,
            policy: RetryPolicy::default(),
        }
    }

    /// Adds an address to broadcast to, replacing the [`BROADCAST`] default on
    /// the first call.
    ///
    /// Naming targets explicitly is how a multi-homed host reaches every
    /// subnet — one call per interface broadcast address — and it also accepts
    /// a plain unicast address, for probing one bulb that is known to be there.
    #[must_use]
    pub fn target(mut self, addr: SocketAddr) -> Self {
        self.targets.push(addr);
        self
    }

    /// Sets how often the registration is re-broadcast. Default one second, as
    /// the official app does.
    #[must_use]
    pub fn interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }

    /// Asks each bulb for its `getSystemConfig` before reporting it, so a
    /// caller gets model and firmware without a second round of plumbing.
    ///
    /// This costs a request/response per bulb, and delays each result by one
    /// round trip. A bulb that answers the broadcast but not the follow-up is
    /// still reported, with [`system_config`](Discovered::system_config) set to
    /// `None`: it exists either way.
    #[must_use]
    pub fn system_config(mut self, yes: bool) -> Self {
        self.system_config = yes;
        self
    }

    /// Sets the [`RetryPolicy`] for the `getSystemConfig` follow-up. Ignored
    /// unless [`system_config`](Discovery::system_config) is on — the broadcast
    /// itself is not retried, it is repeated.
    #[must_use]
    pub fn policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Starts broadcasting, and reports bulbs as they answer.
    ///
    /// The run continues until the returned stream is dropped. Each bulb is
    /// reported once; a bulb whose address has changed since it was last seen
    /// is reported again, with the new one.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`](crate::Error::Io) if the socket cannot be bound or
    /// put into broadcast mode, or if the first broadcast cannot be sent —
    /// which is what an unroutable target or a firewall looks like. Later
    /// failures to send are not reported: the next tick tries again, and one
    /// unreachable interface should not end a run that other interfaces are
    /// answering on.
    pub async fn stream(&self) -> Result<DiscoveryStream> {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
        socket.set_broadcast(true)?;

        let targets = self.targets();
        let payload = registration(local_ip_towards(targets[0]))?;
        for target in &targets {
            socket.send_to(&payload, target).await?;
        }

        let (tx, rx) = mpsc::channel(64);
        let run = Run {
            socket,
            targets,
            payload,
            interval: self.interval,
            system_config: self.system_config,
            policy: self.policy.clone(),
            tx,
        };
        Ok(DiscoveryStream {
            rx,
            task: tokio::spawn(run.drive()),
        })
    }

    /// Broadcasts for `wait`, then returns everything that answered.
    ///
    /// The convenience form, for a CLI or a one-off scan. It always takes the
    /// full `wait`, because there is no way to know that the last bulb has
    /// answered — anything that wants to show results earlier wants
    /// [`stream`](Discovery::stream).
    ///
    /// Bulbs are ordered by when they first answered, which in practice is by
    /// how quickly they respond, and each appears once with the most recent
    /// address it was seen at.
    ///
    /// # Errors
    ///
    /// As [`stream`](Discovery::stream).
    pub async fn collect(&self, wait: Duration) -> Result<Vec<Discovered>> {
        let mut stream = self.stream().await?;
        let mut found: Vec<Discovered> = Vec::new();
        let deadline = Instant::now() + wait;

        while let Ok(Some(bulb)) = tokio::time::timeout_at(deadline, stream.recv()).await {
            match found.iter_mut().find(|seen| seen.mac == bulb.mac) {
                Some(seen) => *seen = bulb,
                None => found.push(bulb),
            }
        }
        Ok(found)
    }

    fn targets(&self) -> Vec<SocketAddr> {
        match self.targets.is_empty() {
            true => vec![BROADCAST],
            false => self.targets.clone(),
        }
    }
}

/// Bulbs, as they answer.
///
/// Implements [`Stream`](futures_core::Stream), and has an inherent
/// [`recv`](DiscoveryStream::recv) for callers who would rather not import
/// `StreamExt`. Dropping it stops the broadcasting.
#[derive(Debug)]
pub struct DiscoveryStream {
    rx: mpsc::Receiver<Discovered>,
    task: JoinHandle<()>,
}

impl DiscoveryStream {
    /// Waits for the next bulb.
    ///
    /// Returns `None` only if the run has ended, which it does when the socket
    /// fails; otherwise it waits indefinitely, since a bulb may be plugged in
    /// at any moment. Callers bound the wait themselves — with
    /// [`Discovery::collect`], or `tokio::time::timeout`.
    pub async fn recv(&mut self) -> Option<Discovered> {
        self.rx.recv().await
    }
}

impl Drop for DiscoveryStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl futures_core::Stream for DiscoveryStream {
    type Item = Discovered;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

/// The state of a run, owned by the background task.
struct Run {
    socket: UdpSocket,
    targets: Vec<SocketAddr>,
    payload: Vec<u8>,
    interval: Duration,
    system_config: bool,
    policy: RetryPolicy,
    tx: mpsc::Sender<Discovered>,
}

impl Run {
    /// Listens for answers, re-broadcasting whenever the wait runs long enough,
    /// until the consumer goes away or the socket does.
    ///
    /// Written as one timed receive rather than a `select!` over a ticker so
    /// that the library does not need tokio's `macros` feature, and with it a
    /// proc-macro dependency, for a loop this shape.
    async fn drive(self) {
        let mut seen: HashMap<String, SocketAddr> = HashMap::new();
        let mut buf = vec![0u8; 4096];
        // The first broadcast already went out, from `stream`.
        let mut next_broadcast = Instant::now() + self.interval;

        loop {
            let received = match timeout_at(next_broadcast, self.socket.recv_from(&mut buf)).await {
                Ok(received) => received,
                Err(_elapsed) => {
                    self.broadcast().await;
                    // Spaced from the send, not from the deadline, so a slow
                    // send cannot bunch the next one up behind it.
                    next_broadcast = Instant::now() + self.interval;
                    continue;
                }
            };
            let (n, from) = match received {
                Ok(datagram) => datagram,
                // A target that is switched off answers our broadcast with an
                // ICMP port-unreachable, which Windows reports here as an
                // error on an unrelated later call. It is not the socket
                // failing, and the bulbs that *are* on still have to be heard.
                Err(e) if e.kind() == ErrorKind::ConnectionReset => continue,
                Err(_) => return,
            };
            let Some(mac) = mac_of(&buf[..n]) else {
                continue;
            };
            // Dedup on MAC, not address: the same bulb answers every broadcast,
            // and DHCP may have moved it since it last did.
            if seen.insert(mac.clone(), from) == Some(from) {
                continue;
            }
            if !self.report(mac, from).await {
                return;
            }
        }
    }

    /// Sends the registration to every target, ignoring those that refuse it.
    async fn broadcast(&self) {
        for target in &self.targets {
            let _ = self.socket.send_to(&self.payload, target).await;
        }
    }

    /// Hands a bulb to the consumer. `false` once nobody is listening.
    async fn report(&self, mac: String, addr: SocketAddr) -> bool {
        let bulb = Discovered {
            addr,
            mac,
            system_config: None,
        };
        if !self.system_config {
            return self.tx.send(bulb).await.is_ok();
        }
        // Off the main loop: a bulb waiting on its follow-up must not hold up
        // the broadcasts, or the bulbs still to answer.
        let tx = self.tx.clone();
        let policy = self.policy.clone();
        tokio::spawn(async move {
            let _ = tx.send(with_system_config(bulb, policy).await).await;
        });
        true
    }
}

/// Asks a bulb what it is. Failure is not fatal — it answered the broadcast, so
/// it is there whether or not it feels like elaborating.
async fn with_system_config(mut bulb: Discovered, policy: RetryPolicy) -> Discovered {
    let request = Request::new("getSystemConfig");
    if let Ok(handle) = Bulb::connect_to(bulb.addr).await {
        bulb.system_config = handle.with_policy(policy).request(&request).await.ok();
    }
    bulb
}

/// The MAC in a reply, if it is a WiZ device's.
///
/// Everything else on the wire is discarded here: an unparseable datagram, some
/// other protocol's service announcement, an error envelope — and our own
/// broadcast, which a socket bound to `0.0.0.0:38899` receives back. A device
/// is a bulb if and only if it names its MAC.
fn mac_of(datagram: &[u8]) -> Option<String> {
    let response: Response = serde_json::from_slice(datagram).ok()?;
    let result = response.result?;
    let mac = result.get("mac")?.as_str()?;
    Some(mac.to_ascii_lowercase())
}

/// The discovery broadcast: `registration`, asking to be *un*-registered.
///
/// `register: false` is deliberate and is what the app itself broadcasts. It
/// means "answer me, but do not start pushing state at me" — push registration
/// is a separate, deliberate act. The consequence to know about is that
/// discovery therefore *clears* an existing push registration for this host,
/// so anything listening for `syncPilot` re-registers after a scan.
fn registration(phone_ip: Option<IpAddr>) -> Result<Vec<u8>> {
    let phone_ip = phone_ip.map_or_else(|| UNKNOWN_PHONE_IP.to_owned(), |ip| ip.to_string());
    let request = Request::with_params(
        "registration",
        &json!({
            // A real MAC belongs to the phone the app runs on. Bulbs accept
            // anything here, and pywizlight has sent this for years.
            "phoneMac": "AAAAAAAAAAAA",
            "register": false,
            "phoneIp": phone_ip,
            "id": "1",
        }),
    )?;
    Ok(serde_json::to_vec(&request)?)
}

/// Which of our addresses the kernel would use to reach `target`.
///
/// Connecting a UDP socket sends nothing; it just asks the routing table. `None`
/// if there is no route at all — on a host with no network, which is a state
/// discovery is about to fail in anyway.
fn local_ip_towards(target: SocketAddr) -> Option<IpAddr> {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.set_broadcast(true).ok()?;
    socket.connect(target).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => None,
        ip => Some(ip),
    }
}
