//! `wizlight bench` — rate, latency and interpolation diagnostics.
//!
//! Drives a target bulb through three deterministic phases so both the UDP
//! socket behaviour and the physical light output (filmed with a high-frame-rate
//! camera at ≥120 fps) can be measured together:
//!
//! 1. **Sustained `setPilot` rate sweep** from `--min-rate` to `--max-rate`
//!    across `--steps` steps, holding each step for `--hold`. Every datagram
//!    advances a three-state primary cycle (`R → G → B` on colour bulbs,
//!    `20% ↔ 100%` dimming on white-only bulbs, `on ↔ off` on sockets) so that
//!    dropped updates and stutters are directly countable in camera frames as
//!    well as on the socket.
//! 2. **Packet-to-photon latency probe** (`black → white` step): turns the bulb
//!    off, waits for it to settle dark, then emits a timestamped `LATENCY_STEP`
//!    marker on stderr at the exact instant the full-white `setPilot` datagram
//!    leaves the socket.
//! 3. **Interpolation probe**: emits a settled `red → blue` abrupt step
//!    (`INTERPOLATION_STEP` on stderr) followed by a rapid `red ↔ blue`
//!    alternation at `--max-rate` (`INTERPOLATION_ALT` on stderr) to reveal
//!    whether the firmware snaps or cross-fades internally.
//!
//! Because this visibly flashes the bulb, a warning is written to stderr before
//! the first write, and the pre-bench [`Pilot`] state is restored on exit —
//! including when interrupted with Ctrl-C.

use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use clap::{Args, ValueEnum};
use serde::Serialize;
use serde_json::json;
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{Instant, sleep_until, timeout};

use super::pilot::{pulse_delta, pulse_duration_ms};
use super::{Cli, Outcome, Report, Target, collect, seconds, target};
use crate::protocol::{
    BulbType, Channel, Dimming, Kelvin, Pilot, PilotBuilder, PulseDelta, PulseParams, Ratio,
    Request, Response, SceneId, Speed, Success,
};
use crate::{Bulb, RetryPolicy};

/// Which payload family `wizlight bench` drives during the rate and step probes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchMode {
    /// Choose automatically from the bulb's capabilities (`rgb` on colour
    /// bulbs, `dimming` on white-only bulbs, power toggles on sockets).
    #[default]
    Auto,
    /// Drive full RGB primary changes (`R → G → B` cycle, `red → blue` step)
    /// via `setPilot`.
    Rgb,
    /// Drive brightness-only changes (`20% ↔ 100%` cycle, `10% → 100%` step)
    /// via `setPilot`.
    Dimming,
    /// Drive brightness-only changes (`20% ↔ 100%` cycle, `10% → 100%` step)
    /// via `setState` instead of `setPilot`.
    SetState,
}

impl BenchMode {
    /// Wire method name used for the rate, latency, and interpolation probes.
    #[must_use]
    pub const fn method(self) -> &'static str {
        match self {
            Self::Auto | Self::Rgb | Self::Dimming => "setPilot",
            Self::SetState => "setState",
        }
    }
}

/// Arguments for `wizlight bench`.
#[derive(Args, Debug, Clone, PartialEq)]
pub struct Bench {
    /// Which bulbs to benchmark.
    #[command(flatten)]
    pub target: Target,

    /// Lowest `setPilot` rate to test, in Hz.
    #[arg(
        long,
        default_value = "1",
        value_name = "HZ",
        value_parser = positive_hz
    )]
    pub min_rate: f64,

    /// Highest `setPilot` rate to test, in Hz.
    #[arg(
        long,
        default_value = "50",
        value_name = "HZ",
        value_parser = positive_hz
    )]
    pub max_rate: f64,

    /// Number of rate steps between `--min-rate` and `--max-rate`.
    #[arg(
        long,
        default_value = "8",
        value_name = "COUNT",
        value_parser = non_zero_usize
    )]
    pub steps: usize,

    /// Explicit comma-separated rates in Hz (e.g. `1,2,3,4,5,6,8,10`),
    /// overriding `--min-rate`, `--max-rate` and `--steps`.
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "HZ,...",
        value_parser = positive_hz,
        conflicts_with_all = ["min_rate", "max_rate", "steps"]
    )]
    pub rates: Vec<f64>,

    /// How long to hold each rate step, in seconds.
    #[arg(
        long,
        default_value = "2",
        value_name = "SECONDS",
        value_parser = positive_duration
    )]
    pub hold: Duration,

    /// Payload mode for the rate and interpolation sweeps.
    #[arg(long, value_enum, default_value_t = BenchMode::Auto, value_name = "MODE")]
    pub mode: BenchMode,

    /// Run a `pulse` duration sweep across these millisecond durations
    /// (e.g. `50,100,150,200,300,500`).
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "MS,...",
        value_parser = pulse_duration_ms
    )]
    pub pulse_durations: Vec<u32>,

    /// Brightness delta used for `--pulse-durations` (`-100..=-1` or `1..=100`).
    #[arg(
        long,
        default_value = "-80",
        allow_hyphen_values = true,
        value_name = "PERCENT",
        value_parser = pulse_delta
    )]
    pub pulse_delta: PulseDelta,
}

impl Eq for Bench {}

impl Bench {
    /// Rejects `--min-rate` above `--max-rate` as a clap usage error.
    ///
    /// # Errors
    ///
    /// Returns a [`clap::Error`] with [`clap::error::ErrorKind::ArgumentConflict`]
    /// when `min_rate > max_rate`.
    pub fn validate(&self, command: &mut clap::Command) -> Result<(), clap::Error> {
        if self.rates.is_empty() && self.min_rate > self.max_rate {
            return Err(command.error(
                clap::error::ErrorKind::ArgumentConflict,
                format!(
                    "--min-rate ({}) must not exceed --max-rate ({})",
                    self.min_rate, self.max_rate
                ),
            ));
        }
        Ok(())
    }

    /// Target rate list (in Hz) for this benchmark run.
    #[must_use]
    pub fn configured_rates(&self) -> Vec<f64> {
        if self.rates.is_empty() {
            rate_steps(self.min_rate, self.max_rate, self.steps)
        } else {
            self.rates.iter().copied().map(round2).collect()
        }
    }

    /// Rate used for the rapid alternation probe.
    #[must_use]
    pub fn alternation_rate(&self) -> f64 {
        self.rates
            .iter()
            .copied()
            .fold(None, |acc: Option<f64>, r| {
                Some(acc.map_or(r, |m| m.max(r)))
            })
            .unwrap_or(self.max_rate)
    }
}

/// Round-trip time statistics in milliseconds for one benchmark phase.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct RttStats {
    /// Minimum observed RTT, in milliseconds.
    pub min: f64,
    /// Median (p50) observed RTT, in milliseconds.
    pub median: f64,
    /// 95th-percentile observed RTT, in milliseconds.
    pub p95: f64,
    /// Maximum observed RTT, in milliseconds.
    pub max: f64,
    /// Mean absolute difference between consecutive RTTs, in milliseconds.
    pub jitter: f64,
}

