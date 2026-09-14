use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

use super::{Cli, Target, target};
use crate::{PUSH_PORT, Pilot, PushEvent, PushManager, PushSubscription};

const PILOT_FIELDS: &[&str] = &[
    "state", "r", "g", "b", "c", "w", "dimming", "temp", "sceneId", "speed", "ratio", "devices",
    "rssi", "src",
];

pub(super) async fn run(cli: &Cli, selection: &Target) -> anyhow::Result<()> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), PUSH_PORT);
    let manager = bind(addr).await?;
    let mut stdout = io::stdout().lock();
    run_with(
        cli,
        selection,
        manager,
        &mut stdout,
        tokio::signal::ctrl_c(),
    )
    .await
}

async fn bind(addr: SocketAddr) -> anyhow::Result<PushManager> {
    PushManager::bind_to(addr)
        .await
        .with_context(|| format!("cannot watch: UDP push listener {addr} is unavailable"))
}

async fn run_with<W, F>(
    cli: &Cli,
    selection: &Target,
    manager: PushManager,
    writer: &mut W,
    shutdown: F,
) -> anyhow::Result<()>
where
    W: io::Write,
    F: Future<Output = io::Result<()>>,
{
    let bulbs = resolve(cli, selection).await?;
    let subscriptions = subscribe(cli, &manager, bulbs).await?;
    let all = selection.all;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut workers = JoinSet::new();

    for (label, subscription) in subscriptions {
        workers.spawn(forward(
            label,
            subscription,
            event_tx.clone(),
            stop_rx.clone(),
        ));
    }
    drop(event_tx);

    tokio::pin!(shutdown);
    let mut previous = BTreeMap::new();
    let failure = loop {
        tokio::select! {
            signal = &mut shutdown => {
                break signal.err().map(|error| anyhow::anyhow!(error).context("could not listen for Ctrl-C"));
            }
            event = event_rx.recv() => {
                let Some((label, pilot)) = event else {
                    break Some(anyhow::anyhow!("the push listener stopped"));
                };
                let handled = (|| -> anyhow::Result<()> {
                    let timestamp = timestamp()?;
                    let line = if cli.json {
                        json_line(cli, all, &label, &timestamp, &pilot)?
                    } else {
                        human_line(all, &label, &timestamp, previous.get(&label), &pilot)?
                    };
                    writeln!(writer, "{line}")?;
                    writer.flush()?;
                    Ok(())
                })();
                if let Err(error) = handled {
                    break Some(error);
                }
                previous.insert(label, pilot);
            }
            joined = workers.join_next() => {
                let failure = match joined {
                    Some(Ok(Err(error))) => error,
                    Some(Err(error)) => error.into(),
                    Some(Ok(Ok(()))) | None => anyhow::anyhow!("the push listener stopped"),
                };
                break Some(failure);
            }
        }
    };

    let _ = stop_tx.send(true);
    while let Some(joined) = workers.join_next().await {
        match joined {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::warn!(%error, "push subscription stopped during cleanup"),
            Err(error) => tracing::warn!(%error, "push subscription task failed during cleanup"),
        }
    }

    failure.map_or(Ok(()), Err)
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

async fn subscribe(
    cli: &Cli,
    manager: &PushManager,
    bulbs: Vec<target::Resolved>,
) -> anyhow::Result<Vec<(String, PushSubscription)>> {
    let mut subscriptions = Vec::with_capacity(bulbs.len());
    for bulb in bulbs {
        let mac = match bulb.mac {
            Some(mac) => mac,
            None => bulb
                .connect(&cli.policy())
                .await?
                .get_pilot()
                .await?
                .mac
                .with_context(|| {
                    format!(
                        "{} did not report a MAC, so it cannot be watched",
                        bulb.addr
                    )
                })?,
        };
        match manager.subscribe(&mac, bulb.addr).await {
            Ok(subscription) => subscriptions.push((mac, subscription)),
            Err(error) => {
                cancel(subscriptions).await;
                return Err(error.into());
            }
        }
    }
    Ok(subscriptions)
}

async fn cancel(subscriptions: Vec<(String, PushSubscription)>) {
    for (mac, subscription) in subscriptions {
        if let Err(error) = subscription.cancel().await {
            tracing::warn!(%mac, %error, "could not unregister push subscription");
        }
    }
}

async fn forward(
    label: String,
    mut subscription: PushSubscription,
    events: mpsc::UnboundedSender<(String, Pilot)>,
    mut stop: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            event = subscription.recv() => match event {
                Some(PushEvent::SyncPilot { pilot, .. }) => {
                    if events.send((label.clone(), pilot)).is_err() {
                        break;
                    }
                }
                Some(PushEvent::FirstBeat { .. }) => {}
                None => anyhow::bail!("push subscription for {label} stopped"),
            }
        }
    }

    if let Err(error) = subscription.cancel().await {
        tracing::warn!(%label, %error, "could not unregister push subscription");
    }
    Ok(())
}

