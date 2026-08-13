//! A bare UDP client for the tests.
//!
//! The crate has no transport of its own yet, and even once it does the harness
//! tests want to drive the wire directly rather than through it.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use serde_json::Value;
use tokio::net::UdpSocket;
use tokio::time::timeout;

/// The default patience for a reply. Generous, because CI is slow.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// A UDP socket that speaks JSON.
pub struct Client {
    socket: UdpSocket,
}

impl Client {
    /// Binds an ephemeral port.
    pub async fn new() -> Self {
        Self {
            socket: UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind client"),
        }
    }

    /// Sends a request and waits for the reply. Panics if none arrives.
    pub async fn ask(&self, addr: SocketAddr, request: Value) -> Value {
        self.try_ask(addr, request.clone(), REPLY_TIMEOUT)
            .await
            .unwrap_or_else(|| panic!("no reply to {request}"))
    }

    /// Sends a request and waits for a reply, giving up after `patience`.
    pub async fn try_ask(
        &self,
        addr: SocketAddr,
        request: Value,
        patience: Duration,
    ) -> Option<Value> {
        self.send(addr, request).await;
        self.recv(patience).await
    }

    /// Fires a request off without waiting for anything.
    pub async fn send(&self, addr: SocketAddr, request: Value) {
        self.socket
            .send_to(request.to_string().as_bytes(), addr)
            .await
            .expect("send request");
    }

    /// Waits for one datagram and parses it, or returns `None` on timeout.
    /// Datagrams that are not JSON come back as a string.
    ///
    /// A socket error counts as "nothing arrived": Windows reports
    /// `WSAECONNRESET` on an unconnected UDP socket when an earlier datagram
    /// drew an ICMP port-unreachable, which is exactly what talking to a
    /// stopped bulb does.
    pub async fn recv(&self, patience: Duration) -> Option<Value> {
        let mut buf = vec![0u8; 4096];
        let (n, _) = timeout(patience, self.socket.recv_from(&mut buf))
            .await
            .ok()?
            .ok()?;
        let raw = String::from_utf8_lossy(&buf[..n]).into_owned();
        Some(serde_json::from_str(&raw).unwrap_or(Value::String(raw)))
    }
}

/// Stands in for a client's `syncPilot` listener, on an ephemeral port so tests
/// never contend for the real 38900.
pub struct PushListener {
    socket: UdpSocket,
}

impl PushListener {
    /// Binds an ephemeral port to receive pushes on.
    pub async fn bind() -> Self {
        Self {
            socket: UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind push listener"),
        }
    }

    /// The port to point a bulb's pushes at.
    pub fn port(&self) -> u16 {
        self.socket.local_addr().expect("local_addr").port()
    }

    /// Waits for the next push, or returns `None` on timeout.
    pub async fn next(&self, patience: Duration) -> Option<Value> {
        let mut buf = vec![0u8; 4096];
        let (n, _) = timeout(patience, self.socket.recv_from(&mut buf))
            .await
            .ok()?
            .ok()?;
        serde_json::from_slice(&buf[..n]).ok()
    }
}
