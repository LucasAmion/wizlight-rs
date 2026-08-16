#![cfg(feature = "cli")]

use clap::Parser;
use wizlight::cli::{Cli, Command, run_command};

#[test]
fn global_flags_and_subcommand_are_parsed() {
    let cli = Cli::try_parse_from([
        "wizlight",
        "--json",
        "--timeout",
        "7",
        "--broadcast",
        "192.168.1.255:38899",
        "discover",
    ])
    .expect("discover should parse");

    assert!(cli.json);
    assert_eq!(cli.timeout, 7);
    assert_eq!(cli.broadcast.as_deref(), Some("192.168.1.255:38899"));
    assert!(matches!(cli.command, Some(Command::Discover)));
}

#[test]
fn output_renderer_formats_typed_data_as_json() {
    let json = wizlight::cli::render_json(&serde_json::json!({"ok": true, "value": 3}));
    assert_eq!(json.trim(), "{\"ok\":true,\"value\":3}");
}

#[test]
fn stubbed_commands_fail_with_a_non_zero_exit() {
    let err = run_command(Command::Discover, true).expect_err("discover should fail while stubbed");
    let message = err.to_string();
    assert!(message.contains("not implemented"));
}