fn timestamp() -> anyhow::Result<String> {
    timestamp_at(SystemTime::now())
}

fn timestamp_at(now: SystemTime) -> anyhow::Result<String> {
    let elapsed = now
        .duration_since(UNIX_EPOCH)
        .context("the system clock is before the Unix epoch")?;
    let seconds = elapsed.as_secs();
    let days = (seconds / 86_400) as i64;
    let seconds_of_day = seconds % 86_400;
    let (year, month, day) = civil_date(days);
    let hour = seconds_of_day / 3_600;
    let minute = seconds_of_day % 3_600 / 60;
    let second = seconds_of_day % 60;
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{:03}Z",
        elapsed.subsec_millis()
    ))
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

fn json_line(
    cli: &Cli,
    all: bool,
    label: &str,
    timestamp: &str,
    pilot: &Pilot,
) -> anyhow::Result<String> {
    let mut result = pilot_map(pilot)?;
    result.insert("timestamp".into(), json!(timestamp));
    if all {
        result.insert("target".into(), json!(label));
    }
    serde_json::to_string(&json!({
        "ok": true,
        "command": "watch",
        "target": cli.command.target(),
        "result": result,
    }))
    .context("could not render a push update")
}

fn human_line(
    all: bool,
    label: &str,
    timestamp: &str,
    previous: Option<&Pilot>,
    pilot: &Pilot,
) -> anyhow::Result<String> {
    let current = pilot_map(pilot)?;
    let previous = previous.map(pilot_map).transpose()?;
    let changed: Vec<String> = PILOT_FIELDS
        .iter()
        .filter_map(|field| {
            let value = current.get(*field);
            let old = previous.as_ref().and_then(|pilot| pilot.get(*field));
            (previous.is_none() || value != old).then(|| human_field(field, value))
        })
        .collect();
    let mut parts = vec![timestamp.to_owned()];
    if all {
        parts.push(label.to_owned());
    }
    parts.push(if changed.is_empty() {
        "no changes".to_owned()
    } else {
        changed.join("  ")
    });
    Ok(parts.join("  "))
}

fn pilot_map(pilot: &Pilot) -> anyhow::Result<Map<String, Value>> {
    serde_json::to_value(pilot)
        .context("could not render a push update")?
        .as_object()
        .cloned()
        .context("a pilot update was not an object")
}