impl RttStats {
    /// Computes RTT summary statistics from chronological RTT samples.
    #[must_use]
    pub fn from_samples(samples: &[Duration]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        let ms: Vec<f64> = samples.iter().map(|d| d.as_secs_f64() * 1_000.0).collect();
        let jitter = if ms.len() < 2 {
            0.0
        } else {
            let sum: f64 = ms.windows(2).map(|w| (w[1] - w[0]).abs()).sum();
            round2(sum / (ms.len() - 1) as f64)
        };

        let mut sorted = ms;
        sorted.sort_by(f64::total_cmp);
        let n = sorted.len();
        let min = round2(sorted[0]);
        let max = round2(sorted[n - 1]);
        let median = if n % 2 == 1 {
            round2(sorted[n / 2])
        } else {
            round2((sorted[n / 2 - 1] + sorted[n / 2]) / 2.0)
        };
        let p95_idx = ((n as f64 * 0.95).ceil() as usize)
            .saturating_sub(1)
            .min(n - 1);
        let p95 = round2(sorted[p95_idx]);

        Some(Self {
            min,
            median,
            p95,
            max,
            jitter,
        })
    }
}

/// Summary of one rate step in the `setPilot` sweep.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct StepSummary {
    /// Target `setPilot` send rate, in Hz.
    pub rate_hz: f64,
    /// Target spacing between consecutive packets, in milliseconds.
    pub interval_ms: f64,
    /// Datagrams handed to the UDP socket.
    pub sent: usize,
    /// Matching `setPilot` acknowledgements received.
    pub responses: usize,
    /// Datagrams for which no acknowledgement arrived (`sent - responses`).
    pub inferred_drops: usize,
    /// Percentage of sent datagrams without an acknowledgement.
    pub loss_pct: f64,
    /// Round-trip time statistics, or `None` when no responses arrived.
    pub rtt_ms: Option<RttStats>,
}

/// Summary of one `pulse` duration probe in `wizlight bench --pulse-durations`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PulseStepSummary {
    /// Signed brightness delta sent in `pulse`.
    pub delta: i32,
    /// Half-cycle `duration` parameter sent in `pulse`, in milliseconds.
    pub duration_ms: u32,
    /// Datagrams handed to the UDP socket (`1`).
    pub sent: usize,
    /// Matching `pulse` acknowledgements received (`0` or `1`).
    pub responses: usize,
    /// Datagrams without an acknowledgement (`sent - responses`).
    pub inferred_drops: usize,
    /// Socket round-trip time in milliseconds, when acknowledged.
    pub socket_rtt_ms: Option<f64>,
}

/// Computes the target rates (in Hz) across the configured sweep.
#[must_use]
pub fn rate_steps(min_rate: f64, max_rate: f64, steps: usize) -> Vec<f64> {
    if steps <= 1 || (max_rate - min_rate).abs() < f64::EPSILON {
        return vec![round2(min_rate)];
    }
    (0..steps)
        .map(|i| {
            let fraction = i as f64 / (steps - 1) as f64;
            round2(min_rate + (max_rate - min_rate) * fraction)
        })
        .collect()
}

/// Runs `wizlight bench` using stderr for warnings/markers and Ctrl-C for shutdown.
pub(super) async fn run(cli: &Cli, bench: &Bench) -> anyhow::Result<Outcome> {
    if bench.rates.is_empty() && bench.min_rate > bench.max_rate {
        anyhow::bail!(
            "--min-rate ({}) must not exceed --max-rate ({})",
            bench.min_rate,
            bench.max_rate
        );
    }
    let stderr = Arc::new(Mutex::new(io::stderr()));
    run_with(cli, bench, stderr, tokio::signal::ctrl_c()).await
}

async fn run_with<W, F>(
    cli: &Cli,
    bench: &Bench,
    stderr: Arc<Mutex<W>>,
    shutdown: F,
) -> anyhow::Result<Outcome>
where
    W: io::Write + Send + 'static,
    F: Future<Output = io::Result<()>>,
{
    let policy = cli.policy();
    let bulbs = resolve(cli, &bench.target).await?;
    let (stop_tx, stop_rx) = watch::channel(false);

    let mut tasks = JoinSet::new();
    for resolved in bulbs {
        let label = resolved.label();
        let policy = policy.clone();
        let bench = bench.clone();
        let stderr = Arc::clone(&stderr);
        let stop_rx = stop_rx.clone();
        tasks.spawn(async move {
            let result = match resolved.connect(&policy).await {
                Ok(bulb) => bench_one(&bulb, &label, &bench, &stderr, stop_rx).await,
                Err(err) => Err(err.into()),
            };
            (label, result)
        });
    }

    tokio::pin!(shutdown);
    let mut results = Vec::new();
    let mut interrupted = false;

    loop {
        tokio::select! {
            _ = &mut shutdown, if !interrupted => {
                interrupted = true;
                let _ = stop_tx.send(true);
            }
            joined = tasks.join_next() => {
                let Some(joined) = joined else {
                    break;
                };
                results.push(joined?);
            }
        }
    }

    if interrupted {
        anyhow::bail!("benchmark interrupted; pre-bench state restored");
    }

    if !bench.target.all {
        let (_, result) = results.into_iter().next().expect("single target resolved");
        let report = result?;
        return Ok(Outcome::new(report.json, report.human));
    }

    results.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(collect(results))
}

