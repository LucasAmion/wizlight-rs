//! Tests for the mock bulb itself.
//!
//! The harness is only useful if it behaves like the hardware, so its quirks are
//! pinned here — particularly the ones that are easy to "fix" by accident.

mod common;

use std::time::Duration;

use common::mock_bulb::{MockBulb, Personality};
use common::udp::{Client, PushListener, REPLY_TIMEOUT};
use serde_json::{Value, json};

fn get_pilot() -> Value {
    json!({"method": "getPilot", "params": {}})
}

#[tokio::test]
async fn reports_its_initial_state() {
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    let reply = client.ask(bulb.addr(), get_pilot()).await;

    assert_eq!(reply["method"], "getPilot");
    assert_eq!(reply["env"], "pro");
    assert_eq!(reply["result"]["mac"], bulb.mac());
    assert_eq!(reply["result"]["state"], true);
    assert_eq!(reply["result"]["dimming"], 100);
}

#[tokio::test]
async fn set_pilot_changes_state_and_acknowledges() {
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    let reply = client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"dimming": 40}}),
        )
        .await;

    assert_eq!(
        reply,
        json!({"method": "setPilot", "env": "pro", "result": {"success": true}})
    );
    assert_eq!(bulb.pilot()["dimming"], 40);
}

#[tokio::test]
async fn records_the_exact_bytes_received() {
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    client.ask(bulb.addr(), get_pilot()).await;
    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"state": false}}),
        )
        .await;

    let requests = bulb.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], r#"{"method":"getPilot","params":{}}"#);
    assert_eq!(bulb.last_request().unwrap()["params"]["state"], false);
}

#[tokio::test]
async fn out_of_range_dimming_is_clamped_not_rejected() {
    // Measured on hardware: the bulb reports success and quietly clamps, which
    // is why the crate has to validate ranges itself.
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    let reply = client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"dimming": 200}}),
        )
        .await;

    assert_eq!(reply["result"]["success"], true);
    assert_eq!(bulb.pilot()["dimming"], 100);
}

#[tokio::test]
async fn out_of_range_temperature_and_scene_are_rejected() {
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    for params in [json!({"temp": 99999}), json!({"sceneId": 999})] {
        let reply = client
            .ask(bulb.addr(), json!({"method": "setPilot", "params": params}))
            .await;
        assert_eq!(reply["error"]["code"], -32602);
        assert_eq!(reply["error"]["message"], "Invalid params");
    }

    // A rejected request must not have changed anything.
    assert_eq!(bulb.pilot()["sceneId"], 11);
    assert_eq!(bulb.pilot()["temp"], 2700);
}

#[tokio::test]
async fn rejects_unknown_methods_and_missing_params() {
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    let unknown = client
        .ask(bulb.addr(), json!({"method": "noSuchMethod", "params": {}}))
        .await;
    assert_eq!(unknown["error"]["code"], -32601);
    assert_eq!(unknown["method"], "noSuchMethod");

    // Measured: the two ways of saying "no params" draw different codes.
    let bare = client.ask(bulb.addr(), json!({"method": "setPilot"})).await;
    assert_eq!(bare["error"]["code"], -32602, "no params key");

    let empty = client
        .ask(bulb.addr(), json!({"method": "setPilot", "params": {}}))
        .await;
    assert_eq!(empty["error"]["code"], -32600, "empty params object");
    assert_eq!(empty["error"]["message"], "Invalid Request");
}

