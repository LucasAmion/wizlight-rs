//! `wizlight discover` — who is out there.

use std::collections::HashSet;
use std::io;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::{Instant, timeout_at};

use super::{Outcome, target};
use crate::{Discovered, Discovery, SystemConfig};

/// Broadcasts for up to `wait` (stopping early once `expect` distinct bulbs
/// answer) and reports everything that answered, sorted by MAC.
///
/// `getSystemConfig` is asked of each bulb as it appears, which is what turns
/// a list of addresses into something a human can act on: the model is often
/// the only way to tell two bulbs apart when both are `192.168.0.something`.
///
/// A scan that finds nothing is a **success**. It answers the question that
/// was asked — nobody is out there — and a script reading the empty list
/// should not also have to special-case an exit code. Failing to *act* on a
/// bulb is the case that exits non-zero, and that belongs to the commands
/// that take a target.
///
/// # Errors
///
/// Only if the socket cannot be opened or the first broadcast cannot be sent;
/// see [`Discovery::stream`].
pub async fn run(
    discovery: &Discovery,
    wait: Duration,
    expect: Option<usize>,
) -> anyhow::Result<Outcome> {
    let found = target::scan(discovery, wait, expect).await?;
    tracing::info!(bulbs = found.len(), ?wait, ?expect, "scan finished");
    Ok(Outcome::new(payload(&found), listing(&found, wait)))
}

/// Streams each discovered bulb to `writer` as it answers, in arrival order.
///
/// Uses fixed-width MAC (12) and IP (15) columns in human mode so each line
/// can be written immediately without waiting for every row; model and
/// firmware stay ragged. Under `--json`, writes one NDJSON envelope per bulb.
///
/// Deduplicates on MAC so a bulb whose address changes mid-scan is reported
/// once and counted once against `expect`.
///
/// # Errors
///
/// Fails if discovery cannot start or writing to `writer` fails.
pub async fn run_stream<W: io::Write>(
    discovery: &Discovery,
    wait: Duration,
    expect: Option<usize>,
    json: bool,
    writer: &mut W,
) -> anyhow::Result<()> {
    let mut stream = discovery.stream().await?;
    let deadline = Instant::now() + wait;
    let mut seen = HashSet::new();

    while let Ok(Some(bulb)) = timeout_at(deadline, stream.recv()).await {
        if !seen.insert(bulb.mac.clone()) {
            continue;
        }
        let line = if json {
            stream_json(&bulb)
        } else {
            stream_human(&bulb)
        };
        writeln!(writer, "{line}")?;
        writer.flush()?;
        if expect.is_some_and(|n| seen.len() >= n) {
            break;
        }
    }

    tracing::info!(
        bulbs = seen.len(),
        ?wait,
        ?expect,
        "streaming scan finished"
    );
    if seen.is_empty() && !json {
        writeln!(writer, "no bulbs answered in {:.1}s", wait.as_secs_f32())?;
        writer.flush()?;
    }
    Ok(())
}

fn bulb_json(bulb: &Discovered) -> Value {
    let config = system_config(bulb);
    json!({
        "mac": bulb.mac,
        "ip": bulb.ip().to_string(),
        "port": bulb.addr.port(),
        "model": config.as_ref().and_then(|c| c.module_name.clone()),
        "firmware": config.as_ref().and_then(|c| c.fw_version.clone()),
    })
}

/// The `--json` payload: one object per bulb, sorted by MAC.
fn payload(found: &[Discovered]) -> Value {
    Value::Array(found.iter().map(bulb_json).collect())
}

fn stream_json(bulb: &Discovered) -> String {
    super::render_json(&json!({
        "ok": true,
        "command": "discover",
        "target": Value::Null,
        "result": bulb_json(bulb),
    }))
}

/// One streamed row: fixed-width MAC (12) and IPv4 (15), ragged model/firmware.
fn stream_human(bulb: &Discovered) -> String {
    let config = system_config(bulb);
    let model = field(config.as_ref().and_then(|c| c.module_name.as_deref()));
    let firmware = field(config.as_ref().and_then(|c| c.fw_version.as_deref()));
    format!("{:<12}  {:<15}  {model}  {firmware}", bulb.mac, bulb.ip())
}

/// One bulb per line, MAC first.
///
/// The MAC leads because it is the column worth copying: it is the stable
/// identity, and it is what `<target>` wants. Columns are padded to the widest
/// entry rather than to a fixed width, so a listing stays aligned without
/// spending half the terminal on a field nobody filled in.
fn listing(found: &[Discovered], wait: Duration) -> String {
    if found.is_empty() {
        return format!("no bulbs answered in {:.1}s", wait.as_secs_f32());
    }

    let rows: Vec<[String; 4]> = found
        .iter()
        .map(|bulb| {
            let config = system_config(bulb);
            [
                bulb.mac.clone(),
                bulb.ip().to_string(),
                field(config.as_ref().and_then(|c| c.module_name.as_deref())),
                field(config.as_ref().and_then(|c| c.fw_version.as_deref())),
            ]
        })
        .collect();

    let widths: [usize; 4] = std::array::from_fn(|column| {
        rows.iter()
            .map(|row| row[column].chars().count())
            .max()
            .unwrap_or_default()
    });

    rows.iter()
        .map(|row| {
            let line = row
                .iter()
                .zip(widths)
                .map(|(cell, width)| format!("{cell:<width$}"))
                .collect::<Vec<_>>()
                .join("  ");
            line.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A bulb that did not answer `getSystemConfig`, or answered it with something
/// unreadable, is still a bulb — it just has less to say about itself.
fn system_config(bulb: &Discovered) -> Option<SystemConfig> {
    bulb.system_config
        .as_ref()?
        .parse_result::<SystemConfig>()
        .ok()
}

fn field(value: Option<&str>) -> String {
    value.unwrap_or("-").to_owned()
}
