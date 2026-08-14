//! Discovery against mock bulbs.
//!
//! The mocks bind ephemeral ports, so instead of broadcasting these tests name
//! each one as a target. That exercises everything except the broadcast address
//! itself, which nothing but hardware can confirm.

mod common;

use std::time::Duration;

use common::mock_bulb::MockBulb;
use common::udp::Responder;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::time::timeout;
use wizlight::{BROADCAST, DEFAULT_INTERVAL, Discovery, PORT, RetryPolicy};

/// Long enough for a loopback round trip on a loaded CI runner, short enough
/// that a suite of these stays quick.
const WAIT: Duration = Duration::from_millis(400);
/// Fast enough to get several broadcasts inside `WAIT`.
const INTERVAL: Duration = Duration::from_millis(40);

fn discovery() -> Discovery {
    Discovery::new().interval(INTERVAL)
}

#[tokio::test]
async fn finds_every_bulb_that_answers() {
    let one = MockBulb::builder().mac("9877d5230f0a").start().await;
    let two = MockBulb::builder().mac("9877d523a4da").start().await;

    let found = discovery()
        .target(one.addr())
        .target(two.addr())
        .collect(WAIT)
        .await
        .expect("discover");

    let mut macs: Vec<&str> = found.iter().map(|bulb| bulb.mac.as_str()).collect();
    macs.sort_unstable();
    assert_eq!(macs, ["9877d5230f0a", "9877d523a4da"]);
    let addrs: Vec<_> = found.iter().map(|bulb| bulb.addr).collect();
    assert!(addrs.contains(&one.addr()) && addrs.contains(&two.addr()));
}

#[tokio::test]
async fn sends_the_registration_the_app_sends() {
    let bulb = MockBulb::start().await;

    discovery()
        .target(bulb.addr())
        .collect(WAIT)
        .await
        .expect("discover");

    let request = bulb.last_request().expect("a registration arrived");
    assert_eq!(request["method"], "registration");
    let params = &request["params"];
    assert_eq!(params["phoneMac"], "AAAAAAAAAAAA");
    assert_eq!(params["id"], "1");
    // The one that matters: `true` would start a push stream at a bulb nobody
    // asked to hear from, and discovery is a read.
    assert_eq!(params["register"], false);
    // Sent for the bulb's benefit, and unused while `register` is false — but
    // it must at least be an address.
    assert!(
        params["phoneIp"]
            .as_str()
            .expect("phoneIp")
            .parse::<std::net::IpAddr>()
            .is_ok(),
        "phoneIp was {}",
        params["phoneIp"]
    );
}

#[tokio::test]
async fn a_bulb_answering_every_broadcast_is_reported_once() {
    let bulb = MockBulb::start().await;

    let found = discovery()
        .target(bulb.addr())
        .collect(WAIT)
        .await
        .expect("discover");

    assert_eq!(found.len(), 1, "same bulb reported more than once");
    assert!(
        bulb.requests().len() > 1,
        "the test proved nothing: only one broadcast went out"
    );
}

#[tokio::test]
async fn something_that_is_not_a_bulb_is_ignored() {
    // A reply in the right shape, from a device with no MAC to give. Only
    // `result.mac` makes something a WiZ bulb.
    let impostor =
        Responder::replying_with(r#"{"method":"registration","env":"pro","result":{"ok":true}}"#)
            .await;
    let noise = Responder::replying_with(r#"{"hello":"world"}"#).await;
    let error = Responder::replying_with(
        r#"{"method":"registration","env":"pro","error":{"code":-32601,"message":"Method not found"}}"#,
    )
    .await;

    let found = discovery()
        .target(impostor.addr())
        .target(noise.addr())
        .target(error.addr())
        .collect(WAIT)
        .await
        .expect("discover");

    assert!(found.is_empty(), "found {found:?}");
}

#[tokio::test]
async fn garbage_is_ignored() {
    let garbage = Responder::replying_with("this is not JSON").await;
    let bulb = MockBulb::start().await;

    let found = discovery()
        .target(garbage.addr())
        .target(bulb.addr())
        .collect(WAIT)
        .await
        .expect("discover");

    // The real point: one device spraying nonsense does not stop the run, or
    // the bulb next to it from being found.
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].mac, bulb.mac());
}

