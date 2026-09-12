//! The `wizlight` command-line interface (feature `cli`, on by default).
//!
//! The CLI lives inside the library rather than in `src/bin/` so that argument
//! parsing and output rendering can be exercised from the test suite.
//! `src/main.rs` is a wrapper around [`run`].
//!
//! # Streams
//!
//! Results go to stdout and everything else goes to stderr — diagnostics, log
//! output and errors, including the JSON ones. A script can redirect stdout
//! and parse it without having to strip anything out of the way first.
//!
//! # The `--json` contract
//!
//! Every command emits one JSON object, and the envelope is the same whether
//! the command worked or not:
//!
//! ```json
//! {"ok": true,  "command": "discover", "target": null,        "result": []}
//! {"ok": false, "command": "status",   "target": "192.168.0.7", "error": "…"}
//! ```
//!
//! `ok` says whether the command did what was asked. `command` and `target`
//! echo the invocation, so a fan-out over many bulbs can be told apart by the
//! reader. `result` is present on success and shaped by the command; `error`
//! is present on failure and is the same message the human rendering shows.
//! The envelope is a stable contract; the inside of `result` grows as commands
//! learn to say more.
//!
//! # Exit codes
//!
//! `0` success, `2` for a usage error — clap's own, before anything runs —
//! and `1` for everything else. The distinct codes for *not found* and *timed
//! out* wait for the commands that can produce them: discovery reports what
//! answered, and nothing here yet fails at a bulb that was supposed to be
//! listening.

use std::collections::BTreeSet;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context as _;
use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand};
use if_addrs::IfAddr;
use serde_json::{Value, json};
use tokio::task::JoinSet;

mod describe;
mod discover;
mod output;
mod pilot;
mod target;

pub use output::{
    HumanRenderer, JsonRenderer, OutputRenderer, colour_on_stderr, colour_on_stdout, render_json,
};
pub use pilot::{ColourOptions, StateOptions};
pub use target::{BadTarget, NotFound, Resolved, TargetSpec};

use crate::{Bulb, Discovery, Error, PORT, RetryPolicy};

/// Exit code for a target that nothing answered to.
///
/// Separate from a failure to talk to a bulb that *was* there, because the two
/// call for different reactions: this one wants a re-scan, a
/// [timeout](EXIT_TIMEOUT) wants a retry.
pub const EXIT_NOT_FOUND: u8 = 3;

/// Exit code for a bulb that was found and then stopped answering.
pub const EXIT_TIMEOUT: u8 = 4;

/// Global CLI flags shared across all commands.
///
/// `-v` is verbosity and `-V` is the version, following `cargo`'s convention.
///
/// `long_about = None` keeps this doc comment out of `--help`. Without it clap
/// promotes the comment to the long description, so `wizlight --help` opened
/// with "Global CLI flags shared across all commands." while `-h` showed the
/// real one — internal notes leaking into user-facing help.
///
/// ```
/// use clap::Parser;
/// use wizlight::cli::{Cli, Command};
///
/// let cli = Cli::try_parse_from(["wizlight", "--json", "--wait", "1", "discover"])?;
/// assert!(cli.json);
/// assert!(matches!(cli.command, Command::Discover));
/// # Ok::<(), clap::Error>(())
/// ```
#[derive(Debug, Parser)]
#[command(name = "wizlight", about = "Philips WiZ smart bulb control")]
#[command(version, long_about = None, arg_required_else_help = true)]
pub struct Cli {
    /// Emit JSON instead of human-readable output.
    #[arg(long, short = 'j', global = true, action = ArgAction::SetTrue)]
    pub json: bool,

