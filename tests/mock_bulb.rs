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
    // Measured on hardware: the bulb reports success and quietly clamps into
    // 1..=100, which is why the crate has to validate ranges itself.
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    for (sent, clamped) in [
        (0, 1),
        (1, 1),
        (100, 100),
        (101, 100),
        (200, 100),
        (255, 100),
    ] {
        let reply = client
            .ask(
                bulb.addr(),
                json!({"method": "setPilot", "params": {"dimming": sent}}),
            )
            .await;

        assert_eq!(reply["result"]["success"], true, "dimming {sent}");
        assert_eq!(bulb.pilot()["dimming"], clamped, "dimming {sent}");
    }
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

/// The harness has to be as unhelpful as the hardware here: an id past the last
/// scene is **accepted and clamped**, so nothing may treat a `success` as
/// evidence that the scene asked for is the one playing.
#[tokio::test]
async fn scene_ids_past_the_last_scene_are_accepted_and_clamped() {
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    let set = |scene: i64| {
        client.ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"sceneId": scene}}),
        )
    };

    for scene in [1, 36, 41] {
        assert_eq!(set(scene).await["result"]["success"], true, "{scene}");
        assert_eq!(bulb.pilot()["sceneId"], scene);
    }

    // Everything from 42 up answers success and plays 41 instead.
    for scene in [42, 100, 248] {
        assert_eq!(set(scene).await["result"]["success"], true, "{scene}");
        assert_eq!(bulb.pilot()["sceneId"], 41, "{scene} should clamp");
    }

    // 37 is not a scene at all: it sets a colour temperature and leaves scene
    // mode, which is why the crate refuses to send it.
    assert_eq!(set(37).await["result"]["success"], true);
    assert_eq!(bulb.pilot()["sceneId"], 0);
    assert_eq!(bulb.pilot()["temp"], 2200);

    // 0 is read-only, 249 is past the end, 256 is a user slot this bulb has
    // never had a custom mode saved into, and 1000 was Rhythm.
    for scene in [0, 249, 256, 265, 1000] {
        assert_eq!(set(scene).await["error"]["code"], -32602, "{scene}");
    }
    assert_eq!(bulb.pilot()["sceneId"], 0, "a refusal changes nothing");
}

/// A user slot accepts a write only once a custom mode has been saved into it,
/// and slots fill in order. This is what made `256` look permanently refused
/// until a custom mode existed to occupy it.
#[tokio::test]
async fn user_slots_work_only_when_populated() {
    let bulb = MockBulb::builder().custom_modes(2).start().await;
    let client = Client::new().await;

    for slot in [256, 257] {
        let reply = client
            .ask(
                bulb.addr(),
                json!({"method": "setPilot", "params": {"sceneId": slot}}),
            )
            .await;
        assert_eq!(reply["result"]["success"], true, "{slot}");
        assert_eq!(bulb.pilot()["sceneId"], slot);
        // Measured: a custom mode may be a dynamic one, and takes a speed.
        assert_eq!(bulb.pilot()["speed"], 100);
    }

    for slot in [258, 265] {
        let reply = client
            .ask(
                bulb.addr(),
                json!({"method": "setPilot", "params": {"sceneId": slot}}),
            )
            .await;
        assert_eq!(reply["error"]["code"], -32602, "{slot} is an empty slot");
    }
}

/// `getPilot` carries `speed` and `dimming` only where the running scene uses
/// them, which is the only way to tell which scenes take which.
#[tokio::test]
async fn a_running_scene_reports_only_the_parameters_it_uses() {
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    let cases = [
        // scene, speed?, dimming?, temp
        (4, true, true, None),         // Party: animates, rate settable
        (29, false, true, None),       // Candlelight: animates, rate is not
        (14, false, false, None),      // Night light: neither applies
        (11, false, true, Some(2700)), // Warm white: a static white
    ];

    for (scene, speed, dimming, temp) in cases {
        client
            .ask(
                bulb.addr(),
                json!({
                    "method": "setPilot",
                    "params": {"sceneId": scene, "speed": 150, "dimming": 40},
                }),
            )
            .await;
        let pilot = bulb.pilot();
        assert_eq!(pilot.get("speed").is_some(), speed, "{scene} speed");
        assert_eq!(pilot.get("dimming").is_some(), dimming, "{scene} dimming");
        assert_eq!(
            pilot.get("temp").and_then(serde_json::Value::as_i64),
            temp,
            "{scene} temp"
        );
    }
}