#[tokio::test]
async fn a_bulb_that_moved_keeps_its_identity() {
    // Same MAC, two addresses: a bulb that DHCP moved between broadcasts. It is
    // one bulb, at the address it answered from most recently.
    let was = MockBulb::builder().mac("9877d5230f0a").start().await;
    let now = MockBulb::builder().mac("9877d5230f0a").start().await;

    let mut stream = discovery()
        .target(was.addr())
        .target(now.addr())
        .stream()
        .await
        .expect("discover");

    let first = timeout(WAIT, stream.next())
        .await
        .expect("a bulb answered")
        .expect("stream is running");
    let second = timeout(WAIT, stream.next())
        .await
        .expect("the address change was reported")
        .expect("stream is running");

    assert_eq!(first.mac, second.mac);
    assert_ne!(first.addr, second.addr, "the same address, twice");

    let found = discovery()
        .target(was.addr())
        .target(now.addr())
        .collect(WAIT)
        .await
        .expect("discover");
    assert_eq!(found.len(), 1, "one bulb, not two: {found:?}");
}

#[tokio::test]
async fn bulbs_are_reported_as_they_answer() {
    // The reason discovery streams at all: a bulb plugged in halfway through a
    // scan is found by a later broadcast, and the caller hears about it then
    // rather than at the end.
    let first = MockBulb::builder().mac("9877d5230f0a").start().await;
    let second = MockBulb::builder().mac("9877d523a4da").start().await;
    let (first_addr, second_addr) = (first.addr(), second.addr());
    drop(second);

    let mut stream = discovery()
        .target(first_addr)
        .target(second_addr)
        .stream()
        .await
        .expect("discover");

    let found = timeout(WAIT, stream.recv())
        .await
        .expect("the bulb that was there answered")
        .expect("stream is running");
    assert_eq!(found.mac, first.mac());

    let second = MockBulb::builder()
        .mac("9877d523a4da")
        .port(second_addr.port())
        .start()
        .await;
    let found = timeout(WAIT, stream.recv())
        .await
        .expect("the bulb plugged in mid-scan answered a later broadcast")
        .expect("stream is running");
    assert_eq!(found.mac, second.mac());
}

#[tokio::test]
async fn dropping_the_stream_stops_the_broadcasts() {
    let bulb = MockBulb::start().await;

    let stream = discovery()
        .target(bulb.addr())
        .stream()
        .await
        .expect("discover");
    tokio::time::sleep(INTERVAL * 2).await;
    drop(stream);
    let sent = bulb.requests().len();

    tokio::time::sleep(INTERVAL * 4).await;
    assert_eq!(bulb.requests().len(), sent, "still broadcasting");
}

#[tokio::test]
async fn system_config_is_fetched_on_request() {
    let bulb = MockBulb::start().await;

    let found = discovery()
        .target(bulb.addr())
        .system_config(true)
        .collect(WAIT)
        .await
        .expect("discover");

    let config = found[0]
        .system_config
        .as_ref()
        .expect("the follow-up was answered");
    let result: Value = config.parse_result().expect("parse");
    assert_eq!(result["moduleName"], "ESP25_SHRGB_01");
    assert_eq!(result["fwVersion"], "1.38.0");
}

#[tokio::test]
async fn a_bulb_that_will_not_elaborate_is_still_a_bulb() {
    // A device that answers the broadcast like a bulb and then answers nothing
    // else usefully — a bulb mid-reboot, or one whose reply to the follow-up is
    // the datagram that gets lost. It exists; it goes in the list.
    let taciturn = Responder::replying_with(
        r#"{"method":"registration","env":"pro","result":{"mac":"9877d5230f0a","success":true}}"#,
    )
    .await;

    let found = discovery()
        .target(taciturn.addr())
        .system_config(true)
        .policy(RetryPolicy {
            attempts: 1,
            attempt_timeout: Duration::from_millis(100),
            min_interval: Duration::from_millis(1),
        })
        .collect(WAIT)
        .await
        .expect("discover");

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].mac, "9877d5230f0a");
    assert!(found[0].system_config.is_none());
}

#[tokio::test]
async fn the_defaults_are_the_broadcast_address_and_one_second() {
    assert_eq!(BROADCAST.to_string(), "255.255.255.255:38899");
    assert_eq!(BROADCAST.port(), PORT);
    assert_eq!(DEFAULT_INTERVAL, Duration::from_secs(1));
}
