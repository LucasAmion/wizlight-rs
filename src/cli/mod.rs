//! The `wizlight` command-line interface (feature `cli`, on by default).
//!
//! The CLI lives inside the library rather than in `src/bin/` so that argument
//! parsing and output rendering can be exercised from the test suite.
//! `src/main.rs` is a wrapper around [`run`].
//!
//! This is the CLI scaffold: the command tree, global flags and renderers are in
//! place, while the actual bulb operations are still intentionally stubbed as
//! "not implemented yet" until the transport wiring is added.

use clap::{ArgAction, CommandFactory, Parser, Subcommand};
use serde_json::json;

mod output;

pub use output::{HumanRenderer, JsonRenderer, OutputRenderer, color_disabled, render_json};

/// Global CLI flags shared across all commands.
#[derive(Debug, Parser)]
#[command(name = "wizlight", about = "Philips WiZ smart bulb control")]
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
    pub command: Option<Command>,
}

/// Supported commands. The actual protocol commands are added as the crate grows;
/// the tree is intentionally complete enough for the CLI skeleton to be
/// exercised and kept stable.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Discover bulbs on the current LAN.
    Discover,
    /// Print the current pilot state for a target bulb.
    Status {
        /// The target bulb, as an IP address or MAC.
        target: String,
    },
    /// Print model info and supported capabilities for a target bulb.
    Info {
        /// The target bulb, as an IP address or MAC.
        target: String,
    },
    /// Power a bulb on.
    On {
        /// The target bulb, as an IP address or MAC.
        target: String,
    },
    /// Power a bulb off.
    Off {
        /// The target bulb, as an IP address or MAC.
        target: String,
    },
    /// Toggle the power state of a bulb.
    Toggle {
        /// The target bulb, as an IP address or MAC.
        target: String,
    },
    /// Change a bulb's state.
    Set {
        /// The target bulb, as an IP address or MAC.
        target: String,
    },
    /// List the scenes supported by the target bulb.
    Scenes {
        /// The target bulb, as an IP address or MAC.
        target: String,
    },
    /// Tail `syncPilot` push updates from a target bulb.
    Watch {
        /// The target bulb, as an IP address or MAC.
        target: String,
    },
    /// Benchmark the bulb update rate and latency.
    Bench {
        /// The target bulb, as an IP address or MAC.
        target: String,
    },
}

/// Parses the command line, runs the requested command and renders its output.
pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if cli.command.is_none() {
        let mut cmd = Cli::command();
        cmd.print_help()?;
        return Ok(());
    }

    let payload = match cli.command.unwrap() {
        Command::Discover => json!({
            "ok": false,
            "command": "discover",
            "message": "discover is not implemented yet; this is the CLI scaffold"
        }),
        Command::Status { target } => json!({
            "ok": false,
            "command": "status",
            "target": target,
            "message": "status is not implemented yet; this is the CLI scaffold"
        }),
        Command::Info { target } => json!({
            "ok": false,
            "command": "info",
            "target": target,
            "message": "info is not implemented yet; this is the CLI scaffold"
        }),
        Command::On { target } => json!({
            "ok": false,
            "command": "on",
            "target": target,
            "message": "on is not implemented yet; this is the CLI scaffold"
        }),
        Command::Off { target } => json!({
            "ok": false,
            "command": "off",
            "target": target,
            "message": "off is not implemented yet; this is the CLI scaffold"
        }),
        Command::Toggle { target } => json!({
            "ok": false,
            "command": "toggle",
            "target": target,
            "message": "toggle is not implemented yet; this is the CLI scaffold"
        }),
        Command::Set { target } => json!({
            "ok": false,
            "command": "set",
            "target": target,
            "message": "set is not implemented yet; this is the CLI scaffold"
        }),
        Command::Scenes { target } => json!({
            "ok": false,
            "command": "scenes",
            "target": target,
            "message": "scenes is not implemented yet; this is the CLI scaffold"
        }),
        Command::Watch { target } => json!({
            "ok": false,
            "command": "watch",
            "target": target,
            "message": "watch is not implemented yet; this is the CLI scaffold"
        }),
        Command::Bench { target } => json!({
            "ok": false,
            "command": "bench",
            "target": target,
            "message": "bench is not implemented yet; this is the CLI scaffold"
        }),
    };

    if cli.json {
        println!("{}", render_json(&payload));
    } else {
        println!("{}", HumanRenderer.render(&payload));
    }

    Ok(())
}
