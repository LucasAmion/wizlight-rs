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

use std::net::{IpAddr, SocketAddr};
use std::process::ExitCode;
use std::time::Duration;

use clap::{ArgAction, Args, Parser, Subcommand};
use serde_json::{Value, json};

mod discover;
mod output;

pub use output::{
    HumanRenderer, JsonRenderer, OutputRenderer, colour_on_stderr, colour_on_stdout, render_json,
};

use crate::{PORT, RetryPolicy};

/// Global CLI flags shared across all commands.
///
/// `-v` is verbosity and `-V` is the version, following `cargo`'s convention.
///
/// `long_about = None` keeps this doc comment out of `--help`. Without it clap
/// promotes the comment to the long description, so `wizlight --help` opened
/// with "Global CLI flags shared across all commands." while `-h` showed the
/// real one — internal notes leaking into user-facing help.
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
    /// The default reaches every bulb on the directly attached network. A
    /// host on several networks needs one of these per subnet, because the
    /// kernel routes the all-subnets address out of one interface only.
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

/// The bulb a command acts on.
#[derive(Args, Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The target bulb, as an IP address or MAC.
    pub target: String,
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
    /// Power a bulb on.
    On(Target),
    /// Power a bulb off.
    Off(Target),
    /// Toggle the power state of a bulb.
    Toggle(Target),
    /// Change a bulb's state.
    Set(Target),
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

    /// The bulb this command acts on, if it takes one.
    #[must_use]
    pub fn target(&self) -> Option<&str> {
        match self {
            Self::Discover => None,
            Self::Status(t)
            | Self::Info(t)
            | Self::On(t)
            | Self::Off(t)
            | Self::Toggle(t)
            | Self::Set(t)
            | Self::Scenes(t)
            | Self::Bench(t)
            | Self::Watch(t) => Some(&t.target),
        }
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
}

impl Outcome {
    /// An outcome carrying its `result` payload and its human rendering.
    #[must_use]
    pub fn new(result: Value, human: impl Into<String>) -> Self {
        Self {
            result,
            human: human.into(),
        }
    }

    /// Renders the outcome in the format the caller asked for.
    #[must_use]
    pub fn render(&self, command: &Command, json: bool) -> String {
        let payload = if json {
            json!({
                "ok": true,
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
    let policy = cli.policy();
    match &cli.command {
        Command::Discover => {
            discover::run(&discover::discovery(&cli.broadcast, &policy), cli.wait).await
        }
        other => anyhow::bail!(
            "`{}` is not implemented yet; this is the CLI scaffold",
            other.name()
        ),
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
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!(
                "{}",
                render_failure(Some(&cli.command), &err.to_string(), cli.json)
            );
            ExitCode::FAILURE
        }
    }
}