    /// How long to wait for each reply before trying again, in seconds.
    #[arg(
        long,
        global = true,
        default_value = "2",
        value_name = "SECONDS",
        value_parser = seconds
    )]
    pub timeout: Duration,

    /// How long a discovery scan lasts, in seconds.
    #[arg(
        long,
        global = true,
        default_value = "5",
        value_name = "SECONDS",
        value_parser = seconds
    )]
    pub wait: Duration,

    /// Override the address discovery broadcasts to. Repeatable.
    ///
    /// By default, the subnet broadcast address of every viable local IPv4
    /// interface is used, including on multi-homed hosts. Pass this flag to
    /// restrict or replace those targets.
    #[arg(long, global = true, value_name = "ADDR", value_parser = address)]
    pub broadcast: Vec<SocketAddr>,

    /// Increase logging verbosity. Repeat the flag to add more detail.
    #[arg(long, short = 'v', global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// The selected subcommand.
    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// The retry policy the globals ask for.
    ///
    /// Deliberately more patient than [`RetryPolicy::default`], whose 500 ms is
    /// measured against a bulb at close range. The same bulb further away has
    /// a round trip past a second, and a CLI that gives up on it is worse than
    /// one that takes a moment: the default here is three attempts of
    /// `--timeout` each.
    #[must_use]
    pub fn policy(&self) -> RetryPolicy {
        RetryPolicy {
            attempt_timeout: self.timeout,
            ..RetryPolicy::default()
        }
    }

    /// The discovery run the globals ask for.
    ///
    /// `system_config` costs a round trip per bulb and is only worth paying
    /// for a listing. Resolving a MAC does not need it: the broadcast reply
    /// already carries the one field being matched on.
    ///
    /// Without an explicit `--broadcast`, every viable local IPv4 interface
    /// contributes its subnet broadcast address.
    ///
    /// # Errors
    ///
    /// If local interfaces cannot be enumerated, or none can carry a broadcast.
    pub fn discovery(&self, system_config: bool) -> anyhow::Result<Discovery> {
        let discovery = Discovery::new()
            .system_config(system_config)
            .policy(self.policy());
        let targets = broadcast_targets(&self.broadcast, local_ipv4_interfaces)?;
        tracing::debug!(?targets, "selected discovery targets");
        Ok(targets
            .iter()
            .fold(discovery, |discovery, addr| discovery.target(*addr)))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InterfaceAddress {
    ip: Ipv4Addr,
    netmask: Ipv4Addr,
    up: bool,
    point_to_point: bool,
}

fn local_ipv4_interfaces() -> io::Result<Vec<InterfaceAddress>> {
    if_addrs::get_if_addrs().map(|interfaces| {
        interfaces
            .into_iter()
            .filter_map(|interface| {
                let up = interface.is_oper_up();
                let point_to_point = interface.is_p2p();
                match interface.addr {
                    IfAddr::V4(addr) => Some(InterfaceAddress {
                        ip: addr.ip,
                        netmask: addr.netmask,
                        up,
                        point_to_point,
                    }),
                    IfAddr::V6(_) => None,
                }
            })
            .collect()
    })
}

fn broadcast_targets<F>(explicit: &[SocketAddr], enumerate: F) -> anyhow::Result<Vec<SocketAddr>>
where
    F: FnOnce() -> io::Result<Vec<InterfaceAddress>>,
{
    if !explicit.is_empty() {
        return Ok(explicit.to_vec());
    }

    let interfaces = enumerate().context("could not enumerate local network interfaces")?;
    let targets: BTreeSet<SocketAddr> = interfaces
        .into_iter()
        .filter_map(|interface| {
            let mask = u32::from(interface.netmask);
            let host_mask = !mask;
            let host_bits = host_mask.count_ones();
            let contiguous = mask.leading_ones() + mask.trailing_zeros() == 32;
            let viable = interface.up
                && !interface.point_to_point
                && !interface.ip.is_loopback()
                && !interface.ip.is_link_local()
                && !interface.ip.is_unspecified()
                && !interface.ip.is_multicast()
                && interface.ip != Ipv4Addr::BROADCAST
                && contiguous
                && (2..32).contains(&host_bits);
            viable.then(|| {
                SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::from(u32::from(interface.ip) | host_mask)),
                    PORT,
                )
            })
        })
        .collect();

    if targets.is_empty() {
        anyhow::bail!("no viable IPv4 network interface found; pass --broadcast ADDRESS")
    }
    Ok(targets.into_iter().collect())
}