#[tokio::test]
async fn colour_temperature_and_scene_are_mutually_exclusive() {
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"r": 255, "g": 80, "b": 0}}),
        )
        .await;
    let pilot = bulb.pilot();
    assert_eq!(pilot["sceneId"], 0, "colour clears the scene");
    assert_eq!(pilot["c"], 0, "the white channels are reported alongside");
    assert!(pilot.get("temp").is_none(), "colour clears the temperature");

    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"temp": 5000}}),
        )
        .await;
    let pilot = bulb.pilot();
    assert_eq!(pilot["temp"], 5000);
    assert!(pilot.get("r").is_none(), "temperature clears the colour");

    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"sceneId": 4, "speed": 100}}),
        )
        .await;
    let pilot = bulb.pilot();
    assert_eq!(pilot["sceneId"], 4);
    assert_eq!(pilot["speed"], 100);
    assert!(
        pilot.get("temp").is_none(),
        "a scene clears the temperature"
    );
}

#[tokio::test]
async fn set_state_turns_the_bulb_on() {
    // setState is not a "colour without power" variant, whatever its name
    // suggests: on firmware 1.38.0 it switches the bulb on just like setPilot.
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"state": false}}),
        )
        .await;
    assert_eq!(bulb.pilot()["state"], false);

    let reply = client
        .ask(
            bulb.addr(),
            json!({"method": "setState", "params": {"r": 255, "g": 0, "b": 0}}),
        )
        .await;

    assert_eq!(reply["method"], "setState");
    assert_eq!(reply["result"]["success"], true);
    assert_eq!(bulb.pilot()["state"], true, "setState powers the bulb on");
}

#[tokio::test]
async fn colour_temperature_and_scene_wake_a_sleeping_bulb() {
    let client = Client::new().await;
    let off = json!({"method": "setPilot", "params": {"state": false}});

    for params in [
        json!({"r": 0, "g": 128, "b": 255}),
        json!({"temp": 4000}),
        json!({"sceneId": 4}),
    ] {
        let bulb = MockBulb::start().await;
        client.ask(bulb.addr(), off.clone()).await;
        client
            .ask(bulb.addr(), json!({"method": "setPilot", "params": params}))
            .await;
        assert_eq!(
            bulb.pilot()["state"],
            true,
            "{params} should power the bulb on"
        );
    }
}

#[tokio::test]
async fn dimming_alone_does_not_wake_a_sleeping_bulb() {
    // Measured, and surprising enough to be worth pinning: an off bulb accepts
    // a dimming-only request, answers success, and discards it — the stored
    // brightness does not even change. Setting brightness before switching on
    // therefore silently does nothing.
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"state": false}}),
        )
        .await;
    let reply = client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"dimming": 55}}),
        )
        .await;

    assert_eq!(reply["result"]["success"], true);
    assert_eq!(bulb.pilot()["state"], false);
    assert_eq!(
        bulb.pilot()["dimming"],
        100,
        "the request is discarded, not stored"
    );
}

#[tokio::test]
async fn registration_starts_the_push_stream() {
    let listener = PushListener::bind().await;
    let bulb = MockBulb::builder().push_port(listener.port()).start().await;
    let client = Client::new().await;

    let ack = client
        .ask(
            bulb.addr(),
            json!({"method": "registration", "params": {
                "phoneMac": "AAAAAAAAAAAA", "register": true, "phoneIp": "127.0.0.1", "id": "1"
            }}),
        )
        .await;
    assert_eq!(ack["result"]["success"], true);
    assert_eq!(ack["result"]["mac"], bulb.mac());

    let hello = listener
        .next(REPLY_TIMEOUT)
        .await
        .expect("push on register");
    assert_eq!(hello["method"], "syncPilot");
    assert_eq!(hello["params"]["src"], "wizc1");

    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"dimming": 30}}),
        )
        .await;
    let change = listener.next(REPLY_TIMEOUT).await.expect("push on change");
    assert_eq!(change["params"]["src"], "udp");
    assert_eq!(change["params"]["dimming"], 30);

    assert!(
        bulb.push_heartbeat().await,
        "heartbeat needs a registration"
    );
    let heartbeat = listener.next(REPLY_TIMEOUT).await.expect("heartbeat");
    assert_eq!(heartbeat["params"]["src"], "hb");
    assert!(heartbeat["params"]["ts"].is_number());
}

