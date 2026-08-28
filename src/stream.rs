//! Rate-limited, fire-and-forget pilot updates.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};

use crate::error::Result;
use crate::protocol::PilotBuilder;
use crate::transport::Transport;

/// The unverified default gap between streamed frames: 50 ms, or 20 Hz.
///
/// This is a placeholder at the upper end of the current 10–20 Hz working
/// hypothesis. It will be replaced after sustained update rates are measured on
/// hardware.
pub const DEFAULT_STREAM_INTERVAL: Duration = Duration::from_millis(50);

/// Rate-limit settings for [`Bulb::stream_with_config`](crate::Bulb::stream_with_config).
///
/// The bucket holds one token, so an idle stream may send one frame immediately
/// but cannot accumulate a burst. A zero interval removes the rate limit while
/// keeping newest-frame coalescing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamConfig {
    /// The minimum gap between stream send attempts.
    pub min_interval: Duration,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            min_interval: DEFAULT_STREAM_INTERVAL,
        }
    }
}

/// A snapshot of what a [`BulbStream`] has done.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamCounters {
    /// Datagrams successfully handed to the local UDP socket.
    pub sent: u64,
    /// Pending frames replaced by a newer update before they could be sent.
    pub coalesced: u64,
    /// Valid frames lost to a local `send_to` I/O failure.
    pub dropped: u64,
}

/// A non-blocking submission handle for real-time pilot updates.
///
/// [`send`](BulbStream::send) validates and serialises synchronously, then
/// replaces the single pending frame. It never waits for a token, the socket, or
/// a bulb acknowledgement. Dropping the handle starts a graceful close;
/// [`shutdown`](BulbStream::shutdown) also waits until the final pending frame's
/// UDP handoff has either succeeded or been counted as dropped.
pub struct BulbStream {
    shared: Arc<Shared>,
    task: Option<JoinHandle<()>>,
}

impl BulbStream {
    pub(crate) fn new(
        addr: SocketAddr,
        transport: Arc<Transport>,
        network_interval: Duration,
        config: StreamConfig,
    ) -> Self {
        let shared = Arc::new(Shared::default());
        let run = Run {
            addr,
            transport,
            network_interval,
            config,
            shared: Arc::clone(&shared),
        };
        Self {
            shared,
            task: Some(tokio::spawn(run.drive())),
        }
    }

    /// Offers a pilot update without waiting for rate-limit capacity or a reply.
    ///
    /// If another frame is still pending, this one replaces it. The caller does
    /// not yield to the worker and no queue can grow behind it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidParam`](crate::Error::InvalidParam) when the
    /// builder is empty or internally conflicting, or
    /// [`Error::Json`](crate::Error::Json) if it cannot be serialised. A
    /// rejected submission is not a frame and does not change the counters.
    pub fn send(&self, pilot: &PilotBuilder) -> Result<()> {
        let request = pilot.set_pilot()?;
        let payload = serde_json::to_vec(&request)?;
        self.shared.replace(payload);
        Ok(())
    }

    /// Returns a coherent snapshot of the stream counters.
    pub fn counters(&self) -> StreamCounters {
        self.shared.state().counters
    }

    /// Closes the stream, flushes its newest pending frame, and returns its counters.
    ///
    /// The final frame still observes [`StreamConfig::min_interval`], so this may
    /// wait for one last token. A plain drop requests the same flush but cannot
    /// wait for it to finish.
    ///
    /// # Panics
    ///
    /// Panics if the internal worker task failed.
    pub async fn shutdown(mut self) -> StreamCounters {
        self.close();
        if let Some(task) = self.task.take() {
            task.await.expect("stream worker failed");
        }
        self.counters()
    }

    fn close(&self) {
        let mut state = self.shared.state();
        state.closed = true;
        drop(state);
        self.shared.changed.notify_one();
    }
}

impl Drop for BulbStream {
    fn drop(&mut self) {
        self.close();
    }
}

impl std::fmt::Debug for BulbStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BulbStream")
            .field("counters", &self.counters())
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Notify,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("stream state mutex poisoned")
    }

    fn replace(&self, payload: Vec<u8>) {
        let mut state = self.state();
        if state.pending.replace(payload).is_some() {
            state.counters.coalesced = state.counters.coalesced.saturating_add(1);
        }
        drop(state);
        self.changed.notify_one();
    }

    fn record_handoff(&self, sent: bool) {
        let mut state = self.state();
        let counter = if sent {
            &mut state.counters.sent
        } else {
            &mut state.counters.dropped
        };
        *counter = counter.saturating_add(1);
    }
}

#[derive(Default)]
struct State {
    pending: Option<Vec<u8>>,
    closed: bool,
    counters: StreamCounters,
}

struct Run {
    addr: SocketAddr,
    transport: Arc<Transport>,
    network_interval: Duration,
    config: StreamConfig,
    shared: Arc<Shared>,
}

impl Run {
    async fn drive(self) {
        let mut next_token = Instant::now();
        loop {
            let (pending, closed) = {
                let state = self.shared.state();
                (state.pending.is_some(), state.closed)
            };
            if !pending {
                if closed {
                    return;
                }
                self.shared.changed.notified().await;
                continue;
            }

            let now = Instant::now();
            if next_token > now {
                sleep_until(next_token).await;
            }
            let slot = self.transport.stream_slot(self.network_interval).await;

            let payload = self
                .shared
                .state()
                .pending
                .take()
                .expect("pending stream frame disappeared");
            let sent = slot.send(self.addr, &payload).await.is_ok();
            self.shared.record_handoff(sent);
            next_token = Instant::now() + self.config.min_interval;
        }
    }
}