#[tokio::test]
async fn a_temperature_outside_the_model_range_is_clamped_not_rejected() {
    // Measured on ESP25_SHRGB_01 fw 1.38.0, which reports a cctRange of
    // 2200-6500: everything the wire accepts is taken and clamped into that,
    // in both directions, and read back clamped. Only 1000-12000 is the
    // boundary between "clamped" and "refused".
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    for (sent, reported) in [
        (1000, 2200),
        (2199, 2200),
        (2200, 2200),
        (4000, 4000),
        (6500, 6500),
        (6501, 6500),
        (9000, 6500),
        (12000, 6500),
    ] {
        let reply = client
            .ask(
                bulb.addr(),
                json!({"method": "setPilot", "params": {"temp": sent}}),
            )
            .await;
        assert_eq!(reply["result"]["success"], true, "temp {sent}");
        assert_eq!(bulb.pilot()["temp"], reported, "temp {sent}");
    }

    for refused in [999, 12001] {
        let reply = client
            .ask(
                bulb.addr(),
                json!({"method": "setPilot", "params": {"temp": refused}}),
            )
            .await;
        assert_eq!(reply["error"]["code"], -32602, "temp {refused}");
    }
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

#[tokio::test]
async fn pulse_validates_params_sets_scene_248_and_restores_state() {
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    // Missing params, empty params, delta=0, delta=65536 (16-bit truncation to 0), and duration=0
    // all draw -32602 Invalid params on hardware.
    for bad in [
        json!({"method": "pulse"}),
        json!({"method": "pulse", "params": {}}),
        json!({"method": "pulse", "params": {"delta": -50}}),
        json!({"method": "pulse", "params": {"duration": 100}}),
        json!({"method": "pulse", "params": {"delta": 0, "duration": 100}}),
        json!({"method": "pulse", "params": {"delta": 65536, "duration": 100}}),
        json!({"method": "pulse", "params": {"delta": -65536, "duration": 100}}),
        json!({"method": "pulse", "params": {"delta": -50, "duration": 0}}),
    ] {
        let reply = client.ask(bulb.addr(), bad.clone()).await;
        assert_eq!(reply["error"]["code"], -32602, "{bad}");
    }

    // Valid pulse enters sceneId 248 + effect "pulse" and restores sceneId 11 when done.
    let reply = client
        .ask(
            bulb.addr(),
            json!({"method": "pulse", "params": {"delta": -60, "duration": 40}}),
        )
        .await;
    assert_eq!(reply["result"]["success"], true);
    let during = bulb.pilot();
    assert_eq!(during["sceneId"], 248);
    assert_eq!(during["effect"], "pulse");

    tokio::time::sleep(Duration::from_millis(120)).await;
    let after = bulb.pilot();
    assert_eq!(after["sceneId"], 11);
    assert_eq!(after["temp"], 2700);
    assert!(after.get("effect").is_none());
}

#[tokio::test]
async fn pulse_on_an_off_bulb_stays_off_and_set_pilot_cancels_pulse() {
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
            json!({"method": "pulse", "params": {"delta": 80, "duration": 500}}),
        )
        .await;
    assert_eq!(reply["result"]["success"], true);
    assert_eq!(bulb.pilot()["state"], false);
    assert_eq!(bulb.pilot()["sceneId"], 248);
    assert_eq!(bulb.pilot()["effect"], "pulse");

    // An intervening setPilot cancels the pulse and does not revert when the pulse timer expires.
    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"r": 0, "g": 255, "b": 0, "dimming": 60}}),
        )
        .await;
    let after_set = bulb.pilot();
    assert_eq!(after_set["state"], true);
    assert_eq!(after_set["sceneId"], 0);
    assert_eq!(after_set["g"], 255);
    assert!(after_set.get("effect").is_none());
}