/// Parses a number of seconds, which may be fractional.
///
/// `try_from_secs_f64` rejects negatives, NaN and anything that overflows a
/// `Duration`, so `--timeout -1` is a usage error rather than an instant
/// failure much later.
fn seconds(input: &str) -> Result<Duration, String> {
    let secs: f64 = input
        .parse()
        .map_err(|_| format!("`{input}` is not a number of seconds"))?;
    Duration::try_from_secs_f64(secs).map_err(|err| format!("`{input}`: {err}"))
}

/// Parses an address, supplying the WiZ [`PORT`] when none was given.
///
/// Bulbs are always on 38899, so making it optional is the difference between
/// `--broadcast 192.168.0.255` and remembering a constant. The port is still
/// accepted, which is how a test points the CLI at a bulb on a loopback port.
fn address(input: &str) -> Result<SocketAddr, String> {
    if let Ok(addr) = input.parse::<SocketAddr>() {
        return Ok(addr);
    }
    input
        .parse::<IpAddr>()
        .map(|ip| SocketAddr::new(ip, PORT))
        .map_err(|_| format!("`{input}` is not an address"))
}

/// Which bulbs a command acts on: one named, or all of them.
///
/// Exactly one of the two is required, and clap enforces that — `--all` with
/// a target is a contradiction, and neither is a command with nothing to act
/// on.
#[derive(Args, Debug, Clone, PartialEq, Eq)]
#[group(required = true, multiple = false)]
pub struct Target {
    /// The target bulb, as an IP address or a MAC.
    #[arg(value_name = "TARGET")]
    pub target: Option<TargetSpec>,

    /// Act on every bulb a scan finds.
    #[arg(long)]
    pub all: bool,
}

/// A command that writes state: which bulbs, and what to set.
#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct Write {
    /// Which bulbs to write to.
    #[command(flatten)]
    pub target: Target,

    /// What to set.
    #[command(flatten)]
    pub options: StateOptions,
}

/// One bulb's contribution to a command's output.
///
/// A fan-out collects one of these per bulb; a single target produces one and
/// renders it alone.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    json: Value,
    human: String,
}

impl Report {
    /// Builds a report from its two renderings.
    #[must_use]
    pub fn new(json: Value, human: impl Into<String>) -> Self {
        Self {
            json,
            human: human.into(),
        }
    }
}

/// Supported commands.
///
/// The tree is complete ahead of the implementations so that the surface, the
/// help text and the JSON contract can settle before commands land against
/// them.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Discover bulbs on the current LAN.
    Discover,
    /// Print the current pilot state for a target bulb.
    Status(Target),
    /// Print model info and supported capabilities for a target bulb.
    Info(Target),
    /// Power a bulb on, optionally setting what it shows.
    On(Write),
    /// Power a bulb off.
    Off(Target),
    /// Toggle the power state of a bulb.
    Toggle(Target),
    /// Change what a bulb shows, with `setState`.
    ///
    /// This does not leave a bulb that was off alone: measured on
    /// `ESP25_SHRGB_01` fw 1.38.0, `setState` turns it on exactly as
    /// `setPilot` does.
    Set(Write),
    /// List the scenes supported by the target bulb.
    Scenes(Target),
    /// Tail `syncPilot` push updates from a target bulb.
    Watch(Target),
    /// Benchmark the bulb update rate and latency.
    Bench(Target),
}

impl Command {
    /// The name as it is spelled on the command line, and in `--json` output.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Discover => "discover",
            Self::Status(_) => "status",
            Self::Info(_) => "info",
            Self::On(_) => "on",
            Self::Off(_) => "off",
            Self::Toggle(_) => "toggle",
            Self::Set(_) => "set",
            Self::Scenes(_) => "scenes",
            Self::Watch(_) => "watch",
            Self::Bench(_) => "bench",
        }
    }

    /// Which bulbs this command acts on, if it takes any.
    #[must_use]
    pub fn selection(&self) -> Option<&Target> {
        match self {
            Self::Discover => None,
            Self::On(write) | Self::Set(write) => Some(&write.target),
            Self::Status(t)
            | Self::Info(t)
            | Self::Off(t)
            | Self::Toggle(t)
            | Self::Scenes(t)
            | Self::Bench(t)
            | Self::Watch(t) => Some(t),
        }
    }

    /// The target as it was spelled on the command line, for output to echo.
    ///
    /// `None` under `--all`, where there is no one target and each bulb
    /// carries its own label in the results.
    #[must_use]
    pub fn target(&self) -> Option<&str> {
        Some(self.selection()?.target.as_ref()?.raw())
    }
}

