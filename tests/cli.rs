#![cfg(feature = "cli")]

mod common;

use std::process::{Command as Proc, Output};
use std::time::Duration;

use clap::{CommandFactory, Parser};
use common::mock_bulb::{MockBulb, Personality};
use serde_json::Value;
use wizlight::cli::{Cli, Command, Target, render_failure, run_command};
use wizlight::{DEFAULT_WAIT, RetryPolicy};

/// Runs the real binary, which is the only way to observe an exit code.
fn wizlight(args: &[&str]) -> Output {
    Proc::new(env!("CARGO_BIN_EXE_wizlight"))
        .args(args)
        .output()
        .expect("the binary was built by the test harness")
}

/// Runs a command that would otherwise broadcast onto the real LAN.
///
/// Every scan in this file is aimed at loopback and given a short window, so
/// that the suite neither shouts at whatever network the machine is on nor
/// waits five seconds a time to hear nothing back.
fn scan(args: &[&str]) -> Output {
    scan_at("127.0.0.1", "0.05", args, &[])
}

/// [`scan`] with an environment variable set for the run.
fn scan_with_env(args: &[&str], key: &str, value: &str) -> Output {
    scan_at("127.0.0.1", "0.05", args, &[(key, value)])
}

fn scan_at(broadcast: &str, wait: &str, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut full = vec!["--broadcast", broadcast, "--wait", wait];
    full.extend_from_slice(args);
    let mut proc = Proc::new(env!("CARGO_BIN_EXE_wizlight"));
    proc.args(&full);
    for (key, value) in env {
        proc.env(key, value);
    }
    proc.output()
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
    assert_eq!(cli.timeout, Duration::from_secs(7));
    assert_eq!(
        cli.broadcast,
        ["192.168.1.255:38899".parse().expect("a socket address")]
    );
    assert_eq!(cli.command, Command::Discover);
}

#[test]
fn a_broadcast_address_may_omit_the_port_and_may_repeat() {
    // Bulbs are only ever on 38899, so requiring it would be ceremony; a
    // multi-homed host needs one address per subnet, so one is not enough.
    let cli = Cli::try_parse_from([
        "wizlight",
        "--broadcast",
        "192.168.0.255",
        "--broadcast",
        "10.0.0.255",
        "discover",
    ])
    .expect("both parse");

    let ports: Vec<u16> = cli.broadcast.iter().map(|addr| addr.port()).collect();
    assert_eq!(ports, [wizlight::PORT, wizlight::PORT]);
    let ips: Vec<String> = cli
        .broadcast
        .iter()
        .map(|addr| addr.ip().to_string())
        .collect();
    assert_eq!(ips, ["192.168.0.255", "10.0.0.255"]);
}

#[test]
fn durations_may_be_fractional_and_may_not_be_negative() {
    let cli = Cli::try_parse_from(["wizlight", "--wait", "0.25", "discover"]).expect("parses");
    assert_eq!(cli.wait, Duration::from_millis(250));

    // Rejected by the parser, so it is a usage error rather than a run that
    // fails much later for a reason nobody connects to the flag.
    assert!(Cli::try_parse_from(["wizlight", "--timeout", "-1", "discover"]).is_err());
    assert!(Cli::try_parse_from(["wizlight", "--wait", "soon", "discover"]).is_err());
}

#[test]
fn the_scan_window_defaults_to_the_measured_one() {
    // Asserted against the library constant rather than a literal: five
    // seconds is what a scan needs to stop coming back a bulb short, and the
    // flag's default must not drift away from it.
    let cli = Cli::try_parse_from(["wizlight", "discover"]).expect("parses");
    assert_eq!(cli.wait, DEFAULT_WAIT);
}