#[tokio::test]
async fn channel_group_replaces_all_five_truncates_to_low_byte_and_discards_all_zero() {
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    // A channel write replaces all five: omitted channels go to 0.
    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"r": 255, "g": 80, "b": 0}}),
        )
        .await;
    assert_eq!(bulb.pilot()["r"], 255);
    assert_eq!(bulb.pilot()["w"], 0);

    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"w": 128}}),
        )
        .await;
    assert_eq!(bulb.pilot()["r"], 0, "colour replaced by warm white");
    assert_eq!(bulb.pilot()["g"], 0);
    assert_eq!(bulb.pilot()["b"], 0);
    assert_eq!(bulb.pilot()["c"], 0);
    assert_eq!(bulb.pilot()["w"], 128);

    // Out-of-byte values truncate to their low eight bits.
    for (sent, reported) in [(300, 44), (1000, 232), (65535, 255), (-1, 255)] {
        let reply = client
            .ask(
                bulb.addr(),
                json!({"method": "setPilot", "params": {"w": sent}}),
            )
            .await;
        assert_eq!(reply["result"]["success"], true, "w={sent}");
        assert_eq!(bulb.pilot()["w"], reported, "w={sent}");
    }

    // Set w=128 before probing all-zero groups so we can prove they do not overwrite it.
    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"w": 128}}),
        )
        .await;

    // An all-zero channel group alone (including 256, which truncates to 0) draws -32600.
    for bad in [
        json!({"method": "setPilot", "params": {"w": 0}}),
        json!({"method": "setPilot", "params": {"c": 0}}),
        json!({"method": "setPilot", "params": {"r": 0, "g": 0, "b": 0}}),
        json!({"method": "setState", "params": {"w": 0}}),
        json!({"method": "setPilot", "params": {"w": 256}}),
    ] {
        let reply = client.ask(bulb.addr(), bad.clone()).await;
        assert_eq!(reply["error"]["code"], -32600, "{bad}");
        assert_eq!(
            bulb.pilot()["w"],
            128,
            "refused write leaves w=128 untouched"
        );
    }

    // Alongside another valid field, an all-zero channel group is discarded whole while the
    // surviving field applies.
    let reply = client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"state": true, "w": 0}}),
        )
        .await;
    assert_eq!(reply["result"]["success"], true);
    assert_eq!(bulb.pilot()["w"], 128, "all-zero group discarded");

    let reply = client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"dimming": 40, "c": 0, "w": 0}}),
        )
        .await;
    assert_eq!(reply["result"]["success"], true);
    assert_eq!(bulb.pilot()["dimming"], 40);
    assert_eq!(bulb.pilot()["w"], 128, "all-zero group still discarded");
}

#[tokio::test]
async fn speed_bounds_and_bare_speed_or_dimming_follow_the_running_scene() {
    let bulb = MockBulb::builder().custom_modes(1).start().await;
    let client = Client::new().await;

    // Speed wire bounds: 10..=200 accepted, 9 and 201 refused with -32602.
    for bad in [0, 9, 201, 255] {
        let reply = client
            .ask(
                bulb.addr(),
                json!({"method": "setPilot", "params": {"sceneId": 4, "speed": bad}}),
            )
            .await;
        assert_eq!(reply["error"]["code"], -32602, "speed {bad}");
    }

    // Bare speed over a running dynamic scene (4) or user slot (256) is honoured.
    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"sceneId": 4, "speed": 100}}),
        )
        .await;
    let reply = client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"speed": 150}}),
        )
        .await;
    assert_eq!(reply["result"]["success"], true);
    assert_eq!(bulb.pilot()["speed"], 150);

    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"sceneId": 256, "speed": 20}}),
        )
        .await;
    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"speed": 180}}),
        )
        .await;
    assert_eq!(bulb.pilot()["speed"], 180);

    // Switching from a dynamic scene to temp or colour drops `speed` from getPilot.
    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"temp": 4000}}),
        )
        .await;
    assert!(bulb.pilot().get("speed").is_none(), "temp drops speed");

    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"sceneId": 4, "speed": 120}}),
        )
        .await;
    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"r": 255, "g": 0, "b": 0}}),
        )
        .await;
    assert!(bulb.pilot().get("speed").is_none(), "colour drops speed");

    // Bare speed over a running static scene (11) is accepted and discarded.
    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"sceneId": 11}}),
        )
        .await;
    let reply = client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"speed": 150}}),
        )
        .await;
    assert_eq!(reply["result"]["success"], true);
    assert!(
        bulb.pilot().get("speed").is_none(),
        "bare speed on static scene 11 is discarded"
    );

    // Bare dimming over an undimmable scene (14, Night light) is accepted and discarded.
    client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"sceneId": 14}}),
        )
        .await;
    let reply = client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"dimming": 30}}),
        )
        .await;
    assert_eq!(reply["result"]["success"], true);
    assert!(
        bulb.pilot().get("dimming").is_none(),
        "bare dimming on Night light is discarded"
    );
}