/// What a command produced.
///
/// Both renderings are built together rather than one being derived from the
/// other, because neither can be: the JSON is a contract and the human form is
/// a layout. Building both where the data is still typed is what stops a
/// command from growing a field in one format and not the other.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    result: Value,
    human: String,
    ok: bool,
}

impl Outcome {
    /// An outcome where everything the command touched succeeded.
    #[must_use]
    pub fn new(result: Value, human: impl Into<String>) -> Self {
        Self {
            result,
            human: human.into(),
            ok: true,
        }
    }

    /// An outcome with results worth printing and a failure to report.
    ///
    /// A fan-out that lost one bulb of three still has two results the caller
    /// wants, and still must not exit `0`.
    #[must_use]
    pub fn partial(result: Value, human: impl Into<String>) -> Self {
        Self {
            ok: false,
            ..Self::new(result, human)
        }
    }

    /// Whether the command did what was asked, everywhere it was asked.
    #[must_use]
    pub const fn succeeded(&self) -> bool {
        self.ok
    }

    /// Renders the outcome in the format the caller asked for.
    #[must_use]
    pub fn render(&self, command: &Command, json: bool) -> String {
        let payload = if json {
            json!({
                "ok": self.ok,
                "command": command.name(),
                "target": command.target(),
                "result": self.result,
            })
        } else {
            Value::String(self.human.clone())
        };
        renderer(json).render(&payload)
    }
}

/// Runs the selected command.
///
/// # Errors
///
/// Whatever the command failed with. Commands that are still stubbed fail
/// saying so.
pub async fn run_command(cli: &Cli) -> anyhow::Result<Outcome> {
    match &cli.command {
        Command::Discover => {
            let discovery = cli.discovery(true)?;
            discover::run(&discovery, cli.wait).await
        }
        Command::Status(target) => {
            act(
                cli,
                target,
                |bulb| async move { pilot::status(&bulb).await },
            )
            .await
        }
        Command::Info(target) => {
            act(
                cli,
                target,
                |bulb| async move { describe::info(&bulb).await },
            )
            .await
        }
        Command::Scenes(target) => {
            act(
                cli,
                target,
                |bulb| async move { describe::scenes(&bulb).await },
            )
            .await
        }
        Command::On(write) => {
            let options = write.options.clone();
            act(cli, &write.target, move |bulb| {
                let options = options.clone();
                async move { pilot::power(&bulb, true, &options).await }
            })
            .await
        }
        Command::Off(target) => {
            act(cli, target, |bulb| async move {
                pilot::power(&bulb, false, &StateOptions::default()).await
            })
            .await
        }
        Command::Toggle(target) => {
            act(
                cli,
                target,
                |bulb| async move { pilot::toggle(&bulb).await },
            )
            .await
        }
        Command::Set(write) => {
            let options = write.options.clone();
            act(cli, &write.target, move |bulb| {
                let options = options.clone();
                async move { pilot::set(&bulb, &options).await }
            })
            .await
        }
        other => anyhow::bail!(
            "`{}` is not implemented yet; this is the CLI scaffold",
            other.name()
        ),
    }
}

/// Resolves the target, runs `op` against every bulb it named, and collects
/// the results.
///
/// One place for the whole shape of a per-bulb command: resolution, the
/// fan-out, and the rule that one bulb failing does not abort the others.
async fn act<F, Fut>(cli: &Cli, selection: &Target, op: F) -> anyhow::Result<Outcome>
where
    F: Fn(Bulb) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = anyhow::Result<Report>> + Send + 'static,
{
    let policy = cli.policy();

    if let Some(spec) = &selection.target {
        let bulb = match spec.address() {
            Some(addr) => Resolved { addr, mac: None },
            None => {
                let discovery = cli.discovery(false)?;
                target::resolve(spec, &discovery, cli.wait).await?
            }
        };
        let report = op(bulb.connect(&policy).await?).await?;
        return Ok(Outcome::new(report.json, report.human));
    }

    let discovery = cli.discovery(false)?;
    let bulbs = target::resolve_all(&discovery, cli.wait).await?;
    tracing::info!(bulbs = bulbs.len(), "fanning out");

    // Concurrently, because the alternative is a round trip per bulb in
    // series, and a room's worth of bulbs changing colour one after another
    // looks like a fault.
    let mut tasks = JoinSet::new();
    for bulb in bulbs {
        let op = op.clone();
        let policy = policy.clone();
        tasks.spawn(async move {
            let label = bulb.label();
            let result = match bulb.connect(&policy).await {
                Ok(handle) => op(handle).await,
                Err(err) => Err(err.into()),
            };
            (label, result)
        });
    }

    let mut results = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        results.push(joined?);
    }
    results.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(collect(results))
}

