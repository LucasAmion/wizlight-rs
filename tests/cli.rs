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
        target: Some(name.parse().expect("a valid target")),
        all: false,
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
async fn the_push_commands_are_still_stubbed_and_name_themselves() {
    // `watch` and `bench` wait on the library's push listener and its
    // rate-limited write path; neither exists yet.
    let stubbed = [
        Command::Watch(target("9877d5230f0a")),
        Command::Bench(target("9877d5230f0a")),
    ];

    for command in stubbed {
        let name = command.name();
        let cli = Cli::try_parse_from(["wizlight", name, "9877d5230f0a"]).expect("parses");
        assert_eq!(cli.command, command);

        let err = run_command(&cli).await.expect_err("still stubbed");
        let message = err.to_string();
        assert!(message.contains("not implemented"), "{message}");
        assert!(message.contains(name), "{message}");
    }
}

/// A bulb that is there and will not answer, so a command times out rather
/// than failing to find anything. A closed port would be quicker, but Windows
/// turns the resulting ICMP into a socket error and the failure would no
/// longer be the one being tested.
async fn deaf_bulb() -> MockBulb {
    let bulb = MockBulb::start().await;
    bulb.drop_next(usize::MAX);
    bulb
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failure_goes_to_stderr_and_stdout_stays_empty() {
    let bulb = deaf_bulb().await;
    let addr = bulb.addr().to_string();

    let output =
        tokio::task::spawn_blocking(move || wizlight(&["--timeout", "0.05", "status", &addr]))
            .await
            .expect("the command ran");

    assert_eq!(output.status.code(), Some(4), "a timeout has its own code");
    assert!(
        output.stdout.is_empty(),
        "results go to stdout, errors do not"
    );

    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert!(stderr.starts_with("error: "), "{stderr}");
    assert!(stderr.contains("getPilot"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_json_failure_goes_to_stderr_as_json_and_only_once() {
    let bulb = deaf_bulb().await;
    let addr = bulb.addr().to_string();
    let target = addr.clone();

    let output = tokio::task::spawn_blocking(move || {
        wizlight(&["--json", "--timeout", "0.05", "status", &addr])
    })
    .await
    .expect("the command ran");

    assert_eq!(output.status.code(), Some(4));
    assert!(
        output.stdout.is_empty(),
        "results go to stdout, errors do not"
    );

    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert_eq!(stderr.lines().count(), 1, "rendered twice: {stderr}");

    let value: Value = serde_json::from_str(stderr.trim()).expect("stderr is JSON under --json");
    assert_eq!(value["ok"], Value::Bool(false));
    assert_eq!(value["command"], "status");
    assert_eq!(value["target"], target);
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

/// Runs a command against a bulb, addressing it by whatever `target` says.
///
/// `--broadcast` points any scan at the mock rather than at the LAN, and the
/// windows are short because everything here is loopback.
async fn against(bulb: &MockBulb, target: &str, args: &[&str]) -> Output {
    let addr = bulb.addr().to_string();
    let target = target.to_owned();
    let mut owned: Vec<String> = vec![
        "--broadcast".into(),
        addr,
        "--wait".into(),
        SCAN.into(),
        "--timeout".into(),
        "0.5".into(),
    ];
    owned.extend(args.iter().map(|arg| (*arg).to_string()));
    owned.push(target);

    tokio::task::spawn_blocking(move || {
        let borrowed: Vec<&str> = owned.iter().map(String::as_str).collect();
        wizlight(&borrowed)
    })
    .await
    .expect("the command ran")
}

/// Lets a bulb answer the scan, then stops it answering anything else.
async fn goes_quiet_once_scanned(bulb: &MockBulb) {
    for _ in 0..400 {
        if !bulb.requests().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    bulb.drop_next(usize::MAX);
}

fn stdout_json(output: &Output) -> Value {
    let stdout = std::str::from_utf8(&output.stdout).expect("utf-8");
    serde_json::from_str(stdout.trim()).unwrap_or_else(|err| panic!("{err}: {stdout}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn status_reports_what_the_bulb_says_it_is_doing() {
    let bulb = MockBulb::builder()
        .mac("9877d5230f0a")
        .pilot(serde_json::json!({
            "state": true, "r": 255, "g": 80, "b": 0, "dimming": 40, "sceneId": 0, "rssi": -52
        }))
        .start()
        .await;
    let addr = bulb.addr().to_string();

    let human = against(&bulb, &addr, &["status"]).await;
    assert_eq!(human.status.code(), Some(0));
    let line = String::from_utf8(human.stdout).expect("utf-8");
    assert_eq!(line.trim(), "on  rgb 255,80,0  40%  -52 dBm");

    let json = stdout_json(&against(&bulb, &addr, &["--json", "status"]).await);
    assert_eq!(json["ok"], Value::Bool(true));
    assert_eq!(json["command"], "status");
    assert_eq!(json["result"]["state"], Value::Bool(true));
    assert_eq!(json["result"]["r"], 255);
    assert_eq!(json["result"]["dimming"], 40);
}

#[tokio::test(flavor = "multi_thread")]
async fn status_names_a_running_scene() {
    let bulb = MockBulb::builder()
        .pilot(serde_json::json!({"state": true, "sceneId": 4, "speed": 100, "dimming": 100}))
        .start()
        .await;
    let addr = bulb.addr().to_string();

    let human = String::from_utf8(against(&bulb, &addr, &["status"]).await.stdout).expect("utf-8");
    assert!(human.contains("scene Party (4)"), "{human}");

    let json = stdout_json(&against(&bulb, &addr, &["--json", "status"]).await);
    assert_eq!(json["result"]["sceneId"], 4);
    assert_eq!(json["result"]["scene"], "Party");
}

#[tokio::test(flavor = "multi_thread")]
async fn info_describes_the_model_and_what_it_can_do() {
    let bulb = MockBulb::start().await;
    let addr = bulb.addr().to_string();

    let human = String::from_utf8(against(&bulb, &addr, &["info"]).await.stdout).expect("utf-8");
    assert!(human.contains("ESP25_SHRGB_01"), "{human}");
    assert!(human.contains("1.38.0"), "{human}");
    assert!(human.contains("2200-6500 K"), "{human}");
    assert!(human.contains("colour"), "{human}");

    let json = stdout_json(&against(&bulb, &addr, &["--json", "info"]).await);
    assert_eq!(json["result"]["class"], "RGB");
    assert_eq!(json["result"]["features"]["color"], Value::Bool(true));
    assert_eq!(json["result"]["kelvin_range"]["min"], 2200);
}

#[tokio::test(flavor = "multi_thread")]
async fn scenes_lists_only_what_that_class_plays() {
    // A tunable white bulb has no colour, so the colour-only scenes are not
    // offered to it. Printing the whole table would be the easy thing to do
    // and would be wrong.
    let colour = MockBulb::start().await;
    let white = MockBulb::builder()
        .personality(Personality::tunable_white())
        .start()
        .await;
    let colour_addr = colour.addr().to_string();
    let white_addr = white.addr().to_string();

    let listed =
        String::from_utf8(against(&colour, &colour_addr, &["scenes"]).await.stdout).expect("utf-8");
    assert!(listed.contains("Party"), "{listed}");

    let listed =
        String::from_utf8(against(&white, &white_addr, &["scenes"]).await.stdout).expect("utf-8");
    assert!(!listed.contains("Party"), "{listed}");
    assert!(listed.contains("Cozy"), "{listed}");

    let json = stdout_json(&against(&white, &white_addr, &["--json", "scenes"]).await);
    let scenes = json["result"].as_array().expect("a list");
    assert!(scenes.iter().all(|scene| scene["name"] != "Party"));
}

#[tokio::test(flavor = "multi_thread")]
async fn on_sends_the_colour_it_was_given() {
    let bulb = MockBulb::start().await;
    let addr = bulb.addr().to_string();

    let output = against(&bulb, &addr, &["on", "--rgb", "255,80,0", "-b", "40"]).await;
    assert_eq!(output.status.code(), Some(0));

    let request = bulb.last_request().expect("a write arrived");
    assert_eq!(request["method"], "setPilot");
    assert_eq!(request["params"]["state"], Value::Bool(true));
    assert_eq!(request["params"]["r"], 255);
    assert_eq!(request["params"]["g"], 80);
    assert_eq!(request["params"]["b"], 0);
    assert_eq!(request["params"]["dimming"], 40);
}

#[tokio::test(flavor = "multi_thread")]
async fn hsv_is_converted_because_the_protocol_has_none() {
    let bulb = MockBulb::start().await;
    let addr = bulb.addr().to_string();

    // Pure red at full saturation and value.
    against(&bulb, &addr, &["on", "--hsv", "0,100,100"]).await;
    let request = bulb.last_request().expect("a write arrived");
    assert_eq!(request["params"]["r"], 255);
    assert_eq!(request["params"]["g"], 0);
    assert_eq!(request["params"]["b"], 0);

    // Half value halves the channels; it is not the bulb's own dimming, which
    // is not sent at all here.
    against(&bulb, &addr, &["on", "--hsv", "120,100,50"]).await;
    let request = bulb.last_request().expect("a write arrived");
    assert_eq!(request["params"]["g"], 128);
    assert_eq!(request["params"]["dimming"], Value::Null);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scene_is_named_or_numbered_and_matched_leniently() {
    let bulb = MockBulb::start().await;
    let addr = bulb.addr().to_string();

    for spelling in ["Deep dive", "deep-dive", "DEEPDIVE", "23"] {
        against(&bulb, &addr, &["on", "--scene", spelling]).await;
        let request = bulb.last_request().expect("a write arrived");
        assert_eq!(request["params"]["sceneId"], 23, "{spelling}");
    }

    // An id the bulb accepts and then does something else with is refused
    // before it is sent: 41 plays at a third of normal brightness.
    let output = against(&bulb, &addr, &["on", "--scene", "41"]).await;
    assert_eq!(
        output.status.code(),
        Some(2),
        "a bad value is a usage error"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn off_and_toggle_write_the_power_state() {
    let bulb = MockBulb::builder()
        .pilot(serde_json::json!({"state": true, "dimming": 100}))
        .start()
        .await;
    let addr = bulb.addr().to_string();

    against(&bulb, &addr, &["off"]).await;
    assert_eq!(
        bulb.last_request().expect("a write")["params"]["state"],
        Value::Bool(false)
    );

    // The bulb is now off, so a toggle turns it back on.
    let output = against(&bulb, &addr, &["toggle"]).await;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        bulb.last_request().expect("a write")["params"]["state"],
        Value::Bool(true)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("utf-8").trim(),
        "on"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn set_uses_set_state_and_needs_something_to_set() {
    let bulb = MockBulb::start().await;
    let addr = bulb.addr().to_string();

    against(&bulb, &addr, &["set", "--kelvin", "2700"]).await;
    let request = bulb.last_request().expect("a write arrived");
    assert_eq!(request["method"], "setState");
    assert_eq!(request["params"]["temp"], 2700);

    // `on` is complete on its own and `set` is not, which no single clap group
    // can express — but it is still a usage error, and exits like one.
    let output = against(&bulb, &addr, &["set"]).await;
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert!(stderr.contains("needs something to set"), "{stderr}");
    assert!(stderr.contains("Usage:"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_ways_of_saying_a_colour_is_a_usage_error() {
    // clap rejects it before anything is sent. `PilotBuilder` refuses the same
    // pair, but only once the command is running and with no flag to name.
    let bulb = MockBulb::start().await;
    let addr = bulb.addr().to_string();

    for pair in [
        vec!["--rgb", "255,0,0", "--kelvin", "2700"],
        vec!["--rgb", "255,0,0", "--hsv", "0,100,100"],
        vec!["--scene", "Party", "--kelvin", "2700"],
    ] {
        let mut args = vec!["on"];
        args.extend(pair.iter().copied());
        let output = against(&bulb, &addr, &args).await;
        assert_eq!(output.status.code(), Some(2), "{pair:?}");
        assert!(bulb.requests().is_empty(), "{pair:?} reached the bulb");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn asking_a_bulb_for_what_it_has_no_hardware_for_names_its_class() {
    // The bulb will not refuse this itself — measured: it answers `success`
    // for parameters it has nothing to apply to.
    let bulb = MockBulb::builder()
        .personality(Personality::dimmable_white())
        .start()
        .await;
    let addr = bulb.addr().to_string();

    let output = against(&bulb, &addr, &["on", "--rgb", "255,0,0"]).await;
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert!(stderr.contains("ESP06_SHDW9_01"), "{stderr}");
    assert!(stderr.contains("Dimmable White"), "{stderr}");
    assert!(stderr.contains("cannot show a colour"), "{stderr}");

    // And it was refused before any write went out.
    assert!(
        bulb.requests().iter().all(|raw| !raw.contains("setPilot")),
        "{:?}",
        bulb.requests()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_target_may_be_a_mac_in_any_spelling() {
    let bulb = MockBulb::builder().mac("9877d5230f0a").start().await;

    for spelling in ["9877d5230f0a", "98:77:d5:23:0f:0a", "9877D5230F0A"] {
        let output = against(&bulb, spelling, &["--json", "status"]).await;
        assert_eq!(output.status.code(), Some(0), "{spelling}");
        // The envelope echoes the argument as it was typed.
        assert_eq!(stdout_json(&output)["target"], spelling);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mac_nothing_answers_to_is_not_found_rather_than_a_failure() {
    let bulb = MockBulb::builder().mac("9877d5230f0a").start().await;

    let output = against(&bulb, "aabbccddeeff", &["status"]).await;
    assert_eq!(output.status.code(), Some(3), "not found has its own code");
    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert!(stderr.contains("aabbccddeeff"), "{stderr}");
    assert!(stderr.contains("powered off"), "{stderr}");
}

#[test]
fn a_target_that_is_neither_an_address_nor_a_mac_is_a_usage_error() {
    let output = wizlight(&["status", "kitchen"]);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("utf-8");
    assert!(
        stderr.contains("neither an IP address nor a MAC"),
        "{stderr}"
    );
}

#[test]
fn a_command_needs_a_target_or_all_and_not_both() {
    assert_eq!(wizlight(&["status"]).status.code(), Some(2));
    assert_eq!(
        wizlight(&["status", "--all", "9877d5230f0a"]).status.code(),
        Some(2)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn all_fans_out_and_one_failure_does_not_hide_the_others() {
    let working = MockBulb::builder()
        .mac("9877d5230f0a")
        .pilot(serde_json::json!({"state": true, "dimming": 100}))
        .start()
        .await;
    let deaf = MockBulb::builder().mac("9877d523a4da").start().await;
    let addrs = [working.addr().to_string(), deaf.addr().to_string()];

    let output = tokio::task::spawn_blocking(move || {
        wizlight(&[
            "--json",
            "--broadcast",
            &addrs[0],
            "--broadcast",
            &addrs[1],
            "--wait",
            SCAN,
            "--timeout",
            "0.05",
            "status",
            "--all",
        ])
    });
    // It answers the scan and then goes quiet, so it is found and then
    // unreachable — waited for rather than timed, because a cold debug binary
    // can take longer to start than any sleep worth writing.
    goes_quiet_once_scanned(&deaf).await;
    let output = output.await.expect("the command ran");

    assert_eq!(output.status.code(), Some(1), "a partial failure is not 0");
    let json = stdout_json(&output);
    assert_eq!(json["ok"], Value::Bool(false));
    assert_eq!(json["target"], Value::Null);

    let results = json["result"].as_array().expect("one entry per bulb");
    assert_eq!(results.len(), 2);
    // Sorted by MAC, so a fan-out can be diffed between runs.
    assert_eq!(results[0]["target"], "9877d5230f0a");
    assert_eq!(results[0]["ok"], Value::Bool(true));
    assert_eq!(results[0]["result"]["state"], Value::Bool(true));
    assert_eq!(results[1]["target"], "9877d523a4da");
    assert_eq!(results[1]["ok"], Value::Bool(false));
    assert!(
        results[1]["error"]
            .as_str()
            .expect("a message")
            .contains("no reply"),
        "{}",
        results[1]["error"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fan_out_labels_every_line_with_the_bulb_it_came_from() {
    let one = MockBulb::builder()
        .mac("9877d5230f0a")
        .pilot(serde_json::json!({"state": true, "dimming": 100}))
        .start()
        .await;
    let two = MockBulb::builder()
        .mac("9877d523a4da")
        .pilot(serde_json::json!({"state": false, "dimming": 40}))
        .start()
        .await;
    let addrs = [one.addr().to_string(), two.addr().to_string()];

    let output = tokio::task::spawn_blocking(move || {
        wizlight(&[
            "--broadcast",
            &addrs[0],
            "--broadcast",
            &addrs[1],
            "--wait",
            SCAN,
            "status",
            "--all",
        ])
    })
    .await
    .expect("the command ran");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).expect("utf-8");
    // One line per bulb, MAC first, so the output stays greppable.
    assert_eq!(
        stdout.trim(),
        "9877d5230f0a  on  100%\n9877d523a4da  off  40%"
    );
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