#[tokio::test]
async fn ratio_and_devices_validate_bounds_and_discard_or_refuse_on_single_head() {
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    // ratio wire range is 0..=100; on a single-head bulb it is accepted and discarded.
    for ok in [0, 50, 75, 100] {
        let reply = client
            .ask(
                bulb.addr(),
                json!({"method": "setPilot", "params": {"ratio": ok}}),
            )
            .await;
        assert_eq!(reply["result"]["success"], true, "ratio {ok}");
        assert!(
            bulb.pilot().get("ratio").is_none(),
            "single-head bulb discards ratio {ok}"
        );
    }
    for bad in [-1, 101] {
        let reply = client
            .ask(
                bulb.addr(),
                json!({"method": "setPilot", "params": {"ratio": bad}}),
            )
            .await;
        assert_eq!(reply["error"]["code"], -32602, "ratio {bad}");
    }

    // devices on write: 1 and 3 accepted on single-head; 0, 2 and 4 refused with -32602.
    for ok in [1, 3] {
        let reply = client
            .ask(
                bulb.addr(),
                json!({"method": "setPilot", "params": {"devices": ok, "dimming": 50}}),
            )
            .await;
        assert_eq!(reply["result"]["success"], true, "write devices {ok}");
        assert!(bulb.pilot().get("devices").is_none());
    }
    for bad in [0, 2, 4] {
        let reply = client
            .ask(
                bulb.addr(),
                json!({"method": "setPilot", "params": {"devices": bad, "dimming": 50}}),
            )
            .await;
        assert_eq!(reply["error"]["code"], -32602, "write devices {bad}");
    }

    // devices on read (getPilot): zero-based head index, one-based tag in reply.
    let bare_read = client.ask(bulb.addr(), json!({"method": "getPilot"})).await;
    assert_eq!(bare_read["result"]["mac"], bulb.mac());
    assert!(bare_read["result"].get("devices").is_none());

    let head_zero = client
        .ask(
            bulb.addr(),
            json!({"method": "getPilot", "params": {"devices": 0}}),
        )
        .await;
    assert_eq!(head_zero["result"]["devices"], 1);

    for bad in [-1, 1, 2, 3] {
        let reply = client
            .ask(
                bulb.addr(),
                json!({"method": "getPilot", "params": {"devices": bad}}),
            )
            .await;
        assert_eq!(reply["error"]["code"], -32602, "read devices {bad}");
    }
}

#[tokio::test]
async fn unknown_pilot_params_and_reboot_return_invalid_request_32600() {
    let bulb = MockBulb::start().await;
    let client = Client::new().await;

    let reply = client
        .ask(
            bulb.addr(),
            json!({"method": "setPilot", "params": {"nonsense": 1}}),
        )
        .await;
    assert_eq!(reply["error"]["code"], -32600);
    assert_eq!(reply["error"]["message"], "Invalid Request");
    assert!(bulb.pilot().get("nonsense").is_none());

    for reboot in [
        json!({"method": "reboot"}),
        json!({"method": "reboot", "params": {}}),
        json!({"method": "reboot", "params": null}),
    ] {
        let reply = client.ask(bulb.addr(), reboot.clone()).await;
        assert_eq!(reply["error"]["code"], -32600, "{reboot}");
        assert_eq!(reply["error"]["message"], "Invalid Request", "{reboot}");
    }
}