async fn resolve(cli: &Cli, selection: &Target) -> anyhow::Result<Vec<target::Resolved>> {
    if let Some(spec) = &selection.target {
        let bulb = match spec.address() {
            Some(addr) => target::Resolved { addr, mac: None },
            None => {
                let discovery = cli.discovery(false)?;
                target::resolve(spec, &discovery, cli.wait).await?
            }
        };
        return Ok(vec![bulb]);
    }

    let discovery = cli.discovery(false)?;
    target::resolve_all(&discovery, cli.wait).await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VisualMode {
    Colour,
    Dimming,
    Power,
}

impl VisualMode {
    fn from_bench_and_bulb(bench_mode: BenchMode, bulb_type: Option<&BulbType>) -> Self {
        match bench_mode {
            BenchMode::Rgb => Self::Colour,
            BenchMode::Dimming | BenchMode::SetState => Self::Dimming,
            BenchMode::Auto => Self::from_bulb_type(bulb_type),
        }
    }

    fn from_bulb_type(bulb_type: Option<&BulbType>) -> Self {
        let Some(bulb_type) = bulb_type else {
            return Self::Colour;
        };
        if bulb_type.features.color {
            Self::Colour
        } else if bulb_type.features.brightness {
            Self::Dimming
        } else {
            Self::Power
        }
    }

    fn cycle_frame(self, index: usize) -> PilotBuilder {
        match self {
            Self::Colour => {
                let (r, g, b) = match index % 3 {
                    0 => (255, 0, 0),
                    1 => (0, 255, 0),
                    _ => (0, 0, 255),
                };
                PilotBuilder::new()
                    .state(true)
                    .rgb(Channel::new(r), Channel::new(g), Channel::new(b))
                    .dimming(Dimming::new(100).expect("100 is in range"))
            }
            Self::Dimming => {
                let level = if index % 2 == 0 { 20 } else { 100 };
                PilotBuilder::new()
                    .state(true)
                    .dimming(Dimming::new(level).expect("dimming in range"))
            }
            Self::Power => PilotBuilder::new().state(index % 2 == 0),
        }
    }

    fn white_on(self) -> PilotBuilder {
        match self {
            Self::Colour => PilotBuilder::new()
                .state(true)
                .rgbww(
                    Channel::new(255),
                    Channel::new(255),
                    Channel::new(255),
                    Channel::new(255),
                    Channel::new(255),
                )
                .dimming(Dimming::new(100).expect("100 is in range")),
            Self::Dimming => PilotBuilder::new()
                .state(true)
                .dimming(Dimming::new(100).expect("100 is in range")),
            Self::Power => PilotBuilder::new().state(true),
        }
    }

    fn interp_pair(self) -> (PilotBuilder, PilotBuilder, &'static str, &'static str) {
        match self {
            Self::Colour => (
                PilotBuilder::new()
                    .state(true)
                    .rgb(Channel::new(255), Channel::new(0), Channel::new(0))
                    .dimming(Dimming::new(100).expect("100 is in range")),
                PilotBuilder::new()
                    .state(true)
                    .rgb(Channel::new(0), Channel::new(0), Channel::new(255))
                    .dimming(Dimming::new(100).expect("100 is in range")),
                "red_to_blue_step",
                "red_blue_alternation",
            ),
            Self::Dimming => (
                PilotBuilder::new()
                    .state(true)
                    .dimming(Dimming::new(10).expect("10 is in range")),
                PilotBuilder::new()
                    .state(true)
                    .dimming(Dimming::new(100).expect("100 is in range")),
                "dim_10_to_100_step",
                "dim_10_100_alternation",
            ),
            Self::Power => (
                PilotBuilder::new().state(false),
                PilotBuilder::new().state(true),
                "off_to_on_step",
                "off_on_alternation",
            ),
        }
    }
}

async fn bench_one<W: io::Write>(
    bulb: &Bulb,
    label: &str,
    bench: &Bench,
    stderr: &Arc<Mutex<W>>,
    mut stop: watch::Receiver<bool>,
) -> anyhow::Result<Report> {
    let initial = bulb.get_pilot().await?;
    let bulb_type = bulb.bulb_type().await.ok();
    let mode = VisualMode::from_bench_and_bulb(bench.mode, bulb_type.as_ref());

    note(
        stderr,
        &format!(
            "warning: benchmarking will visibly flash {label}; pre-bench state will be restored on exit (Ctrl-C to abort)"
        ),
    );

    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .await
        .context("could not bind benchmark UDP socket")?;
    let addr = bulb.addr();
    let drain_timeout = bulb.policy().attempt_timeout;
    let settle = bench.hold.min(Duration::from_millis(250));

    let outcome = run_phases(
        &socket,
        addr,
        label,
        bench,
        mode,
        drain_timeout,
        settle,
        stderr,
        &mut stop,
    )
    .await;

    let restored = restore(bulb, &initial).await.is_ok();
    let (rate_sweep, latency, interpolation, pulse_sweep) = outcome?;

    Ok(build_report(
        &initial,
        bulb_type.as_ref(),
        bench,
        &rate_sweep,
        &latency,
        &interpolation,
        &pulse_sweep,
        restored,
    ))
}

struct LatencyResult {
    pattern: &'static str,
    trials: usize,
    sent: usize,
    responses: usize,
    inferred_drops: usize,
    socket_rtt_ms: Option<RttStats>,
}

struct InterpolationResult {
    step_pattern: &'static str,
    step_sent: usize,
    step_responses: usize,
    step_drops: usize,
    step_rtt_ms: Option<RttStats>,
    alt_pattern: &'static str,
    alt_rate_hz: f64,
    alt_sent: usize,
    alt_responses: usize,
    alt_drops: usize,
    alt_loss_pct: f64,
    alt_rtt_ms: Option<RttStats>,
}

#[allow(clippy::too_many_arguments)]
async fn run_phases<W: io::Write>(
    socket: &UdpSocket,
    addr: SocketAddr,
    label: &str,
    bench: &Bench,
    mode: VisualMode,
    drain_timeout: Duration,
    settle: Duration,
    stderr: &Arc<Mutex<W>>,
    stop: &mut watch::Receiver<bool>,
) -> anyhow::Result<(
    Vec<StepSummary>,
    LatencyResult,
    InterpolationResult,
    Vec<PulseStepSummary>,
)> {
    let write_method = bench.mode.method();
    if mode == VisualMode::Dimming {
        // Ensure a steady white baseline before running dimming-only sweeps.
        let baseline = encode_pilot(
            write_method,
            &PilotBuilder::new()
                .state(true)
                .temp(Kelvin::new(4000).expect("4000 K in range"))
                .dimming(Dimming::new(100).expect("100 in range")),
        )?;
        let _ =
            send_single_probe(socket, addr, write_method, &baseline, drain_timeout, stop).await?;
        if pause(settle, stop).await {
            anyhow::bail!("benchmark interrupted");
        }
    }

    let rates = bench.configured_rates();
    let mut summaries = Vec::with_capacity(rates.len());

    for (idx, rate_hz) in rates.iter().copied().enumerate() {
        if is_stopped(stop) {
            anyhow::bail!("benchmark interrupted");
        }
        let packet_count = ((bench.hold.as_secs_f64() * rate_hz).round() as usize).max(1);
        note(
            stderr,
            &format!(
                "[{}] {label}  RATE_STEP {}/{}  {:.2} Hz ({packet_count} packets)",
                utc_timestamp(),
                idx + 1,
                rates.len(),
                rate_hz
            ),
        );
        let payloads: Vec<Vec<u8>> = (0..packet_count)
            .map(|i| encode_pilot(write_method, &mode.cycle_frame(i)))
            .collect::<Result<_, _>>()?;
        let step = run_paced_burst(
            socket,
            addr,
            write_method,
            rate_hz,
            &payloads,
            drain_timeout,
            stop,
        )
        .await?;
        summaries.push(step);

        if pause(settle, stop).await {
            anyhow::bail!("benchmark interrupted");
        }
    }

    // Phase 2: Packet-to-photon latency (black -> white step)
    let trials = if bench.hold >= Duration::from_millis(500) {
        3
    } else {
        1
    };
    let off_payload = encode_pilot(write_method, &PilotBuilder::new().state(false))?;
    let white_payload = encode_pilot(write_method, &mode.white_on())?;
    let mut latency_rtts = Vec::with_capacity(trials);
    let mut latency_sent = 0usize;

    for trial in 0..trials {
        if is_stopped(stop) {
            anyhow::bail!("benchmark interrupted");
        }
        let _ = send_single_probe(
            socket,
            addr,
            write_method,
            &off_payload,
            drain_timeout,
            stop,
        )
        .await?;
        if pause(settle, stop).await {
            anyhow::bail!("benchmark interrupted");
        }
        note(
            stderr,
            &format!(
                "[{}] {label}  LATENCY_STEP {}/{}  black -> white",
                utc_timestamp(),
                trial + 1,
                trials
            ),
        );
        latency_sent += 1;
        if let Some(rtt) = send_single_probe(
            socket,
            addr,
            write_method,
            &white_payload,
            drain_timeout,
            stop,
        )
        .await?
        {
            latency_rtts.push(rtt);
        }
        if pause(settle, stop).await {
            anyhow::bail!("benchmark interrupted");
        }
    }

    let latency = LatencyResult {
        pattern: "black_to_white_step",
        trials,
        sent: latency_sent,
        responses: latency_rtts.len(),
        inferred_drops: latency_sent.saturating_sub(latency_rtts.len()),
        socket_rtt_ms: RttStats::from_samples(&latency_rtts),
    };

    // Phase 3: Interpolation probe (abrupt step + rapid alternation)
    let (from_builder, to_builder, step_pattern, alt_pattern) = mode.interp_pair();
    let from_payload = encode_pilot(write_method, &from_builder)?;
    let to_payload = encode_pilot(write_method, &to_builder)?;

    let _ = send_single_probe(
        socket,
        addr,
        write_method,
        &from_payload,
        drain_timeout,
        stop,
    )
    .await?;
    if pause(settle, stop).await {
        anyhow::bail!("benchmark interrupted");
    }

    note(
        stderr,
        &format!(
            "[{}] {label}  INTERPOLATION_STEP  {step_pattern}",
            utc_timestamp()
        ),
    );
    let step_rtt =
        send_single_probe(socket, addr, write_method, &to_payload, drain_timeout, stop).await?;
    let step_samples: Vec<Duration> = step_rtt.into_iter().collect();
    if pause(settle, stop).await {
        anyhow::bail!("benchmark interrupted");
    }

    let alt_rate = bench.alternation_rate();
    let alt_hold = bench.hold.min(Duration::from_secs(1));
    let alt_count = ((alt_hold.as_secs_f64() * alt_rate).round() as usize).max(4);
    note(
        stderr,
        &format!(
            "[{}] {label}  INTERPOLATION_ALT  {alt_pattern} @ {:.2} Hz ({alt_count} packets)",
            utc_timestamp(),
            alt_rate
        ),
    );
    let alt_payloads: Vec<Vec<u8>> = (0..alt_count)
        .map(|i| {
            if i % 2 == 0 {
                from_payload.clone()
            } else {
                to_payload.clone()
            }
        })
        .collect();
    let alt_summary = run_paced_burst(
        socket,
        addr,
        write_method,
        alt_rate,
        &alt_payloads,
        drain_timeout,
        stop,
    )
    .await?;

    let interpolation = InterpolationResult {
        step_pattern,
        step_sent: 1,
        step_responses: step_samples.len(),
        step_drops: 1usize.saturating_sub(step_samples.len()),
        step_rtt_ms: RttStats::from_samples(&step_samples),
        alt_pattern,
        alt_rate_hz: alt_summary.rate_hz,
        alt_sent: alt_summary.sent,
        alt_responses: alt_summary.responses,
        alt_drops: alt_summary.inferred_drops,
        alt_loss_pct: alt_summary.loss_pct,
        alt_rtt_ms: alt_summary.rtt_ms,
    };

    // Optional Phase 4: Pulse duration sweep (`--pulse-durations`)
    let mut pulse_summaries = Vec::with_capacity(bench.pulse_durations.len());
    if !bench.pulse_durations.is_empty() {
        let _ = send_single_probe(
            socket,
            addr,
            write_method,
            &white_payload,
            drain_timeout,
            stop,
        )
        .await?;
        if pause(settle, stop).await {
            anyhow::bail!("benchmark interrupted");
        }

        let delta = bench.pulse_delta.get();
        let total_pulses = bench.pulse_durations.len();
        for (idx, duration_ms) in bench.pulse_durations.iter().copied().enumerate() {
            if is_stopped(stop) {
                anyhow::bail!("benchmark interrupted");
            }
            let pulse_req = PulseParams::new(delta, duration_ms)?.request()?;
            let pulse_payload = serde_json::to_vec(&pulse_req)?;
            note(
                stderr,
                &format!(
                    "[{}] {label}  PULSE_STEP {}/{total_pulses}  delta={delta} duration={duration_ms}ms",
                    utc_timestamp(),
                    idx + 1,
                ),
            );
            let rtt = send_single_probe(socket, addr, "pulse", &pulse_payload, drain_timeout, stop)
                .await?;
            pulse_summaries.push(PulseStepSummary {
                delta,
                duration_ms,
                sent: 1,
                responses: usize::from(rtt.is_some()),
                inferred_drops: usize::from(rtt.is_none()),
                socket_rtt_ms: rtt.map(|d| round2(d.as_secs_f64() * 1_000.0)),
            });

            // Wait for the pulse (`2 * duration_ms + settle_margin`) to finish and settle
            // when running at normal hold times, or `settle` in fast unit tests.
            let pulse_wait = if bench.hold >= Duration::from_millis(500) {
                Duration::from_millis(u64::from(duration_ms).saturating_mul(2) + 500)
            } else {
                settle
            };
            if pause(pulse_wait, stop).await {
                anyhow::bail!("benchmark interrupted");
            }
        }
    }

    Ok((summaries, latency, interpolation, pulse_summaries))
}

async fn run_paced_burst(
    socket: &UdpSocket,
    addr: SocketAddr,
    expected_method: &str,
    rate_hz: f64,
    payloads: &[Vec<u8>],
    drain_timeout: Duration,
    stop: &mut watch::Receiver<bool>,
) -> anyhow::Result<StepSummary> {
    drain_socket(socket);

    let interval = Duration::from_secs_f64(1.0 / rate_hz);
    let total = payloads.len();
    let mut sent_times: Vec<Instant> = Vec::with_capacity(total);
    let mut matched_idx = 0usize;
    let mut rtts: Vec<Duration> = Vec::with_capacity(total);
    let mut buf = [0u8; 4096];

    let start = Instant::now();
    let mut next_send_idx = 0usize;

    loop {
        if is_stopped(stop) {
            anyhow::bail!("benchmark interrupted");
        }

        let all_sent = next_send_idx >= total;
        let all_acked = matched_idx >= total;
        if all_sent && all_acked {
            // Finish holding the final interval so the step lasts its full duration.
            let step_end = start + interval.mul_f64(total as f64);
            if Instant::now() < step_end {
                let _ = pause(step_end.duration_since(Instant::now()), stop).await;
            }
            break;
        }

        let next_deadline = if !all_sent {
            start + interval.mul_f64(next_send_idx as f64)
        } else {
            let last_sent = sent_times.last().copied().unwrap_or(start);
            last_sent + drain_timeout
        };

        let now = Instant::now();
        if !all_sent && now >= next_deadline {
            socket.send_to(&payloads[next_send_idx], addr).await?;
            sent_times.push(Instant::now());
            next_send_idx += 1;
            continue;
        }
        if all_sent && now >= next_deadline {
            break;
        }

        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    anyhow::bail!("benchmark interrupted");
                }
            }
            _ = sleep_until(next_deadline) => {
                if !all_sent {
                    socket.send_to(&payloads[next_send_idx], addr).await?;
                    sent_times.push(Instant::now());
                    next_send_idx += 1;
                } else {
                    break;
                }
            }
            recv = socket.recv_from(&mut buf) => {
                if let Ok((n, from)) = recv {
                    if from == addr && is_write_ack(&buf[..n], expected_method) && matched_idx < sent_times.len() {
                        let now = Instant::now();
                        // Expire any sent packet older than drain_timeout before matching FIFO.
                        while matched_idx + 1 < sent_times.len()
                            && now.saturating_duration_since(sent_times[matched_idx]) > drain_timeout
                        {
                            matched_idx += 1;
                        }
                        rtts.push(now.saturating_duration_since(sent_times[matched_idx]));
                        matched_idx += 1;
                    }
                }
            }
        }
    }

    let sent = sent_times.len();
    let responses = rtts.len();
    let inferred_drops = sent.saturating_sub(responses);
    let loss_pct = if sent == 0 {
        0.0
    } else {
        round2((inferred_drops as f64 / sent as f64) * 100.0)
    };

    Ok(StepSummary {
        rate_hz: round2(rate_hz),
        interval_ms: round2(interval.as_secs_f64() * 1_000.0),
        sent,
        responses,
        inferred_drops,
        loss_pct,
        rtt_ms: RttStats::from_samples(&rtts),
    })
}

