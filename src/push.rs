//! Subscriptions to unsolicited bulb state updates.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::task::{Context, Poll};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};

use crate::discovery::local_ip_towards;
use crate::error::{Error, Result};
use crate::protocol::{Pilot, Request};

/// The UDP port WiZ bulbs send push updates to.
pub const PUSH_PORT: u16 = 38900;

/// How often an active subscription is renewed at its bulb.
pub const PUSH_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);

/// A typed reason that push updates cannot be used.
///
/// Callers can match this independently from transient request failures and
/// fall back to polling without inspecting an operating-system error string.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PushUnavailable {
    /// Another process already owns the requested listener address.
    #[error("push listener address {addr} is already in use")]
    PortInUse {
        /// The address that could not be bound.
        addr: SocketAddr,
    },
    /// The routing table did not yield a source address for this bulb.
    #[error("could not determine a local source IP towards {target}")]
    SourceIp {
        /// The bulb the source address was needed for.
        target: SocketAddr,
    },
    /// The listener stopped after its socket failed or its last subscriber left.
    #[error("the push listener has stopped")]
    ListenerStopped,
}

/// One unsolicited message from a subscribed bulb.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PushEvent {
    /// A bulb state update.
    SyncPilot {
        /// Where the datagram came from.
        addr: SocketAddr,
        /// The state carried in the push.
        pilot: Pilot,
    },
    /// A bulb announcing itself after boot or reconnect.
    FirstBeat {
        /// Where the datagram came from.
        addr: SocketAddr,
        /// The bulb's lowercase, separator-free MAC.
        mac: String,
    },
}

/// Owns the shared UDP listener and its per-MAC subscription registry.
///
/// Bind one manager and use it for every bulb in the process. The operating
/// system permits only one listener on [`PUSH_PORT`]; if another process owns
/// it, [`bind`](PushManager::bind) returns a typed
/// [`PushUnavailable::PortInUse`] reason. The socket closes when the last
/// [`PushSubscription`] is dropped.
///
/// ```no_run
/// use std::net::{IpAddr, Ipv4Addr};
///
/// use wizlight::{Bulb, PushEvent, PushManager};
///
/// # async fn example() -> Result<(), wizlight::Error> {
/// let bulb = Bulb::connect(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 5))).await?;
/// let pushes = PushManager::bind().await?;
/// let mut subscription = pushes.subscribe("9877d5230f0a", bulb.addr()).await?;
/// if let Some(PushEvent::SyncPilot { pilot, .. }) = subscription.recv().await {
///     println!("on={:?}", pilot.state);
/// }
/// # Ok(())
/// # }
/// ```
pub struct PushManager {
    shared: Arc<Shared>,
    local_addr: SocketAddr,
}

impl PushManager {
    /// Binds the standard all-interface listener on [`PUSH_PORT`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::PushUnavailable`] with
    /// [`PushUnavailable::PortInUse`] when another listener owns the port, or
    /// [`Error::Io`] for another socket failure.
    pub async fn bind() -> Result<Self> {
        Self::bind_to(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            PUSH_PORT,
        ))
        .await
    }

    /// Binds a different listener address.
    ///
    /// This is useful for a test bulb or a UDP forwarder. Real WiZ bulbs always
    /// push to [`PUSH_PORT`].
    ///
    /// # Errors
    ///
    /// As [`bind`](PushManager::bind).
    pub async fn bind_to(addr: SocketAddr) -> Result<Self> {
        let socket = match UdpSocket::bind(addr).await {
            Ok(socket) => Arc::new(socket),
            Err(error) if error.kind() == ErrorKind::AddrInUse => {
                return Err(PushUnavailable::PortInUse { addr }.into());
            }
            Err(error) => return Err(Error::Io(error)),
        };
        let local_addr = socket.local_addr()?;
        let shared = Arc::new(Shared::new(Arc::downgrade(&socket)));
        let listener = tokio::spawn(listen(Arc::clone(&socket), Arc::clone(&shared)));
        let keepalive = tokio::spawn(keepalive(socket, Arc::clone(&shared)));
        shared.set_workers(listener, keepalive);
        Ok(Self { shared, local_addr })
    }

    /// The address the listener is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Re-registers every active subscription immediately.
    ///
    /// WiZ discovery sends `register: false` and may clear an existing push
    /// registration. Call this after a [`Discovery`](crate::Discovery) run that
    /// overlaps active subscriptions rather than waiting for the next periodic
    /// keepalive.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PushUnavailable`] if the listener has stopped, or the
    /// first [`Error::Io`] from a registration send after trying every target.
    pub async fn refresh(&self) -> Result<()> {
        let socket = self
            .shared
            .socket
            .upgrade()
            .ok_or(PushUnavailable::ListenerStopped)?;
        let registrations = self
            .shared
            .registrations()
            .ok_or(PushUnavailable::ListenerStopped)?;
        send_registrations(&socket, registrations).await
    }

    /// Subscribes to one bulb, identified by its MAC.
    ///
    /// Registration is sent before this returns, then refreshed every
    /// [`PUSH_KEEPALIVE_INTERVAL`]. The advertised `phoneIp` is selected from
    /// the route towards this specific target, so bulbs on different interfaces
    /// receive the correct address.
    ///
    /// Only one live subscription per MAC is accepted by a manager. MAC matching
    /// is ASCII case-insensitive.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PushUnavailable`] when no source IP can be selected or
    /// the listener has already stopped, [`Error::InvalidParam`] for a duplicate
    /// MAC, [`Error::Json`] if registration cannot be serialised, or [`Error::Io`]
    /// if its initial send fails.
    pub async fn subscribe(
        &self,
        mac: impl Into<String>,
        target: SocketAddr,
    ) -> Result<PushSubscription> {
        let mac = mac.into().to_ascii_lowercase();
        let phone_ip = local_ip_towards(target).ok_or(PushUnavailable::SourceIp { target })?;
        let payload = registration(phone_ip, true)?;
        let unregister = registration(phone_ip, false)?;
        let socket = self
            .shared
            .socket
            .upgrade()
            .ok_or(PushUnavailable::ListenerStopped)?;
        let (tx, rx) = mpsc::channel(16);
        let id = self
            .shared
            .insert(mac.clone(), target, payload.clone(), tx)?;

        if let Err(error) = socket.send_to(&payload, target).await {
            abort(self.shared.cancel(&mac, id));
            return Err(Error::Io(error));
        }

        Ok(PushSubscription {
            mac,
            id: Some(id),
            target,
            unregister,
            shared: Arc::clone(&self.shared),
            rx,
        })
    }
}