/// Turns per-bulb results into one outcome.
///
/// A bulb that failed appears in the output next to the ones that worked —
/// hiding it would make a fan-out over a room a coin toss — and its presence
/// is what makes the exit code non-zero.
fn collect(results: Vec<(String, anyhow::Result<Report>)>) -> Outcome {
    let failed = results.iter().filter(|(_, r)| r.is_err()).count();
    let json: Vec<Value> = results
        .iter()
        .map(|(label, result)| match result {
            Ok(report) => json!({"target": label, "ok": true, "result": report.json}),
            Err(err) => json!({"target": label, "ok": false, "error": err.to_string()}),
        })
        .collect();
    let human = results
        .iter()
        .map(|(label, result)| match result {
            Ok(report) => prefixed(label, &report.human),
            Err(err) => prefixed(label, &format!("error: {err}")),
        })
        .collect::<Vec<_>>()
        .join("\n");

    let value = Value::Array(json);
    if failed == 0 {
        Outcome::new(value, human)
    } else {
        Outcome::partial(value, human)
    }
}

/// Labels a bulb's slice of a fan-out.
///
/// A one-line report stays on one line, so `--all` output remains greppable;
/// anything longer gets a heading and an indent, because a `scenes` listing
/// with a MAC repeated down the left margin is unreadable.
fn prefixed(label: &str, body: &str) -> String {
    if body.contains('\n') {
        let indented: Vec<String> = body.lines().map(|line| format!("  {line}")).collect();
        format!("{label}:\n{}", indented.join("\n"))
    } else {
        format!("{label}  {body}")
    }
}

/// Renders a failure in the format the caller asked for.
///
/// One function so the two formats cannot drift, and so nothing prints a
/// failure twice: the command runner returns the error, and only this renders
/// it.
#[must_use]
pub fn render_failure(command: Option<&Command>, message: &str, json: bool) -> String {
    let payload = if json {
        json!({
            "ok": false,
            "command": command.map(Command::name),
            "target": command.and_then(Command::target),
            "error": message,
        })
    } else {
        Value::String(format!("error: {message}"))
    };
    renderer(json).render(&payload)
}

/// The renderer for the selected output format.
#[must_use]
pub fn renderer(json: bool) -> Box<dyn OutputRenderer> {
    if json {
        Box::new(JsonRenderer)
    } else {
        Box::new(HumanRenderer)
    }
}

/// The exit code a failure deserves.
///
/// `downcast_ref` searches the whole context chain, so a library error keeps
/// its meaning however many layers of context it picked up on the way out.
fn exit_code(err: &anyhow::Error) -> ExitCode {
    if err.downcast_ref::<NotFound>().is_some() {
        return ExitCode::from(EXIT_NOT_FOUND);
    }
    if matches!(err.downcast_ref::<Error>(), Some(Error::Timeout { .. })) {
        return ExitCode::from(EXIT_TIMEOUT);
    }
    ExitCode::FAILURE
}

/// Installs the log subscriber for the requested verbosity.
///
/// `RUST_LOG` wins when it is set, so the flag is a shorthand rather than a
/// restriction. Output goes to stderr, which keeps it clear of `--json`, and
/// is coloured only when stderr is a terminal — `tracing-subscriber` would
/// otherwise write escape sequences straight into a pipe or a log file.
fn init_logging(verbose: u8, json: bool) {
    let default = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(colour_on_stderr(json))
        .init();
}