async fn send_single_probe(
    socket: &UdpSocket,
    addr: SocketAddr,
    expected_method: &str,
    payload: &[u8],
    drain_timeout: Duration,
    stop: &mut watch::Receiver<bool>,
) -> anyhow::Result<Option<Duration>> {
    drain_socket(socket);
    let sent_at = Instant::now();
    socket.send_to(payload, addr).await?;

    let deadline = sent_at + drain_timeout;
    let mut buf = [0u8; 4096];
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    anyhow::bail!("benchmark interrupted");
                }
            }
            res = timeout(deadline.duration_since(now), socket.recv_from(&mut buf)) => {
                match res {
                    Ok(Ok((n, from))) if from == addr && is_write_ack(&buf[..n], expected_method) => {
                        return Ok(Some(sent_at.elapsed()));
                    }
                    Ok(Ok(_)) => continue,
                    Ok(Err(_)) | Err(_) => return Ok(None),
                }
            }
        }
    }
}

fn is_write_ack(bytes: &[u8], expected_method: &str) -> bool {
    let Ok(response) = serde_json::from_slice::<Response>(bytes) else {
        return false;
    };
    if response.method.as_deref() != Some(expected_method) || response.error.is_some() {
        return false;
    }
    response
        .parse_result::<Success>()
        .is_ok_and(|ack| ack.success)
}