#[tokio::test]
async fn unregistering_stops_the_push_stream() {
    let listener = PushListener::bind().await;
    let bulb = MockBulb::builder().push_port(listener.port()).start().await;
    let client = Client::new().await;

    let register = |register: bool| {
        json!({"method": "registration", "params": {
            "phoneMac": "AAAAAAAAAAAA", "register": register, "phoneIp": "127.0.0.1", "id": "1"
        }})
    };
    client.ask(bulb.addr(), register(true)).await;
    listener
        .next(REPLY_TIMEOUT)
        .await
        .expect("push on register");
    client.ask(bulb.addr(), register(false)).await;

    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"dimming": 30}}),
        )
        .await;

    assert!(bulb.push_target().is_none());
    assert!(
        listener.next(Duration::from_millis(300)).await.is_none(),
        "no pushes after unregistering"
    );
}

#[tokio::test]
async fn can_push_before_acknowledging() {
    // Real hardware does this, and code that assumes otherwise races.
    let listener = PushListener::bind().await;
    let bulb = MockBulb::builder().push_port(listener.port()).start().await;
    let client = Client::new().await;

    client
        .ask(
            bulb.addr(),
            json!({"method": "registration", "params": {
                "phoneMac": "AAAAAAAAAAAA", "register": true, "phoneIp": "127.0.0.1", "id": "1"
            }}),
        )
        .await;
    listener
        .next(REPLY_TIMEOUT)
        .await
        .expect("push on register");

    bulb.push_before_ack(true);
    client
        .send(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"state": false}}),
        )
        .await;

    let push = listener.next(REPLY_TIMEOUT).await.expect("push");
    assert_eq!(push["params"]["src"], "udp");
    let ack = client.recv(REPLY_TIMEOUT).await.expect("ack");
    assert_eq!(ack["result"]["success"], true);
}

#[tokio::test]
async fn drops_datagrams_on_demand() {
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    bulb.drop_next(2);
    for _ in 0..2 {
        assert!(
            client
                .try_ask(bulb.addr(), get_pilot(), Duration::from_millis(200))
                .await
                .is_none(),
            "dropped request must not answer"
        );
    }

    let reply = client.ask(bulb.addr(), get_pilot()).await;
    assert_eq!(reply["method"], "getPilot");
    assert_eq!(
        bulb.requests().len(),
        3,
        "dropped requests are still recorded"
    );
}

#[tokio::test]
async fn answers_with_garbage_on_demand() {
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    bulb.malformed_next(1);
    let reply = client.ask(bulb.addr(), get_pilot()).await;
    assert_eq!(reply, Value::String("garbage".into()));

    let reply = client.ask(bulb.addr(), get_pilot()).await;
    assert_eq!(reply["method"], "getPilot");
}

#[tokio::test]
async fn answers_with_a_forced_error_on_demand() {
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    bulb.error_next(1, -32603, "Internal error");
    let reply = client.ask(bulb.addr(), get_pilot()).await;
    assert_eq!(reply["error"]["code"], -32603);
    assert_eq!(reply["error"]["message"], "Internal error");

    assert!(client.ask(bulb.addr(), get_pilot()).await["result"].is_object());
}

#[tokio::test]
async fn delays_replies_on_demand() {
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    bulb.set_latency(Some(Duration::from_millis(300)));
    assert!(
        client
            .try_ask(bulb.addr(), get_pilot(), Duration::from_millis(50))
            .await
            .is_none(),
        "a slow bulb should not answer within the timeout"
    );

    bulb.set_latency(None);
    assert_eq!(
        client.ask(bulb.addr(), get_pilot()).await["method"],
        "getPilot"
    );
}

#[tokio::test]
async fn rejects_input_that_is_not_a_request() {
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    client
        .send(bulb.addr(), Value::String("not json".into()))
        .await;
    let reply = client.recv(REPLY_TIMEOUT).await.expect("parse error");

    assert_eq!(reply["error"]["code"], -32700);
}

