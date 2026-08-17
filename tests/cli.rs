#![cfg(feature = "cli")]

use std::process::{Command as Proc, Output};

use clap::{CommandFactory, Parser};
use serde_json::Value;
use wizlight::cli::{Cli, Command, Target, render_failure, run_command};

/// Runs the real binary, which is the only way to observe an exit code.
fn wizlight(args: &[&str]) -> Output {
    Proc::new(env!("CARGO_BIN_EXE_wizlight"))
        .args(args)
        .output()
        .expect("the binary was built by the test harness")
}

fn target(name: &str) -> Target {
    Target {
        target: name.to_owned(),
    }
}

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
    assert_eq!(cli.command, Command::Discover);
}

#[test]
fn a_target_is_parsed_and_reported_by_the_command() {
    let cli = Cli::try_parse_from(["wizlight", "status", "192.168.0.5"]).expect("status parses");
    assert_eq!(cli.command, Command::Status(target("192.168.0.5")));
    assert_eq!(cli.command.name(), "status");
    assert_eq!(cli.command.target(), Some("192.168.0.5"));
    assert_eq!(Command::Discover.target(), None);
}

#[test]
fn a_global_flag_is_accepted_after_the_subcommand() {
    let cli = Cli::try_parse_from(["wizlight", "status", "192.168.0.5", "--json"])
        .expect("global flags are global");
    assert!(cli.json);
}

#[test]
fn output_renderer_formats_typed_data_as_json() {
    let json = wizlight::cli::render_json(&serde_json::json!({"ok": true, "value": 3}));
    assert_eq!(json.trim(), "{\"ok\":true,\"value\":3}");
}

#[test]
fn a_human_failure_is_a_plain_line_not_json() {
    let rendered = render_failure(Some(&Command::Status(target("1.2.3.4"))), "boom", false);
    assert_eq!(rendered, "error: boom");
}

#[test]
fn a_json_failure_carries_the_command_and_target() {
    let rendered = render_failure(Some(&Command::Status(target("1.2.3.4"))), "boom", true);
    let value: Value = serde_json::from_str(&rendered).expect("valid JSON");
    assert_eq!(value["ok"], Value::Bool(false));
    assert_eq!(value["command"], "status");
    assert_eq!(value["target"], "1.2.3.4");
    assert_eq!(value["error"], "boom");

    // A command with no target still produces the key, as null.
    let rendered = render_failure(Some(&Command::Discover), "boom", true);
    let value: Value = serde_json::from_str(&rendered).expect("valid JSON");
    assert_eq!(value["command"], "discover");
    assert_eq!(value["target"], Value::Null);
}

#[test]
fn every_command_is_still_stubbed_and_names_itself() {
    let commands = [
        Command::Discover,
        Command::Status(target("x")),
        Command::Info(target("x")),
        Command::On(target("x")),
        Command::Off(target("x")),
        Command::Toggle(target("x")),
        Command::Set(target("x")),
        Command::Scenes(target("x")),
        Command::Watch(target("x")),
        Command::Bench(target("x")),
    ];

    for command in &commands {
        let err = run_command(command).expect_err("still stubbed");
        let message = err.to_string();
        assert!(message.contains("not implemented"), "{message}");
        assert!(message.contains(command.name()), "{message}");
    }
}

