//! `Bulb::request` against the mock bulb.
//!
//! One test per way the exchange can go wrong, because the point of the typed
//! error is that a caller can tell them apart.

mod common;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use common::mock_bulb::{MockBulb, Personality};
use common::udp::Client;
use serde::Deserialize;
use serde_json::json;
use tokio::net::UdpSocket;
use wizlight::{Bulb, Error, PORT, Request, RetryPolicy};

/// Fails fast, so a test that expects a timeout does not spend 1.5 s in it.
fn impatient() -> RetryPolicy {
    RetryPolicy {
        attempts: 3,
        attempt_timeout: Duration::from_millis(100),
        min_interval: Duration::from_millis(1),
    }
}

async fn connect(bulb: &MockBulb) -> Bulb {
    Bulb::connect_to(bulb.addr()).await.expect("bind socket")
}

#[tokio::test]
async fn answers_a_request() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let response = client
        .request(&Request::new("getPilot"))
        .await
        .expect("getPilot");

    assert_eq!(response.method.as_deref(), Some("getPilot"));
    assert_eq!(response.error, None);
    assert_eq!(response.result.expect("result")["mac"], bulb.mac());
    // Exactly what the official app puts on the wire, empty params included.
    assert_eq!(bulb.requests(), [r#"{"method":"getPilot","params":{}}"#]);
}

#[tokio::test]
async fn sends_the_params_it_was_given() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let request = Request::with_params("setPilot", &json!({"dimming": 40})).expect("serialise");
    let response = client.request(&request).await.expect("setPilot");

    assert_eq!(response.result.expect("result")["success"], true);
    assert_eq!(bulb.pilot()["dimming"], 40);
}

#[tokio::test]
async fn unknown_fields_in_a_reply_are_ignored() {
    // The 1.38.0 firmware returns seven `getModelConfig` fields that no
    // pywizlight fixture has. Parsing must survive the next seven too.
    #[derive(Debug, Deserialize)]
    struct ModelConfig {
        #[serde(rename = "cctRange")]
        cct_range: [u32; 4],
    }

    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let response = client
        .request(&Request::new("getModelConfig"))
        .await
        .expect("getModelConfig");
    let config: ModelConfig = response.parse_result().expect("parse");

    assert_eq!(config.cct_range, [2200, 2700, 6500, 6500]);
}

#[tokio::test]
async fn retries_until_the_bulb_answers() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await.with_policy(impatient());
    bulb.drop_next(2);

    let response = client
        .request(&Request::new("getPilot"))
        .await
        .expect("getPilot");

    assert_eq!(response.method.as_deref(), Some("getPilot"));
    assert_eq!(bulb.requests().len(), 3, "two dropped, one answered");
}

#[tokio::test]
async fn gives_up_after_the_configured_attempts() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await.with_policy(impatient());
    bulb.drop_next(100);

    let error = client
        .request(&Request::new("getPilot"))
        .await
        .expect_err("nothing was answered");

    match error {
        Error::Timeout {
            method,
            addr,
            attempts,
            ..
        } => {
            assert_eq!(method, "getPilot");
            assert_eq!(addr, bulb.addr());
            assert_eq!(attempts, 3);
        }
        other => panic!("expected a timeout, got {other:?}"),
    }
    assert_eq!(bulb.requests().len(), 3, "one datagram per attempt");
}

#[tokio::test]
async fn retries_are_paced() {
    // Retrying flat out is how a struggling bulb is pushed over the edge: at
    // zero spacing it drops three quarters of what it is sent.
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await.with_policy(RetryPolicy {
        attempts: 3,
        attempt_timeout: Duration::from_millis(20),
        min_interval: Duration::from_millis(80),
    });
    bulb.drop_next(100);

    let started = Instant::now();
    client
        .request(&Request::new("getPilot"))
        .await
        .expect_err("nothing was answered");

    // Two gaps between three datagrams. Only a lower bound is asserted; how
    // much longer a loaded CI runner takes is not this test's business.
    assert!(
        started.elapsed() >= Duration::from_millis(160),
        "retries were not paced: {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_reply_that_is_not_json_is_an_error() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await.with_policy(impatient());
    bulb.malformed_next(1);

    let error = client
        .request(&Request::new("getPilot"))
        .await
        .expect_err("garbage is not a reply");

    assert!(
        matches!(error, Error::Json(_)),
        "expected a parse failure, got {error:?}"
    );
    assert_eq!(bulb.requests().len(), 1, "garbage is not worth retrying");
}

#[tokio::test]
async fn a_method_the_firmware_lacks_is_not_supported() {
    // Measured: the bulb answers getWifiConfig with -32601, same as any method
    // it has never heard of.
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let error = client
        .request(&Request::new("getWifiConfig"))
        .await
        .expect_err("no such method on this firmware");

    match error {
        Error::NotSupported { method } => assert_eq!(method, "getWifiConfig"),
        other => panic!("expected NotSupported, got {other:?}"),
    }
}

#[tokio::test]
async fn a_model_without_the_method_at_all_says_so() {
    let bulb = MockBulb::builder()
        .personality(Personality::tunable_white())
        .start()
        .await;
    let client = connect(&bulb).await;

    let error = client
        .request(&Request::new("getPower"))
        .await
        .expect_err("older firmware has no getPower");

    assert!(
        matches!(error, Error::NotSupported { .. }),
        "expected NotSupported, got {error:?}"
    );
}

#[tokio::test]
async fn power_is_answered_by_models_that_implement_it() {
    let socket = MockBulb::builder()
        .personality(Personality::socket())
        .start()
        .await;
    let client = connect(&socket).await;
    let response = client
        .request(&Request::new("getPower"))
        .await
        .expect("getPower");
    assert_eq!(response.result.expect("result")["power"], 1_065_385);

    // Measured: the RGB bulb answers too, always with 0. The method existing
    // and the meter existing are different things.
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;
    let response = client
        .request(&Request::new("getPower"))
        .await
        .expect("getPower");
    assert_eq!(response.result.expect("result")["power"], 0);
}

#[tokio::test]
async fn rejected_params_are_reported_as_such() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let request = Request::with_params("setPilot", &json!({"temp": 99999})).expect("serialise");
    let error = client.request(&request).await.expect_err("out of range");

    assert!(
        matches!(error, Error::InvalidParam { .. }),
        "expected InvalidParam, got {error:?}"
    );
}