impl Drop for PushManager {
    fn drop(&mut self) {
        self.shared.stop_if_empty();
    }
}

impl std::fmt::Debug for PushManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushManager")
            .field("local_addr", &self.local_addr)
            .field("subscriptions", &self.shared.subscription_count())
            .finish()
    }
}

/// A cancellable stream of push events for one MAC.
///
/// Dropping this handle unregisters it on a best-effort basis and begins
/// stopping the listener when it was the last subscription. Use
/// [`cancel`](PushSubscription::cancel) to wait until the bulb is unregistered
/// and the listener port has been released. Dropping the manager does not
/// invalidate live subscriptions.
pub struct PushSubscription {
    mac: String,
    id: Option<u64>,
    target: SocketAddr,
    unregister: Vec<u8>,
    shared: Arc<Shared>,
    rx: mpsc::Receiver<PushEvent>,
}

impl PushSubscription {
    /// The lowercase MAC this handle receives events for.
    pub fn mac(&self) -> &str {
        &self.mac
    }

    /// Waits for the next valid `syncPilot` or `firstBeat` message.
    ///
    /// Returns `None` if the listener stops. Literal `test` datagrams, malformed
    /// JSON, unknown methods and pushes for other MACs are ignored. The listener
    /// buffers sixteen unread events per subscription and drops later events
    /// rather than let one slow consumer block every bulb.
    pub async fn recv(&mut self) -> Option<PushEvent> {
        self.rx.recv().await
    }

    /// Cancels this subscription and waits for its network cleanup.
    ///
    /// The bulb is sent `register: false`. If this was the manager's last
    /// subscription, this also waits for both worker tasks to stop and release
    /// the listener port.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] if the unregister datagram cannot be handed to the
    /// local socket. Local cancellation still completes in that case.
    pub async fn cancel(mut self) -> Result<()> {
        let socket = self.shared.socket.upgrade();
        let workers = self
            .id
            .take()
            .and_then(|id| self.shared.cancel(&self.mac, id));
        join(workers).await;
        match socket {
            Some(socket) => socket
                .send_to(&self.unregister, self.target)
                .await
                .map(|_| ())
                .map_err(Error::Io),
            None => Ok(()),
        }
    }
}

impl Drop for PushSubscription {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        let socket = self.shared.socket.upgrade();
        let workers = self.shared.cancel(&self.mac, id);
        if let Some(socket) = socket {
            let _ = socket.try_send_to(&self.unregister, self.target);
        }
        abort(workers);
    }
}

impl std::fmt::Debug for PushSubscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PushSubscription")
            .field("mac", &self.mac)
            .finish_non_exhaustive()
    }
}