#[test]
fn the_cli_is_more_patient_than_the_library_default() {
    // The library's 500 ms is measured at close range; the same bulb across a
    // flat has a round trip past a second. Everything else about the policy is
    // the library's.
    let cli = Cli::try_parse_from(["wizlight", "discover"]).expect("parses");
    let policy = cli.policy();
    let default = RetryPolicy::default();

    assert_eq!(policy.attempt_timeout, Duration::from_secs(2));
    assert!(policy.attempt_timeout > default.attempt_timeout);
    assert_eq!(policy.attempts, default.attempts);
    assert_eq!(policy.min_interval, default.min_interval);
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

#[tokio::test]
async fn every_command_but_discover_is_still_stubbed_and_names_itself() {
    let stubbed = [
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

    for command in stubbed {
        let name = command.name();
        let cli = Cli::try_parse_from(["wizlight", name, "x"]).expect("parses");
        assert_eq!(cli.command, command);

        let err = run_command(&cli).await.expect_err("still stubbed");
        let message = err.to_string();
        assert!(message.contains("not implemented"), "{message}");
        assert!(message.contains(name), "{message}");
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

/// Long enough for a loopback registration and the `getSystemConfig` that
/// follows it, short enough that the suite stays quick.
const SCAN: &str = "0.4";

#[tokio::test(flavor = "multi_thread")]
async fn discover_lists_what_answered() {
    let bulb = MockBulb::builder().mac("9877d5230f0a").start().await;
    let addr = bulb.addr().to_string();

    let output = tokio::task::spawn_blocking(move || scan_at(&addr, SCAN, &["discover"], &[]))
        .await
        .expect("the scan ran");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let line = stdout.trim();

    // MAC first: it is the column worth copying, being the only identity that
    // survives a DHCP lease.
    assert!(line.starts_with("9877d5230f0a"), "{line}");
    assert!(line.contains("127.0.0.1"), "{line}");
    assert!(line.contains("ESP25_SHRGB_01"), "{line}");
    assert!(line.contains("1.38.0"), "{line}");
}

#[tokio::test(flavor = "multi_thread")]
async fn discover_json_carries_the_envelope_and_one_object_per_bulb() {
    let bulb = MockBulb::builder().mac("9877d5230f0a").start().await;
    let addr = bulb.addr().to_string();
    let port = bulb.port();

    let output =
        tokio::task::spawn_blocking(move || scan_at(&addr, SCAN, &["--json", "discover"], &[]))
            .await
            .expect("the scan ran");

    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let value: Value = serde_json::from_str(stdout.trim()).expect("stdout is JSON under --json");

    assert_eq!(value["ok"], Value::Bool(true));
    assert_eq!(value["command"], "discover");
    assert_eq!(value["target"], Value::Null);

    let bulbs = value["result"].as_array().expect("result is a list");
    assert_eq!(bulbs.len(), 1);
    assert_eq!(bulbs[0]["mac"], "9877d5230f0a");
    assert_eq!(bulbs[0]["ip"], "127.0.0.1");
    assert_eq!(bulbs[0]["port"], port);
    assert_eq!(bulbs[0]["model"], "ESP25_SHRGB_01");
    assert_eq!(bulbs[0]["firmware"], "1.38.0");
}

#[tokio::test(flavor = "multi_thread")]
async fn discover_finding_nothing_is_a_success_with_an_empty_list() {
    // A scan answers a question, and "nobody is out there" is an answer. It is
    // acting on a bulb that is not there that deserves a non-zero exit.
    let output = tokio::task::spawn_blocking(|| scan(&["--json", "discover"]))
        .await
        .expect("the scan ran");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let value: Value = serde_json::from_str(stdout.trim()).expect("JSON");
    assert_eq!(value["ok"], Value::Bool(true));
    assert_eq!(value["result"], serde_json::json!([]));

    let human = String::from_utf8(scan(&["discover"]).stdout).expect("utf-8");
    assert!(human.trim().starts_with("no bulbs answered"), "{human}");
}

#[tokio::test(flavor = "multi_thread")]
async fn discover_reports_a_bulb_that_will_not_describe_itself() {
    // `getSystemConfig` is a courtesy, not a condition of existing. Firmware
    // old enough to lack it answers the registration perfectly well, and a
    // listing that hid those bulbs would be worse than one with gaps in it.
    let bulb = MockBulb::builder()
        .personality(Personality::rgb().with_system_config(
            r#"{"env":"pro","error":{"code":-32601,"message":"Method not found"}}"#,
        ))
        .mac("9877d523a4da")
        .start()
        .await;
    let addr = bulb.addr().to_string();

    let output =
        tokio::task::spawn_blocking(move || scan_at(&addr, SCAN, &["--json", "discover"], &[]))
            .await
            .expect("the scan ran");

    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    let value: Value = serde_json::from_str(stdout.trim()).expect("JSON");
    let bulbs = value["result"].as_array().expect("result is a list");
    assert_eq!(bulbs.len(), 1, "{value}");
    assert_eq!(bulbs[0]["mac"], "9877d523a4da");
    assert_eq!(bulbs[0]["model"], Value::Null);
    assert_eq!(bulbs[0]["firmware"], Value::Null);

    // The human column is a dash rather than the word "null".
    let addr = bulb.addr().to_string();
    let human = String::from_utf8(
        tokio::task::spawn_blocking(move || scan_at(&addr, SCAN, &["discover"], &[]))
            .await
            .expect("the scan ran")
            .stdout,
    )
    .expect("utf-8");
    assert_eq!(human.trim(), "9877d523a4da  127.0.0.1  -  -");
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
fn help_leaks_no_rustdoc_at_either_length() {
    // clap promotes the `Cli` doc comment to the long description unless
    // `long_about = None` says otherwise, which had `--help` opening with
    // "Global CLI flags shared across all commands." while `-h` did not.
    //
    // This asserted `long == short` while every flag's help was one line. It
    // no longer is — `--broadcast` needs a second paragraph to explain the
    // multi-homed case, and telling only `--help` about it is exactly what the
    // two lengths are for. So the invariant is what it always meant: the same
    // opening, and no internal notes in either.
    let long = String::from_utf8(wizlight(&["--help"]).stdout).expect("utf-8");
    let short = String::from_utf8(wizlight(&["-h"]).stdout).expect("utf-8");

    for help in [&long, &short] {
        assert!(help.starts_with("Philips WiZ smart bulb control"), "{help}");
        assert!(!help.contains("Global CLI flags"), "{help}");
        // Every subcommand is listed at both lengths.
        for command in ["discover", "status", "info", "on", "off", "toggle", "set"] {
            assert!(help.contains(command), "{command} missing from {help}");
        }
    }

    // The long form is the one that elaborates, and only it.
    assert!(
        long.contains("multi-homed") || long.contains("several networks"),
        "{long}"
    );
    assert!(short.len() < long.len(), "-h should be the summary");
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
    let quiet = String::from_utf8(scan(&["discover"]).stderr).expect("utf-8");
    assert!(
        !quiet.contains("parsed invocation"),
        "logs at the default level: {quiet}"
    );

    let loud =
        String::from_utf8(scan(&["-vv", "--timeout", "9", "discover"]).stderr).expect("utf-8");
    assert!(loud.contains("parsed invocation"), "{loud}");
    assert!(loud.contains("timeout=9s"), "{loud}");
}

#[test]
fn rust_log_overrides_the_verbosity_flag() {
    let output = scan_with_env(&["discover"], "RUST_LOG", "debug");

    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert!(stderr.contains("parsed invocation"), "{stderr}");
}

#[test]
fn logging_never_lands_on_stdout() {
    // Log output on stdout would corrupt `--json` for anything parsing it.
    let stdout = String::from_utf8(scan(&["-vvv", "--json", "discover"]).stdout).expect("utf-8");
    let value: Value = serde_json::from_str(stdout.trim()).expect("stdout is only the payload");
    assert_eq!(value["command"], "discover");
}

#[test]
fn output_is_not_coloured_when_it_is_not_going_to_a_terminal() {
    // The test harness captures through a pipe, so nothing here is a TTY.
    // `tracing-subscriber` colours by default and would otherwise write escape
    // sequences into whatever the output was redirected to.
    let stderr = String::from_utf8(scan(&["-vv", "discover"]).stderr).expect("utf-8");
    assert!(stderr.contains("parsed invocation"), "{stderr}");
    assert!(
        !stderr.contains('\x1b'),
        "ANSI escape in a pipe: {stderr:?}"
    );
}

#[test]
fn no_color_suppresses_colour() {
    let output = scan_with_env(&["-vv", "discover"], "NO_COLOR", "1");
    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert!(!stderr.contains('\x1b'), "{stderr:?}");
}
