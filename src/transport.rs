//! The reliable request/response transport.
//!
//! One UDP socket, one exchange at a time, with retries. This is the half of
//! the protocol that waits for an answer; the streaming path that does not is
//! deliberately separate, because the two want opposite things from the
//! network.
//!
//! # Why one at a time
//!
//! A WiZ reply carries the method it answers and nothing else — no request id,
//! nothing echoed back from the params. Two `getPilot` calls in flight on one
//! socket cannot be told apart, so an exchange holds a lock for its whole
//! duration and concurrent callers queue behind it. Bulbs do pipeline happily,
//! but until there is something to correlate on we cannot take advantage of it.

use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep_until, timeout};

use crate::error::{Error, Result};
use crate::protocol::{Request, Response};

/// How hard [`Bulb::request`](crate::Bulb::request) tries before giving up.
///
/// The defaults are measured, not inherited. Over 600 requests to an
/// `ESP25_SHRGB_01` on firmware 1.38.0: round trips of p50 101 ms and 235 ms at
/// the very worst, no loss at all while requests were spaced 20 ms or more
/// apart, and 78 % loss when they were fired back to back. So the bulb is fast
/// and reliable right up until it is flooded, and the policy is built around
/// spacing datagrams rather than waiting a long time for them.
///
/// `pywizlight` uses six datagrams with a 0.75 s → 3 s backoff over a 13 s
/// timeout. Nothing observed on this hardware justifies waiting that long: an
/// unreachable bulb here fails in under two seconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// How many datagrams to send before giving up. Default 3.
    pub attempts: u32,
    /// How long to wait for the reply to each one. Default 500 ms — about
    /// twice the worst round trip ever measured.
    pub attempt_timeout: Duration,
    /// The minimum gap between consecutive datagrams to the same bulb.
    /// Default 20 ms, the spacing at which measured loss reaches zero, giving
    /// roughly 50 requests per second.
    ///
    /// Retries are paced by this too: retrying into a bulb that is already
    /// overwhelmed only makes the loss worse.
    pub min_interval: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 3,
            attempt_timeout: Duration::from_millis(500),
            min_interval: Duration::from_millis(20),
        }
    }
}

/// A socket that speaks the WiZ envelope, with pacing and retries.
pub(crate) struct Transport {
    socket: UdpSocket,
    /// Held for a whole exchange, so replies cannot be stolen by another one.
    exchange: Mutex<()>,
    pacer: Pacer,
}

impl Transport {
    /// Binds an ephemeral port on all interfaces.
    ///
    /// Not loopback: the same socket type is used for broadcast discovery, and
    /// a loopback-bound socket never sees a broadcast.
    pub(crate) async fn bind() -> Result<Self> {
        Ok(Self {
            socket: UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?,
            exchange: Mutex::new(()),
            pacer: Pacer::default(),
        })
    }

    /// The address the socket is bound to.
    pub(crate) fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }

    /// Sends `request` to `addr` and waits for its reply, retrying until the
    /// policy runs out.
    pub(crate) async fn exchange(
        &self,
        addr: SocketAddr,
        request: &Request,
        policy: &RetryPolicy,
    ) -> Result<Response> {
        let payload = serde_json::to_vec(request)?;
        let attempts = policy.attempts.max(1);
        let started = Instant::now();

        let _guard = self.exchange.lock().await;
        self.discard_backlog();

        for _ in 0..attempts {
            self.pacer.pace(policy.min_interval).await;
            match self.socket.send_to(&payload, addr).await {
                Ok(_) => {}
                // Windows reports an earlier datagram's ICMP port-unreachable
                // as an error on the *next* call. That is a bulb that was off,
                // not a broken socket, so it counts as a failed attempt.
                Err(e) if e.kind() == ErrorKind::ConnectionReset => continue,
                Err(e) => return Err(e.into()),
            }
            match timeout(policy.attempt_timeout, self.receive(addr, &request.method)).await {
                Ok(reply) => return reply,
                Err(_elapsed) => continue,
            }
        }

        Err(Error::Timeout {
            method: request.method.clone(),
            addr,
            attempts,
            elapsed: started.elapsed(),
        })
    }

    /// Waits for a reply to `method` from `addr`, ignoring everything else.
    ///
    /// The caller bounds how long this runs; on its own it waits forever.
    async fn receive(&self, addr: SocketAddr, method: &str) -> Result<Response> {
        let mut buf = vec![0u8; 4096];
        loop {
            let (n, from) = match self.socket.recv_from(&mut buf).await {
                Ok(datagram) => datagram,
                Err(e) if e.kind() == ErrorKind::ConnectionReset => continue,
                Err(e) => return Err(e.into()),
            };
            if from != addr {
                continue;
            }
            let response: Response = serde_json::from_slice(&buf[..n])?;
            // A `syncPilot` push arrives on the push socket, not this one, but
            // the bulb sends whatever it likes wherever it was told to; and a
            // reply to a request we already gave up on may still turn up.
            // Either way it is not the answer to this question.
            if response.method.as_deref().is_some_and(|m| m != method) {
                continue;
            }
            return match response.error {
                Some(error) => Err(Error::from_device(method, error)),
                None => Ok(response),
            };
        }
    }

    /// Throws away anything already queued on the socket.
    ///
    /// A reply to a previous exchange that timed out is still in the buffer,
    /// and it would otherwise be handed to this one — the methods can match,
    /// since the same request is often repeated.
    fn discard_backlog(&self) {
        let mut buf = [0u8; 4096];
        while self.socket.try_recv_from(&mut buf).is_ok() {}
    }
}

/// Keeps consecutive datagrams a minimum distance apart.
#[derive(Default)]
struct Pacer {
    /// The earliest the next datagram may leave.
    next: StdMutex<Option<Instant>>,
}

impl Pacer {
    /// Waits until it is this datagram's turn, and claims the slot after it.
    async fn pace(&self, min_interval: Duration) {
        let now = Instant::now();
        let wait_until = {
            let mut next = self.next.lock().expect("pacer mutex poisoned");
            let slot = next.filter(|at| *at > now);
            *next = Some(slot.unwrap_or(now) + min_interval);
            slot
        };
        if let Some(at) = wait_until {
            sleep_until(at).await;
        }
    }
}