impl futures_core::Stream for PushSubscription {
    type Item = PushEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

struct Shared {
    socket: Weak<UdpSocket>,
    state: Mutex<State>,
}

impl Shared {
    fn new(socket: Weak<UdpSocket>) -> Self {
        Self {
            socket,
            state: Mutex::new(State::default()),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("push registry mutex poisoned")
    }

    fn set_workers(&self, listener: JoinHandle<()>, keepalive: JoinHandle<()>) {
        let mut state = self.state();
        state.workers = Some(Workers {
            listener,
            keepalive,
        });
    }

    fn insert(
        &self,
        mac: String,
        target: SocketAddr,
        payload: Vec<u8>,
        tx: mpsc::Sender<PushEvent>,
    ) -> Result<u64> {
        let mut state = self.state();
        if state.stopped {
            return Err(PushUnavailable::ListenerStopped.into());
        }
        if state.subscriptions.contains_key(&mac) {
            return Err(Error::InvalidParam {
                message: format!("a push subscription for {mac} already exists"),
            });
        }
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.subscriptions.insert(
            mac,
            SubscriptionEntry {
                id,
                target,
                payload,
                tx,
            },
        );
        Ok(id)
    }

    fn cancel(&self, mac: &str, id: u64) -> Option<Workers> {
        let mut state = self.state();
        if state.subscriptions.get(mac).map(|entry| entry.id) == Some(id) {
            state.subscriptions.remove(mac);
        }
        state.stop_if_empty()
    }

    fn stop_if_empty(&self) {
        let workers = self.state().stop_if_empty();
        abort(workers);
    }

    fn stop(&self) {
        let workers = {
            let mut state = self.state();
            state.stopped = true;
            state.subscriptions.clear();
            state.workers.take()
        };
        abort(workers);
    }

    fn subscription_count(&self) -> usize {
        self.state().subscriptions.len()
    }

    fn registrations(&self) -> Option<Vec<(SocketAddr, Vec<u8>)>> {
        let state = self.state();
        if state.stopped {
            return None;
        }
        Some(
            state
                .subscriptions
                .values()
                .map(|entry| (entry.target, entry.payload.clone()))
                .collect(),
        )
    }

    fn sender(&self, mac: &str) -> Option<mpsc::Sender<PushEvent>> {
        self.state()
            .subscriptions
            .get(mac)
            .map(|entry| entry.tx.clone())
    }
}

#[derive(Default)]
struct State {
    subscriptions: HashMap<String, SubscriptionEntry>,
    workers: Option<Workers>,
    next_id: u64,
    stopped: bool,
}

struct SubscriptionEntry {
    id: u64,
    target: SocketAddr,
    payload: Vec<u8>,
    tx: mpsc::Sender<PushEvent>,
}

struct Workers {
    listener: JoinHandle<()>,
    keepalive: JoinHandle<()>,
}

impl State {
    fn stop_if_empty(&mut self) -> Option<Workers> {
        if self.subscriptions.is_empty() {
            self.stopped = true;
            self.workers.take()
        } else {
            None
        }
    }
}

fn abort(workers: Option<Workers>) {
    if let Some(workers) = workers {
        workers.listener.abort();
        workers.keepalive.abort();
    }
}

async fn join(workers: Option<Workers>) {
    if let Some(workers) = workers {
        workers.listener.abort();
        workers.keepalive.abort();
        let _ = workers.listener.await;
        let _ = workers.keepalive.await;
    }
}

async fn listen(socket: Arc<UdpSocket>, shared: Arc<Shared>) {
    let mut buf = vec![0u8; 4096];
    loop {
        let (n, addr) = match socket.recv_from(&mut buf).await {
            Ok(datagram) => datagram,
            Err(error) if error.kind() == ErrorKind::ConnectionReset => continue,
            Err(_) => {
                shared.stop();
                return;
            }
        };
        let Some((mac, event)) = parse_push(&buf[..n], addr) else {
            continue;
        };
        if let Some(tx) = shared.sender(&mac) {
            let _ = tx.try_send(event);
        }
    }
}

async fn keepalive(socket: Arc<UdpSocket>, shared: Arc<Shared>) {
    let mut next = Instant::now() + PUSH_KEEPALIVE_INTERVAL;
    loop {
        sleep_until(next).await;
        let Some(registrations) = shared.registrations() else {
            return;
        };
        let _ = send_registrations(&socket, registrations).await;
        next += PUSH_KEEPALIVE_INTERVAL;
    }
}

async fn send_registrations(
    socket: &UdpSocket,
    registrations: Vec<(SocketAddr, Vec<u8>)>,
) -> Result<()> {
    let mut first_error = None;
    for (target, payload) in registrations {
        if let Err(error) = socket.send_to(&payload, target).await {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), |error| Err(Error::Io(error)))
}

#[derive(Deserialize)]
struct PushEnvelope {
    method: String,
    #[serde(default)]
    params: Value,
}

fn parse_push(datagram: &[u8], addr: SocketAddr) -> Option<(String, PushEvent)> {
    if datagram == b"test" {
        return None;
    }
    let envelope: PushEnvelope = serde_json::from_slice(datagram).ok()?;
    match envelope.method.as_str() {
        "syncPilot" => {
            let mut pilot: Pilot = serde_json::from_value(envelope.params).ok()?;
            let mac = pilot.mac.as_ref()?.to_ascii_lowercase();
            pilot.mac = Some(mac.clone());
            Some((mac, PushEvent::SyncPilot { addr, pilot }))
        }
        "firstBeat" => {
            let mac = envelope.params.get("mac")?.as_str()?.to_ascii_lowercase();
            Some((mac.clone(), PushEvent::FirstBeat { addr, mac }))
        }
        _ => None,
    }
}

fn registration(phone_ip: IpAddr, register: bool) -> Result<Vec<u8>> {
    let request = Request::with_params(
        "registration",
        &json!({
            "phoneMac": "AAAAAAAAAAAA",
            "register": register,
            "phoneIp": phone_ip,
        }),
    )?;
    Ok(serde_json::to_vec(&request)?)
}