#[tokio::test]
async fn personalities_report_their_own_hardware() {
    let rgb = MockBulb::start().await;
    let white = MockBulb::builder()
        .personality(Personality::tunable_white())
        .mac("a8bb503ea5f4")
        .start()
        .await;
    let client = Client::new().await;

    let rgb_config = client
        .ask(
            rgb.addr(),
            json!({"method": "getSystemConfig", "params": {}}),
        )
        .await;
    assert_eq!(rgb_config["result"]["moduleName"], "ESP25_SHRGB_01");
    assert_eq!(rgb_config["result"]["fwVersion"], "1.38.0");

    let white_config = client
        .ask(
            white.addr(),
            json!({"method": "getSystemConfig", "params": {}}),
        )
        .await;
    assert_eq!(white_config["result"]["moduleName"], "ESP14_SHTW1C_01");

    // The Kelvin range lives in getModelConfig on new firmware and in
    // getUserConfig on old, which is the whole reason both are read.
    let rgb_model = client
        .ask(
            rgb.addr(),
            json!({"method": "getModelConfig", "params": {}}),
        )
        .await;
    assert_eq!(
        rgb_model["result"]["cctRange"],
        json!([2200, 2700, 6500, 6500])
    );

    let white_model = client
        .ask(
            white.addr(),
            json!({"method": "getModelConfig", "params": {}}),
        )
        .await;
    assert_eq!(
        white_model["error"]["code"], -32601,
        "no getModelConfig on 1.18.0"
    );

    let white_user = client
        .ask(
            white.addr(),
            json!({"method": "getUserConfig", "params": {}}),
        )
        .await;
    assert_eq!(white_user["result"]["whiteRange"], json!([2700, 6500]));
}

#[tokio::test]
async fn power_reporting_is_per_model() {
    let socket = MockBulb::builder()
        .personality(Personality::socket())
        .start()
        .await;
    let older = MockBulb::builder()
        .personality(Personality::tunable_white())
        .start()
        .await;
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    let request = json!({"method": "getPower", "params": {}});
    let from_socket = client.ask(socket.addr(), request.clone()).await;
    assert_eq!(from_socket["result"]["power"], 1065385);

    // Measured on an ESP25_SHRGB_01: it answers, and the answer is always 0.
    // Treating "answers getPower" as "reports power" would be wrong.
    let from_bulb = client.ask(bulb.addr(), request.clone()).await;
    assert_eq!(from_bulb["result"]["power"], 0);

    let from_older = client.ask(older.addr(), request).await;
    assert_eq!(from_older["error"]["code"], -32601);
}

#[tokio::test]
async fn two_bulbs_run_side_by_side() {
    let one = MockBulb::builder().mac("9877d5230f0a").start().await;
    let two = MockBulb::builder().mac("9877d523a4da").start().await;
    let client = Client::new().await;

    assert_ne!(one.port(), two.port(), "ephemeral ports must not collide");

    client
        .ask(
            one.addr(),
            json!({"method": "setPilot", "params": {"dimming": 20}}),
        )
        .await;

    assert_eq!(one.pilot()["dimming"], 20);
    assert_eq!(two.pilot()["dimming"], 100, "bulbs keep separate state");
    assert_eq!(
        client.ask(two.addr(), get_pilot()).await["result"]["mac"],
        "9877d523a4da"
    );
    assert!(two.requests().len() == 1 && one.requests().len() == 1);
}

#[tokio::test]
async fn stops_serving_once_dropped() {
    let bulb = MockBulb::start().await;
    let addr = bulb.addr();
    let client = Client::new().await;
    client.ask(addr, get_pilot()).await;

    drop(bulb);

    assert!(
        client
            .try_ask(addr, get_pilot(), Duration::from_millis(200))
            .await
            .is_none(),
        "a dropped bulb should be off the air"
    );
}