fn drain_socket(socket: &UdpSocket) {
    let mut buf = [0u8; 4096];
    while socket.try_recv_from(&mut buf).is_ok() {}
}

fn encode_pilot(method: &str, builder: &PilotBuilder) -> anyhow::Result<Vec<u8>> {
    let request: Request = match method {
        "setState" => builder.set_state()?,
        _ => builder.set_pilot()?,
    };
    Ok(serde_json::to_vec(&request)?)
}

/// Restores a bulb to the [`Pilot`] snapshot taken before the benchmark ran.
///
/// Measured on `ESP25_SHRGB_01` fw 1.38.0: any write carrying colour,
/// temperature or a scene switches an off bulb on, so restoring an initially
/// off bulb applies its stored light parameters first and then sends a final
/// `state: false`.
pub async fn restore(bulb: &Bulb, initial: &Pilot) -> anyhow::Result<()> {
    let patient = bulb.policy().clone();
    let restore_bulb = Bulb::connect_to(bulb.addr())
        .await?
        .with_policy(RetryPolicy {
            attempts: patient.attempts.max(3),
            attempt_timeout: patient.attempt_timeout.max(Duration::from_millis(500)),
            min_interval: patient.min_interval,
        });

    let was_off = initial.state == Some(false);
    let mut builder = PilotBuilder::new();
    let mut has_light_fields = false;

    if let Some(id) = initial.scene_id.filter(|id| *id != 0) {
        if let Ok(scene_id) = SceneId::new(id) {
            builder = builder.scene(scene_id);
            has_light_fields = true;
            if scene_id.takes_speed() {
                if let Some(speed) = initial.speed.and_then(|s| Speed::new(s).ok()) {
                    builder = builder.speed(speed);
                }
            }
        }
    } else if [initial.r, initial.g, initial.b, initial.c, initial.w]
        .into_iter()
        .flatten()
        .any(|v| v > 0)
    {
        if let Some((r, g, b)) = initial.rgb() {
            builder = builder.rgb(Channel::new(r), Channel::new(g), Channel::new(b));
        }
        if let Some(c) = initial.c {
            builder = builder.cold_white(Channel::new(c));
        }
        if let Some(w) = initial.w {
            builder = builder.warm_white(Channel::new(w));
        }
        has_light_fields = true;
    } else if let Some(temp) = initial.temp.and_then(|t| Kelvin::new(t).ok()) {
        builder = builder.temp(temp);
        has_light_fields = true;
    }

    if let Some(dimming) = initial.dimming.and_then(|d| Dimming::new(d).ok()) {
        builder = builder.dimming(dimming);
        has_light_fields = true;
    }
    if let Some(ratio) = initial.ratio.and_then(|r| Ratio::new(r).ok()) {
        builder = builder.ratio(ratio);
        has_light_fields = true;
    }

    if was_off {
        if has_light_fields {
            let _ = restore_bulb.set_pilot(&builder.state(true)).await;
        }
        restore_bulb
            .set_pilot(&PilotBuilder::new().state(false))
            .await?;
    } else {
        builder = builder.state(initial.state.unwrap_or(true));
        restore_bulb.set_pilot(&builder).await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn build_report(
    initial: &Pilot,
    bulb_type: Option<&BulbType>,
    bench: &Bench,
    rate_sweep: &[StepSummary],
    latency: &LatencyResult,
    interpolation: &InterpolationResult,
    pulse_sweep: &[PulseStepSummary],
    restored: bool,
) -> Report {
    let latency_procedure = "Film the terminal stderr LATENCY_STEP marker and the bulb together at >=120 fps; packet-to-photon latency_ms = (first_lit_frame - marker_frame) * (1000 / fps).";
    let interp_procedure = "At >=120 fps, <=1 intermediate frame on INTERPOLATION_STEP indicates a hardware snap; >=2 monotonic intermediate frames indicates an internal ramp of (frames / fps) seconds. A steady blend during INTERPOLATION_ALT indicates the ramp time constant exceeds 1 / (2 * rate_hz).";
    let rates = bench.configured_rates();
    let min_rate_hz = rates.first().copied().unwrap_or(round2(bench.min_rate));
    let max_rate_hz = rates.last().copied().unwrap_or(round2(bench.max_rate));

    let mut json = json!({
        "device": {
            "mac": initial.mac,
            "model": bulb_type.and_then(|b| b.module_name.as_ref().map(ToString::to_string)),
            "firmware": bulb_type.and_then(|b| b.fw_version.clone()),
            "class": bulb_type.map(|b| b.class.as_str()),
            "rssi": initial.rssi,
        },
        "config": {
            "mode": bench.mode,
            "method": bench.mode.method(),
            "min_rate_hz": min_rate_hz,
            "max_rate_hz": max_rate_hz,
            "steps": rates.len(),
            "hold_ms": bench.hold.as_millis() as u64,
        },
        "rate_sweep": rate_sweep,
        "latency_probe": {
            "pattern": latency.pattern,
            "trials": latency.trials,
            "sent": latency.sent,
            "responses": latency.responses,
            "inferred_drops": latency.inferred_drops,
            "socket_rtt_ms": latency.socket_rtt_ms,
            "procedure": latency_procedure,
        },
        "interpolation_probe": {
            "abrupt_step": {
                "pattern": interpolation.step_pattern,
                "sent": interpolation.step_sent,
                "responses": interpolation.step_responses,
                "inferred_drops": interpolation.step_drops,
                "socket_rtt_ms": interpolation.step_rtt_ms,
            },
            "rapid_alternation": {
                "pattern": interpolation.alt_pattern,
                "rate_hz": interpolation.alt_rate_hz,
                "sent": interpolation.alt_sent,
                "responses": interpolation.alt_responses,
                "inferred_drops": interpolation.alt_drops,
                "loss_pct": interpolation.alt_loss_pct,
                "socket_rtt_ms": interpolation.alt_rtt_ms,
            },
            "procedure": interp_procedure,
        },
        "restored": restored,
    });
    if !pulse_sweep.is_empty() {
        json["pulse_sweep"] = json!(pulse_sweep);
    }

    let mut lines = Vec::new();
    let model = bulb_type
        .and_then(|b| b.module_name.as_ref().map(ToString::to_string))
        .unwrap_or_else(|| "unknown".to_owned());
    let fw = bulb_type
        .and_then(|b| b.fw_version.as_deref())
        .unwrap_or("-");
    let class = bulb_type.map_or("-", |b| b.class.as_str());
    let rssi = initial
        .rssi
        .map_or_else(|| "-".to_owned(), |r| format!("{r} dBm"));
    lines.push(format!("device        {model}  fw {fw}  {class}  {rssi}"));
    lines.push(
        "  rate (Hz)  sent  ack  drop    loss   min ms   p50 ms   p95 ms   max ms  jitter ms"
            .to_owned(),
    );
    for step in rate_sweep {
        let (min, med, p95, max, jit) = match step.rtt_ms {
            Some(rtt) => (
                format!("{:>7.2}", rtt.min),
                format!("{:>7.2}", rtt.median),
                format!("{:>7.2}", rtt.p95),
                format!("{:>7.2}", rtt.max),
                format!("{:>9.2}", rtt.jitter),
            ),
            None => (
                "      -".to_owned(),
                "      -".to_owned(),
                "      -".to_owned(),
                "      -".to_owned(),
                "        -".to_owned(),
            ),
        };
        lines.push(format!(
            "  {:>9.2}  {:>4} {:>4}  {:>4}  {:>5.1}%  {min}  {med}  {p95}  {max}  {jit}",
            step.rate_hz, step.sent, step.responses, step.inferred_drops, step.loss_pct
        ));
    }

    let lat_rtt = latency.socket_rtt_ms.map_or_else(
        || "no socket reply".to_owned(),
        |r| {
            format!(
                "socket p50 {:.2} ms (min {:.2}, p95 {:.2}, max {:.2}, jitter {:.2})",
                r.median, r.min, r.p95, r.max, r.jitter
            )
        },
    );
    lines.push(format!(
        "latency       {}  {}/{} ack  {}",
        latency.pattern, latency.responses, latency.sent, lat_rtt
    ));
    lines.push(format!("camera        {latency_procedure}"));

    let step_rtt = interpolation
        .step_rtt_ms
        .map_or_else(|| "-".to_owned(), |r| format!("{:.2} ms", r.median));
    let alt_rtt = interpolation
        .alt_rtt_ms
        .map_or_else(|| "-".to_owned(), |r| format!("p50 {:.2} ms", r.median));
    lines.push(format!(
        "interpolate   {} {}/{} ack ({}); {} @ {:.2} Hz {}/{} ack ({} drops, {})",
        interpolation.step_pattern,
        interpolation.step_responses,
        interpolation.step_sent,
        step_rtt,
        interpolation.alt_pattern,
        interpolation.alt_rate_hz,
        interpolation.alt_responses,
        interpolation.alt_sent,
        interpolation.alt_drops,
        alt_rtt
    ));
    if !pulse_sweep.is_empty() {
        let formatted: Vec<String> = pulse_sweep
            .iter()
            .map(|p| {
                let rtt = p
                    .socket_rtt_ms
                    .map_or_else(|| "drop".to_owned(), |ms| format!("{ms:.2} ms"));
                format!("{}ms ({rtt})", p.duration_ms)
            })
            .collect();
        lines.push(format!(
            "pulse         delta {:+}%  {}",
            pulse_sweep[0].delta,
            formatted.join(", ")
        ));
    }
    lines.push(format!(
        "restored      {}",
        if restored { "yes" } else { "no" }
    ));

    Report::new(json, lines.join("\n"))
}

fn is_stopped(stop: &watch::Receiver<bool>) -> bool {
    *stop.borrow()
}

async fn pause(duration: Duration, stop: &mut watch::Receiver<bool>) -> bool {
    if duration.is_zero() {
        return is_stopped(stop);
    }
    tokio::select! {
        changed = stop.changed() => changed.is_err() || *stop.borrow(),
        () = tokio::time::sleep(duration) => is_stopped(stop),
    }
}

fn note<W: io::Write>(stderr: &Arc<Mutex<W>>, line: &str) {
    if let Ok(mut guard) = stderr.lock() {
        let _ = writeln!(guard, "{line}");
        let _ = guard.flush();
    }
}

fn utc_timestamp() -> String {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = elapsed.as_secs();
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = seconds % 86_400;
    let (year, month, day) = civil_date(days);
    let hour = seconds_of_day / 3_600;
    let minute = seconds_of_day % 3_600 / 60;
    let second = seconds_of_day % 60;
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z",
        elapsed.subsec_millis()
    )
}

fn civil_date(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

fn positive_hz(input: &str) -> Result<f64, String> {
    let hz: f64 = input
        .parse()
        .map_err(|_| format!("`{input}` is not a rate in Hz"))?;
    if !hz.is_finite() || hz <= 0.0 {
        return Err(format!("rate must be greater than 0 Hz, got `{input}`"));
    }
    Ok(hz)
}

fn non_zero_usize(input: &str) -> Result<usize, String> {
    let count: usize = input
        .parse()
        .map_err(|_| format!("`{input}` is not a step count"))?;
    if count == 0 {
        return Err("step count must be at least 1".to_owned());
    }
    Ok(count)
}

fn positive_duration(input: &str) -> Result<Duration, String> {
    let duration = seconds(input)?;
    if duration.is_zero() {
        return Err("hold duration must be greater than 0 seconds".to_owned());
    }
    Ok(duration)
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;
    use serde_json::{Value, json};

    use super::super::watch::mock_bulb::MockBulb;
    use super::*;

    #[test]
    fn rate_steps_spans_configured_range_deterministically() {
        assert_eq!(
            rate_steps(1.0, 50.0, 8),
            vec![1.0, 8.0, 15.0, 22.0, 29.0, 36.0, 43.0, 50.0]
        );
        assert_eq!(rate_steps(10.0, 30.0, 3), vec![10.0, 20.0, 30.0]);
        assert_eq!(rate_steps(20.0, 20.0, 5), vec![20.0]);
        assert_eq!(rate_steps(5.0, 50.0, 1), vec![5.0]);
    }

    #[test]
    fn rtt_stats_computes_min_median_p95_max_and_jitter() {
        assert_eq!(RttStats::from_samples(&[]), None);

        let single = RttStats::from_samples(&[Duration::from_millis(42)]).expect("single sample");
        assert_eq!(
            single,
            RttStats {
                min: 42.0,
                median: 42.0,
                p95: 42.0,
                max: 42.0,
                jitter: 0.0,
            }
        );

        // Chronological order: 10, 30, 20, 40, 100 ms.
        // Consecutive diffs: |30-10|=20, |20-30|=10, |40-20|=20, |100-40|=60 -> mean = 110 / 4 = 27.5 ms.
        // Sorted: 10, 20, 30, 40, 100 -> min=10, median=30, p95=100, max=100.
        let samples = [10, 30, 20, 40, 100].map(Duration::from_millis);
        let stats = RttStats::from_samples(&samples).expect("five samples");
        assert_eq!(stats.min, 10.0);
        assert_eq!(stats.median, 30.0);
        assert_eq!(stats.p95, 100.0);
        assert_eq!(stats.max, 100.0);
        assert_eq!(stats.jitter, 27.5);
    }

    #[tokio::test]
    async fn bench_restores_pre_bench_state_on_completion_and_warns_on_stderr() {
        let bulb = MockBulb::builder()
            .pilot(json!({
                "mac": "9877d5230f0a",
                "state": true,
                "r": 120,
                "g": 45,
                "b": 200,
                "c": 0,
                "w": 32,
                "dimming": 65,
                "sceneId": 0,
                "rssi": -54
            }))
            .start()
            .await;
        let addr = bulb.addr().to_string();
        let cli = Cli::try_parse_from([
            "wizlight",
            "--json",
            "bench",
            &addr,
            "--min-rate",
            "20",
            "--max-rate",
            "40",
            "--steps",
            "2",
            "--hold",
            "0.05",
        ])
        .expect("parse bench");
        let super::super::Command::Bench(bench) = &cli.command else {
            panic!("expected bench");
        };
        let stderr = Arc::new(Mutex::new(Vec::<u8>::new()));

        let outcome = run_with(&cli, bench, Arc::clone(&stderr), std::future::pending())
            .await
            .expect("bench succeeds");

        let stderr_text =
            String::from_utf8(stderr.lock().expect("lock").clone()).expect("stderr utf-8");
        assert!(
            stderr_text.contains("warning: benchmarking will visibly flash"),
            "{stderr_text}"
        );
        assert!(stderr_text.contains("LATENCY_STEP"), "{stderr_text}");
        assert!(stderr_text.contains("INTERPOLATION_STEP"), "{stderr_text}");

        let rendered: Value =
            serde_json::from_str(&outcome.render(&cli.command, true)).expect("valid json");
        assert_eq!(rendered["ok"], true);
        assert_eq!(rendered["command"], "bench");
        assert_eq!(rendered["result"]["restored"], true);
        assert_eq!(
            rendered["result"]["rate_sweep"]
                .as_array()
                .expect("steps")
                .len(),
            2
        );

        let final_pilot = bulb.pilot();
        assert_eq!(final_pilot["state"], true);
        assert_eq!(final_pilot["r"], 120);
        assert_eq!(final_pilot["g"], 45);
        assert_eq!(final_pilot["b"], 200);
        assert_eq!(final_pilot["w"], 32);
        assert_eq!(final_pilot["dimming"], 65);
    }

    #[tokio::test]
    async fn bench_restores_an_off_bulb_when_interrupted_by_ctrl_c() {
        let bulb = MockBulb::builder()
            .pilot(json!({
                "mac": "9877d5230f0a",
                "state": false,
                "sceneId": 11,
                "temp": 2700,
                "dimming": 45
            }))
            .start()
            .await;
        let addr = bulb.addr().to_string();
        let cli = Cli::try_parse_from([
            "wizlight",
            "bench",
            &addr,
            "--min-rate",
            "10",
            "--max-rate",
            "50",
            "--steps",
            "4",
            "--hold",
            "0.5",
        ])
        .expect("parse bench");
        let super::super::Command::Bench(bench) = &cli.command else {
            panic!("expected bench");
        };
        let stderr = Arc::new(Mutex::new(Vec::<u8>::new()));
        let shutdown = async {
            tokio::time::sleep(Duration::from_millis(60)).await;
            Ok(())
        };

        let err = run_with(&cli, bench, stderr, shutdown)
            .await
            .expect_err("interrupted run returns error");
        assert!(err.to_string().contains("interrupted"), "{err}");

        let final_pilot = bulb.pilot();
        assert_eq!(final_pilot["state"], false, "{final_pilot}");
        assert_eq!(final_pilot["sceneId"], 11, "{final_pilot}");
        assert_eq!(final_pilot["dimming"], 45, "{final_pilot}");
    }

    #[test]
    fn bench_report_human_and_json_match_snapshots() {
        let cli = Cli::try_parse_from([
            "wizlight",
            "bench",
            "9877d5230f0a",
            "--rates",
            "5,50",
            "--hold",
            "1",
            "--pulse-durations",
            "50,150",
            "--pulse-delta",
            "-80",
        ])
        .expect("parse");
        let super::super::Command::Bench(bench) = &cli.command else {
            panic!("expected bench");
        };
        let initial: Pilot = serde_json::from_value(json!({
            "mac": "9877d5230f0a",
            "rssi": -52,
            "state": true
        }))
        .unwrap();
        let bulb_type = BulbType::from_data(&crate::protocol::BulbData {
            module_name: Some("ESP25_SHRGB_01"),
            type_id: None,
            fw_version: Some("1.38.0"),
            kelvin_range: Some(crate::KelvinRange::new(2200, 6500)),
            white_channels: Some(1),
            white_to_color_ratio: Some(80),
            fan_speed_range: None,
        })
        .unwrap();
        let rtt = RttStats {
            min: 18.4,
            median: 24.5,
            p95: 41.2,
            max: 49.0,
            jitter: 6.15,
        };
        let rate_sweep = [
            StepSummary {
                rate_hz: 5.0,
                interval_ms: 200.0,
                sent: 5,
                responses: 5,
                inferred_drops: 0,
                loss_pct: 0.0,
                rtt_ms: Some(rtt),
            },
            StepSummary {
                rate_hz: 50.0,
                interval_ms: 20.0,
                sent: 50,
                responses: 49,
                inferred_drops: 1,
                loss_pct: 2.0,
                rtt_ms: Some(rtt),
            },
        ];
        let latency = LatencyResult {
            pattern: "black_to_white_step",
            trials: 3,
            sent: 3,
            responses: 3,
            inferred_drops: 0,
            socket_rtt_ms: Some(rtt),
        };
        let interpolation = InterpolationResult {
            step_pattern: "red_to_blue_step",
            step_sent: 1,
            step_responses: 1,
            step_drops: 0,
            step_rtt_ms: Some(rtt),
            alt_pattern: "red_blue_alternation",
            alt_rate_hz: 20.0,
            alt_sent: 20,
            alt_responses: 20,
            alt_drops: 0,
            alt_loss_pct: 0.0,
            alt_rtt_ms: Some(rtt),
        };
        let pulse_sweep = [
            PulseStepSummary {
                delta: -80,
                duration_ms: 50,
                sent: 1,
                responses: 1,
                inferred_drops: 0,
                socket_rtt_ms: Some(22.1),
            },
            PulseStepSummary {
                delta: -80,
                duration_ms: 150,
                sent: 1,
                responses: 1,
                inferred_drops: 0,
                socket_rtt_ms: Some(25.4),
            },
        ];

        let report = build_report(
            &initial,
            Some(&bulb_type),
            bench,
            &rate_sweep,
            &latency,
            &interpolation,
            &pulse_sweep,
            true,
        );

        insta::assert_snapshot!(report.human, @r"
        device        ESP25_SHRGB_01  fw 1.38.0  RGB  -52 dBm
          rate (Hz)  sent  ack  drop    loss   min ms   p50 ms   p95 ms   max ms  jitter ms
               5.00     5    5     0    0.0%    18.40    24.50    41.20    49.00       6.15
              50.00    50   49     1    2.0%    18.40    24.50    41.20    49.00       6.15
        latency       black_to_white_step  3/3 ack  socket p50 24.50 ms (min 18.40, p95 41.20, max 49.00, jitter 6.15)
        camera        Film the terminal stderr LATENCY_STEP marker and the bulb together at >=120 fps; packet-to-photon latency_ms = (first_lit_frame - marker_frame) * (1000 / fps).
        interpolate   red_to_blue_step 1/1 ack (24.50 ms); red_blue_alternation @ 20.00 Hz 20/20 ack (0 drops, p50 24.50 ms)
        pulse         delta -80%  50ms (22.10 ms), 150ms (25.40 ms)
        restored      yes
        ");
        insta::assert_json_snapshot!(report.json, @r#"
        {
          "config": {
            "hold_ms": 1000,
            "max_rate_hz": 50.0,
            "method": "setPilot",
            "min_rate_hz": 5.0,
            "mode": "auto",
            "steps": 2
          },
          "device": {
            "class": "RGB",
            "firmware": "1.38.0",
            "mac": "9877d5230f0a",
            "model": "ESP25_SHRGB_01",
            "rssi": -52
          },
          "interpolation_probe": {
            "abrupt_step": {
              "inferred_drops": 0,
              "pattern": "red_to_blue_step",
              "responses": 1,
              "sent": 1,
              "socket_rtt_ms": {
                "jitter": 6.15,
                "max": 49.0,
                "median": 24.5,
                "min": 18.4,
                "p95": 41.2
              }
            },
            "procedure": "At >=120 fps, <=1 intermediate frame on INTERPOLATION_STEP indicates a hardware snap; >=2 monotonic intermediate frames indicates an internal ramp of (frames / fps) seconds. A steady blend during INTERPOLATION_ALT indicates the ramp time constant exceeds 1 / (2 * rate_hz).",
            "rapid_alternation": {
              "inferred_drops": 0,
              "loss_pct": 0.0,
              "pattern": "red_blue_alternation",
              "rate_hz": 20.0,
              "responses": 20,
              "sent": 20,
              "socket_rtt_ms": {
                "jitter": 6.15,
                "max": 49.0,
                "median": 24.5,
                "min": 18.4,
                "p95": 41.2
              }
            }
          },
          "latency_probe": {
            "inferred_drops": 0,
            "pattern": "black_to_white_step",
            "procedure": "Film the terminal stderr LATENCY_STEP marker and the bulb together at >=120 fps; packet-to-photon latency_ms = (first_lit_frame - marker_frame) * (1000 / fps).",
            "responses": 3,
            "sent": 3,
            "socket_rtt_ms": {
              "jitter": 6.15,
              "max": 49.0,
              "median": 24.5,
              "min": 18.4,
              "p95": 41.2
            },
            "trials": 3
          },
          "pulse_sweep": [
            {
              "delta": -80,
              "duration_ms": 50,
              "inferred_drops": 0,
              "responses": 1,
              "sent": 1,
              "socket_rtt_ms": 22.1
            },
            {
              "delta": -80,
              "duration_ms": 150,
              "inferred_drops": 0,
              "responses": 1,
              "sent": 1,
              "socket_rtt_ms": 25.4
            }
          ],
          "rate_sweep": [
            {
              "inferred_drops": 0,
              "interval_ms": 200.0,
              "loss_pct": 0.0,
              "rate_hz": 5.0,
              "responses": 5,
              "rtt_ms": {
                "jitter": 6.15,
                "max": 49.0,
                "median": 24.5,
                "min": 18.4,
                "p95": 41.2
              },
              "sent": 5
            },
            {
              "inferred_drops": 1,
              "interval_ms": 20.0,
              "loss_pct": 2.0,
              "rate_hz": 50.0,
              "responses": 49,
              "rtt_ms": {
                "jitter": 6.15,
                "max": 49.0,
                "median": 24.5,
                "min": 18.4,
                "p95": 41.2
              },
              "sent": 50
            }
          ],
          "restored": true
        }
        "#);
    }
}