fn human_field(field: &str, value: Option<&Value>) -> String {
    let rendered = match value {
        Some(Value::Bool(value)) if field == "state" => {
            if *value { "on" } else { "off" }.to_owned()
        }
        Some(Value::String(value)) => value.clone(),
        Some(value) => value.to_string(),
        None => "-".to_owned(),
    };
    format!("{field}={rendered}")
}

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../tests/common/mock_bulb.rs"]
mod mock_bulb;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::Bulb;
    use clap::Parser as _;
    use tokio::net::UdpSocket;

    use super::*;

    use super::mock_bulb::MockBulb;

    fn loopback(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    fn selection(cli: &Cli) -> &Target {
        let super::super::Command::Watch(target) = &cli.command else {
            panic!("watch command")
        };
        target
    }

    async fn wait_for_registration(bulb: &MockBulb) {
        for _ in 0..100 {
            if bulb.push_target().is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("watch did not register: {:?}", bulb.requests());
    }

    async fn wait_for_unregistration(bulb: &MockBulb) {
        for _ in 0..100 {
            if bulb
                .last_request()
                .is_some_and(|request| request["params"]["register"] == false)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("watch did not unregister: {:?}", bulb.requests());
    }

    #[tokio::test]
    async fn mock_pushes_stream_as_ndjson_until_shutdown_unregisters() {
        let manager = PushManager::bind_to(loopback(0)).await.expect("bind push");
        let listen_addr = manager.local_addr();
        let bulb = MockBulb::builder()
            .push_port(listen_addr.port())
            .start()
            .await;
        let addr = bulb.addr().to_string();
        let cli = Cli::try_parse_from(["wizlight", "--json", "watch", &addr]).expect("parse");
        let shutdown = async {
            wait_for_registration(&bulb).await;
            let handle = Bulb::connect_to(bulb.addr()).await.expect("connect");
            handle
                .set_pilot(&crate::PilotBuilder::new().dimming(crate::Dimming::new(40).unwrap()))
                .await
                .expect("change state");
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(())
        };
        let mut output = Vec::new();

        run_with(&cli, selection(&cli), manager, &mut output, shutdown)
            .await
            .expect("watch succeeds");

        let output = String::from_utf8(output).expect("utf-8");
        let lines: Vec<Value> = output
            .lines()
            .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
            .collect();
        assert!(lines.len() >= 2, "{output}");
        assert!(lines.iter().all(|line| line["command"] == "watch"));
        assert!(lines.iter().all(|line| line["target"] == addr));
        assert!(lines.iter().any(|line| line["result"]["dimming"] == 40));
        assert!(
            lines
                .iter()
                .all(|line| line["result"]["timestamp"].is_string())
        );
        wait_for_unregistration(&bulb).await;
        assert_eq!(
            bulb.last_request().expect("unregistration")["params"]["register"],
            false
        );
        UdpSocket::bind(listen_addr)
            .await
            .expect("listener released after clean shutdown");
    }

    #[tokio::test]
    async fn human_mode_prints_only_fields_changed_after_the_first_update() {
        let manager = PushManager::bind_to(loopback(0)).await.expect("bind push");
        let bulb = MockBulb::builder()
            .push_port(manager.local_addr().port())
            .start()
            .await;
        let addr = bulb.addr().to_string();
        let cli = Cli::try_parse_from(["wizlight", "watch", &addr]).expect("parse");
        let shutdown = async {
            wait_for_registration(&bulb).await;
            let handle = Bulb::connect_to(bulb.addr()).await.expect("connect");
            handle
                .set_pilot(&crate::PilotBuilder::new().dimming(crate::Dimming::new(40).unwrap()))
                .await
                .expect("change state");
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(())
        };
        let mut output = Vec::new();

        run_with(&cli, selection(&cli), manager, &mut output, shutdown)
            .await
            .expect("watch succeeds");

        let output = String::from_utf8(output).expect("utf-8");
        let changed = output
            .lines()
            .find(|line| line.contains("dimming=40"))
            .unwrap_or_else(|| panic!("changed update missing: {output}"));
        assert!(changed.contains("src=udp"), "{changed}");
        assert!(!changed.contains("state="), "{changed}");
        assert!(!changed.contains("temp="), "{changed}");
    }

    #[tokio::test]
    async fn all_tags_each_human_update_with_its_mac() {
        let manager = PushManager::bind_to(loopback(0)).await.expect("bind push");
        let port = manager.local_addr().port();
        let first = MockBulb::builder()
            .mac("9877d5230f0a")
            .push_port(port)
            .start()
            .await;
        let second = MockBulb::builder()
            .mac("9877d523a4da")
            .push_port(port)
            .start()
            .await;
        let first_addr = first.addr().to_string();
        let second_addr = second.addr().to_string();
        let cli = Cli::try_parse_from([
            "wizlight",
            "--broadcast",
            &first_addr,
            "--broadcast",
            &second_addr,
            "--wait",
            "0.1",
            "watch",
            "--all",
        ])
        .expect("parse");
        let mut output = Vec::new();

        run_with(&cli, selection(&cli), manager, &mut output, async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(())
        })
        .await
        .expect("watch succeeds");

        let output = String::from_utf8(output).expect("utf-8");
        assert!(
            output.lines().any(|line| line.contains(first.mac())),
            "{output}"
        );
        assert!(
            output.lines().any(|line| line.contains(second.mac())),
            "{output}"
        );
    }

    #[test]
    fn timestamps_are_utc_rfc3339_across_a_leap_day() {
        assert_eq!(
            timestamp_at(UNIX_EPOCH).expect("epoch formats"),
            "1970-01-01T00:00:00.000Z"
        );
        assert_eq!(
            timestamp_at(UNIX_EPOCH + Duration::from_millis(951_782_400_123))
                .expect("leap day formats"),
            "2000-02-29T00:00:00.123Z"
        );
    }

    #[test]
    fn all_json_tags_each_update_inside_the_result() {
        let cli = Cli::try_parse_from(["wizlight", "--json", "watch", "--all"]).expect("parse");
        let pilot = Pilot {
            mac: Some("9877d5230f0a".to_owned()),
            state: Some(true),
            ..Pilot::default()
        };
        let line =
            json_line(&cli, true, "9877d5230f0a", "2026-09-14T12:00:00Z", &pilot).expect("render");
        let value: Value = serde_json::from_str(&line).expect("JSON");

        assert_eq!(value["target"], Value::Null);
        assert_eq!(value["result"]["target"], "9877d5230f0a");
        assert_eq!(value["result"]["state"], true);
    }

    #[tokio::test]
    async fn an_occupied_listener_address_is_reported_immediately() {
        let occupied = UdpSocket::bind(loopback(0)).await.expect("occupy address");
        let addr = occupied.local_addr().expect("address");
        let error = bind(addr).await.expect_err("must refuse occupied address");
        let message = error.to_string();
        assert!(message.contains("cannot watch"), "{message}");
        assert!(message.contains(&addr.to_string()), "{message}");
    }
}