#[test]
fn a_stubbed_command_exits_non_zero() {
    let output = wizlight(&["status", "192.168.0.5"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "results go to stdout, errors do not"
    );

    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert!(stderr.starts_with("error: "), "{stderr}");
    assert!(stderr.contains("status"), "{stderr}");
}

#[test]
fn a_json_failure_goes_to_stderr_as_json_and_only_once() {
    let output = wizlight(&["--json", "status", "192.168.0.5"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "results go to stdout, errors do not"
    );

    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert_eq!(stderr.lines().count(), 1, "rendered twice: {stderr}");

    let value: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON under --json");
    assert_eq!(value["ok"], Value::Bool(false));
    assert_eq!(value["command"], "status");
    assert_eq!(value["target"], "192.168.0.5");
}

#[test]
fn no_subcommand_is_a_usage_error_not_a_success() {
    let output = wizlight(&[]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert!(stderr.contains("Usage:"), "{stderr}");
}

#[test]
fn an_unknown_subcommand_is_a_usage_error() {
    let output = wizlight(&["nosuchcommand"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn help_succeeds_and_is_printed_to_stdout() {
    for flag in ["--help", "-h"] {
        let output = wizlight(&[flag]);
        assert_eq!(output.status.code(), Some(0), "{flag}");
        let stdout = String::from_utf8(output.stdout).expect("utf-8");
        assert!(stdout.contains("Usage:"), "{flag}: {stdout}");
    }
}

#[test]
fn long_help_matches_short_help_and_leaks_no_rustdoc() {
    // clap promotes the `Cli` doc comment to the long description unless
    // `long_about = None` says otherwise, which had `--help` opening with
    // "Global CLI flags shared across all commands." while `-h` did not.
    let long = String::from_utf8(wizlight(&["--help"]).stdout).expect("utf-8");
    let short = String::from_utf8(wizlight(&["-h"]).stdout).expect("utf-8");

    assert_eq!(long, short);
    assert!(long.starts_with("Philips WiZ smart bulb control"), "{long}");
    assert!(!long.contains("Global CLI flags"), "{long}");
}

#[test]
fn version_reports_the_crate_version() {
    // Asserted against `CARGO_PKG_VERSION` rather than a literal, so the two
    // cannot drift apart on the next release.
    let expected = format!("wizlight {}", env!("CARGO_PKG_VERSION"));
    for flag in ["--version", "-V"] {
        let output = wizlight(&[flag]);
        assert_eq!(output.status.code(), Some(0), "{flag}");
        let stdout = String::from_utf8(output.stdout).expect("utf-8");
        assert_eq!(stdout.trim(), expected, "{flag}");
    }
}

#[test]
fn version_does_not_need_a_subcommand() {
    // `arg_required_else_help` must not turn `--version` into a usage error.
    assert_eq!(wizlight(&["--version"]).status.code(), Some(0));
}

#[test]
fn the_command_tree_is_internally_consistent() {
    // clap's own validation: duplicate short flags, conflicting names, bad arg
    // groups. Cheap insurance now that `-v` and `-V` sit next to each other.
    Cli::command().debug_assert();
}

#[test]
fn verbosity_controls_the_log_output() {
    let quiet = String::from_utf8(wizlight(&["discover"]).stderr).expect("utf-8");
    assert!(
        !quiet.contains("parsed invocation"),
        "logs at the default level: {quiet}"
    );

    let loud =
        String::from_utf8(wizlight(&["-vv", "--timeout", "9", "discover"]).stderr).expect("utf-8");
    assert!(loud.contains("parsed invocation"), "{loud}");
    assert!(loud.contains("timeout_secs=9"), "{loud}");
}

#[test]
fn rust_log_overrides_the_verbosity_flag() {
    let output = Proc::new(env!("CARGO_BIN_EXE_wizlight"))
        .args(["discover"])
        .env("RUST_LOG", "debug")
        .output()
        .expect("the binary was built by the test harness");

    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert!(stderr.contains("parsed invocation"), "{stderr}");
}

#[test]
fn logging_never_lands_on_stdout() {
    // Log output on stdout would corrupt `--json` for anything parsing it.
    let output = wizlight(&["-vvv", "--json", "discover"]);
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
}

#[test]
fn output_is_not_coloured_when_it_is_not_going_to_a_terminal() {
    // The test harness captures through a pipe, so nothing here is a TTY.
    // `tracing-subscriber` colours by default and would otherwise write escape
    // sequences into whatever the output was redirected to.
    let stderr = String::from_utf8(wizlight(&["-vv", "discover"]).stderr).expect("utf-8");
    assert!(stderr.contains("parsed invocation"), "{stderr}");
    assert!(
        !stderr.contains('\x1b'),
        "ANSI escape in a pipe: {stderr:?}"
    );
}

#[test]
fn no_color_suppresses_colour() {
    let output = Proc::new(env!("CARGO_BIN_EXE_wizlight"))
        .args(["-vv", "discover"])
        .env("NO_COLOR", "1")
        .output()
        .expect("the binary was built by the test harness");
    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert!(!stderr.contains('\x1b'), "{stderr:?}");
}
