//! The `wizlight` command-line interface (feature `cli`, on by default).
//!
//! The CLI lives inside the library rather than in `src/bin/` so that argument
//! parsing and output rendering can be exercised from the test suite.
//! `src/main.rs` is a wrapper around [`run`].
//!
//! This is the CLI scaffold: the command tree, global flags and renderers are
//! in place, while the bulb operations themselves are still stubbed. Every
//! command therefore fails, and says so in whichever format was asked for.
//!
//! # Streams
//!
//! Results go to stdout and everything else goes to stderr — diagnostics, log
//! output and errors, including the JSON ones. A script can redirect stdout
//! and parse it without having to strip anything out of the way first.

use std::process::ExitCode;

use clap::{ArgAction, Args, Parser, Subcommand};
use serde_json::{Value, json};

mod output;

pub use output::{
    HumanRenderer, JsonRenderer, OutputRenderer, colour_on_stderr, colour_on_stdout, render_json,
};

/// Global CLI flags shared across all commands.
#[derive(Debug, Parser)]
#[command(name = "wizlight", about = "Philips WiZ smart bulb control")]
#[command(arg_required_else_help = true)]
pub struct Cli {
    /// Emit JSON instead of human-readable output.
    #[arg(long, short = 'j', global = true, action = ArgAction::SetTrue)]
    pub json: bool,

    /// Per-request timeout in seconds.
    #[arg(long, global = true, default_value = "2", value_name = "SECONDS")]
    pub timeout: u64,

    /// Override the broadcast address to use during discovery.
    #[arg(long, global = true, value_name = "ADDR")]
    pub broadcast: Option<String>,

    /// Increase logging verbosity. Repeat the flag to add more detail.
    #[arg(long, short = 'v', global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// The selected subcommand.
    #[command(subcommand)]
    pub command: Command,
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
/// help text and the JSON contract can settle before commands start landing
/// against them.
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

/// Runs the selected command.
///
/// # Errors
///
/// Currently always: every command is still stubbed. Once they land this
/// returns whatever the operation failed with.
pub fn run_command(command: &Command) -> anyhow::Result<()> {
    anyhow::bail!(
        "`{}` is not implemented yet; this is the CLI scaffold",
        command.name()
    )
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
///
/// Exit codes are currently only success and failure. The distinct codes for
/// not-found and timeout wait for commands that can actually produce them; a
/// usage error is clap's own exit, before this is reached.
#[must_use]
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose, cli.json);

    // The globals are carried but not yet consumed: the commands that will
    // honour them do not exist. Logging them is how `--timeout` and
    // `--broadcast` can be checked in the meantime, and it gives `-v`
    // something to show while the library itself is uninstrumented.
    tracing::debug!(
        command = cli.command.name(),
        target = cli.command.target(),
        timeout_secs = cli.timeout,
        broadcast = cli.broadcast.as_deref(),
        json = cli.json,
        "parsed invocation"
    );

    match run_command(&cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!(
                "{}",
                render_failure(Some(&cli.command), &err.to_string(), cli.json)
            );
            ExitCode::FAILURE
        }
    }
}