#[tokio::test]
async fn any_other_refusal_keeps_the_bulb_s_own_code() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;
    bulb.error_next(1, -32700, "Parse error");

    let error = client
        .request(&Request::new("getPilot"))
        .await
        .expect_err("the bulb refused");

    match error {
        Error::Device {
            method,
            code,
            message,
        } => {
            assert_eq!(method, "getPilot");
            assert_eq!(code, -32700);
            assert_eq!(message, "Parse error");
        }
        other => panic!("expected Device, got {other:?}"),
    }
}

#[tokio::test]
async fn a_reply_from_somewhere_else_is_ignored() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;
    bulb.set_latency(Some(Duration::from_millis(150)));

    let target = SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        client.local_addr().expect("local_addr").port(),
    );
    let impostor = tokio::spawn(async move {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind impostor");
        tokio::time::sleep(Duration::from_millis(20)).await;
        let reply = json!({"method": "getPilot", "env": "pro", "result": {"mac": "000000000000"}});
        socket
            .send_to(reply.to_string().as_bytes(), target)
            .await
            .expect("send impostor reply");
    });

    let response = client
        .request(&Request::new("getPilot"))
        .await
        .expect("getPilot");

    impostor.await.expect("impostor finished");
    assert_eq!(
        response.result.expect("result")["mac"],
        bulb.mac(),
        "answered by the wrong device"
    );
}

#[tokio::test]
async fn a_push_arriving_first_is_not_mistaken_for_the_acknowledgement() {
    // Measured: a syncPilot can beat the reply to the request that caused it,
    // and it comes from the bulb's own address, so only the method tells them
    // apart.
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;
    bulb.set_push_port(client.local_addr().expect("local_addr").port());
    bulb.push_before_ack(true);

    let register = Request::with_params(
        "registration",
        &json!({"phoneMac": "AAAAAAAAAAAA", "register": true, "phoneIp": "127.0.0.1", "id": "1"}),
    )
    .expect("serialise");
    let response = client.request(&register).await.expect("registration");
    assert_eq!(response.method.as_deref(), Some("registration"));
    assert_eq!(response.result.expect("result")["success"], true);

    let set = Request::with_params("setPilot", &json!({"dimming": 30})).expect("serialise");
    let response = client.request(&set).await.expect("setPilot");
    assert_eq!(response.method.as_deref(), Some("setPilot"));
    assert_eq!(response.result.expect("result")["success"], true);
}

#[tokio::test]
async fn a_reply_to_an_abandoned_request_is_not_reused() {
    // A reply that arrives after its exchange gave up is still sitting in the
    // socket buffer. Handed to the next exchange it would look perfectly
    // valid — same method, same address — and report the state as it was
    // before, which for a UI is a light that flickers back to its old colour.
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await.with_policy(RetryPolicy {
        attempts: 1,
        attempt_timeout: Duration::from_millis(50),
        min_interval: Duration::from_millis(1),
    });
    let onlooker = Client::new().await;

    bulb.set_latency(Some(Duration::from_millis(150)));
    client
        .request(&Request::new("getPilot"))
        .await
        .expect_err("too slow to answer");

    // Let the abandoned reply, reporting dimming 100, land in our buffer.
    tokio::time::sleep(Duration::from_millis(200)).await;
    bulb.set_latency(None);
    onlooker
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"dimming": 30}}),
        )
        .await;

    let response = client
        .request(&Request::new("getPilot"))
        .await
        .expect("getPilot");

    assert_eq!(
        response.result.expect("result")["dimming"],
        30,
        "answered from the previous exchange's stale reply"
    );
}

#[tokio::test]
async fn concurrent_requests_do_not_steal_each_other_s_replies() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;
    bulb.set_latency(Some(Duration::from_millis(30)));

    let (get_pilot, get_config) = (Request::new("getPilot"), Request::new("getSystemConfig"));
    let (pilot, config) = tokio::join!(client.request(&get_pilot), client.request(&get_config),);

    assert_eq!(
        pilot.expect("getPilot").method.as_deref(),
        Some("getPilot"),
        "got another exchange's reply"
    );
    assert_eq!(
        config.expect("getSystemConfig").method.as_deref(),
        Some("getSystemConfig"),
        "got another exchange's reply"
    );
}

#[tokio::test]
async fn connect_uses_the_standard_port() {
    let client = Bulb::connect(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 5)))
        .await
        .expect("bind socket");

    assert_eq!(client.addr(), SocketAddr::from(([192, 168, 0, 5], PORT)));
    assert_eq!(PORT, 38899);
}