/// Parses the command line, runs the requested command and renders the result.
///
/// Returns the process exit code rather than a `Result`, so that the error is
/// rendered in the requested format instead of by `anyhow`'s `Debug` output.
#[must_use]
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    // The rules clap's own derive cannot state, reported the way clap would
    // have: both exit 2 like any other misuse. `set` with nothing to set is
    // one; a colour with every channel at zero is the other, and that one
    // applies to `on` as well, where the bulb would answer `success` having
    // quietly ignored it.
    if let Command::Set(write) = &cli.command {
        if let Err(err) = write.options.require_something(&mut Cli::command()) {
            err.exit();
        }
    }
    if let Command::On(write) | Command::Set(write) = &cli.command {
        if let Err(err) = write.options.require_something_lit(&mut Cli::command()) {
            err.exit();
        }
    }
    init_logging(cli.verbose, cli.json);

    tracing::debug!(
        command = cli.command.name(),
        target = cli.command.target(),
        timeout = ?cli.timeout,
        wait = ?cli.wait,
        broadcast = ?cli.broadcast,
        json = cli.json,
        "parsed invocation"
    );

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!(
                "{}",
                render_failure(
                    Some(&cli.command),
                    &format!("could not start the async runtime: {err}"),
                    cli.json
                )
            );
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run_command(&cli)) {
        Ok(outcome) => {
            println!("{}", outcome.render(&cli.command, cli.json));
            if outcome.succeeded() {
                ExitCode::SUCCESS
            } else {
                // The results are on stdout and the bulbs that failed are
                // named in them, so there is nothing to add on stderr.
                ExitCode::FAILURE
            }
        }
        Err(err) => {
            eprintln!(
                "{}",
                render_failure(Some(&cli.command), &err.to_string(), cli.json)
            );
            exit_code(&err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{InterfaceAddress, broadcast_targets};
    use std::io;
    use std::net::{Ipv4Addr, SocketAddr};

    fn interface(ip: [u8; 4], prefix: u8, up: bool, point_to_point: bool) -> InterfaceAddress {
        let netmask = match prefix {
            0 => 0,
            prefix => u32::MAX << (32 - prefix),
        };
        InterfaceAddress {
            ip: Ipv4Addr::from(ip),
            netmask: Ipv4Addr::from(netmask),
            up,
            point_to_point,
        }
    }

    #[test]
    fn automatic_broadcasts_cover_every_viable_subnet_once() {
        let interfaces = vec![
            interface([192, 168, 4, 20], 24, true, false),
            interface([192, 168, 4, 21], 24, true, false),
            interface([10, 12, 8, 9], 20, true, false),
            interface([172, 16, 1, 8], 24, false, false),
            interface([127, 0, 0, 1], 8, true, false),
            interface([169, 254, 8, 2], 16, true, false),
            interface([100, 64, 0, 1], 30, true, true),
            interface([198, 51, 100, 8], 31, true, false),
            interface([203, 0, 113, 8], 32, true, false),
        ];

        let targets = broadcast_targets(&[], || Ok(interfaces)).expect("interfaces enumerate");
        let expected: Vec<SocketAddr> = ["10.12.15.255:38899", "192.168.4.255:38899"]
            .into_iter()
            .map(|addr| addr.parse().expect("a socket address"))
            .collect();
        assert_eq!(targets, expected);
    }

    #[test]
    fn automatic_broadcasts_fail_without_a_viable_interface() {
        let interfaces = vec![
            interface([127, 0, 0, 1], 8, true, false),
            interface([169, 254, 8, 2], 16, true, false),
            interface([192, 168, 4, 20], 24, false, false),
        ];

        let error = broadcast_targets(&[], || Ok(interfaces)).expect_err("no viable interface");
        assert!(error.to_string().contains("--broadcast"));
    }

    #[test]
    fn explicit_broadcasts_bypass_interface_enumeration() {
        let explicit = ["192.168.9.255:38899".parse().expect("a socket address")];
        let targets = broadcast_targets(&explicit, || -> io::Result<Vec<InterfaceAddress>> {
            panic!("explicit targets must not enumerate interfaces")
        })
        .expect("explicit targets work without enumeration");

        assert_eq!(targets, explicit);
    }
}
