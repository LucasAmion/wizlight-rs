//! Typed protocol methods and the pilot builder against the mock bulb.

mod common;

use std::time::Duration;

use common::mock_bulb::{MockBulb, Personality};
use serde_json::{Value, json};
use wizlight::protocol::{Channel, Devices, Dimming, Kelvin, PilotBuilder, Ratio, SceneId, Speed};
use wizlight::{Bulb, Error, RetryPolicy};

async fn connect(bulb: &MockBulb) -> Bulb {
    Bulb::connect_to(bulb.addr()).await.expect("bind socket")
}

/// The last datagram the bulb received, parsed.
///
/// Compared as JSON rather than as bytes: key order is not part of the
/// protocol, and pinning it would mean forcing `serde_json/preserve_order` on
/// every crate in a consumer's build. See the note in `Cargo.toml`.
fn last_request(bulb: &MockBulb) -> Value {
    let raw = bulb.requests().last().cloned().expect("a request was sent");
    serde_json::from_str(&raw).expect("requests are JSON")
}

#[tokio::test]
async fn get_pilot_returns_typed_state() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let pilot = client.get_pilot().await.expect("get_pilot");
    assert_eq!(pilot.mac.as_deref(), Some(bulb.mac()));
    assert_eq!(pilot.state, Some(true));
    assert_eq!(pilot.temp, Some(2700));
    assert_eq!(pilot.dimming, Some(100));
}

#[tokio::test]
async fn set_pilot_rgb_round_trips_and_matches_wire_format() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let builder = PilotBuilder::new()
        .rgb(Channel::new(255), Channel::new(80), Channel::new(0))
        .dimming(Dimming::new(40).unwrap());

    client.set_pilot(&builder).await.expect("set_pilot");
    assert_eq!(
        last_request(&bulb),
        json!({
            "method": "setPilot",
            "params": {"r": 255, "g": 80, "b": 0, "dimming": 40},
        })
    );

    let pilot = client.get_pilot().await.expect("get_pilot");
    assert_eq!(pilot.rgb(), Some((255, 80, 0)));
    assert_eq!(pilot.dimming, Some(40));
    assert_eq!(pilot.state, Some(true));
    assert!(pilot.temp.is_none());
}

#[tokio::test]
async fn set_pilot_rgbw_puts_the_white_channel_on_the_wire() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let builder = PilotBuilder::new().rgbw(
        Channel::new(10),
        Channel::new(20),
        Channel::new(30),
        Channel::new(40),
    );
    client.set_pilot(&builder).await.expect("set_pilot");

    assert_eq!(
        last_request(&bulb),
        json!({
            "method": "setPilot",
            "params": {"r": 10, "g": 20, "b": 30, "w": 40},
        })
    );

    let pilot = client.get_pilot().await.expect("get_pilot");
    assert_eq!(pilot.rgbw(), Some((10, 20, 30, 40)));
}

#[tokio::test]
async fn set_state_uses_the_set_state_method() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let builder = PilotBuilder::new().temp(Kelvin::new(4000).unwrap());
    client.set_state(&builder).await.expect("set_state");

    assert_eq!(
        last_request(&bulb),
        json!({"method": "setState", "params": {"temp": 4000}})
    );
    let pilot = client.get_pilot().await.expect("get_pilot");
    assert_eq!(pilot.temp, Some(4000));
    assert!(pilot.r.is_none());
}

#[tokio::test]
async fn scene_and_speed_round_trip() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let builder = PilotBuilder::new()
        .scene(SceneId::new(4))
        .speed(Speed::new(100).unwrap());
    client.set_pilot(&builder).await.expect("set_pilot");

    let pilot = client.get_pilot().await.expect("get_pilot");
    assert_eq!(pilot.scene_id, Some(4));
    assert_eq!(pilot.speed, Some(100));
    assert!(pilot.temp.is_none());
    assert!(pilot.r.is_none());
}

#[tokio::test]
async fn ratio_and_devices_reach_a_dual_head_bulb() {
    let bulb = MockBulb::builder()
        .personality(Personality::dual_head())
        .start()
        .await;
    let client = connect(&bulb).await;

    let builder = PilotBuilder::new()
        .state(true)
        .ratio(Ratio::new(75).unwrap())
        .devices(Devices::new(2).unwrap());
    client.set_pilot(&builder).await.expect("set_pilot");

    assert_eq!(
        last_request(&bulb),
        json!({
            "method": "setPilot",
            "params": {"state": true, "ratio": 75, "devices": 2},
        })
    );

    let pilot = client.get_pilot().await.expect("get_pilot");
    assert_eq!(pilot.ratio, Some(75));
    assert_eq!(pilot.devices, Some(2));
}

#[tokio::test]
async fn a_dimmable_white_bulb_takes_dimming_and_reports_its_range() {
    let bulb = MockBulb::builder()
        .personality(Personality::dimmable_white())
        .start()
        .await;
    let client = connect(&bulb).await;

    let system = client.get_system_config().await.expect("system");
    assert_eq!(system.module_name.as_deref(), Some("ESP06_SHDW9_01"));

    client
        .set_pilot(&PilotBuilder::new().dimming(Dimming::new(25).unwrap()))
        .await
        .expect("set_pilot");
    assert_eq!(client.get_pilot().await.unwrap().dimming, Some(25));

    // No getModelConfig on 1.11.7, so the range comes from getUserConfig.
    assert_eq!(
        client.kelvin_range().await.expect("range"),
        Some((2700, 6500))
    );
}

#[tokio::test]
async fn colour_modes_are_mutually_exclusive_on_the_bulb() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    client
        .set_pilot(&PilotBuilder::new().rgb(Channel::new(10), Channel::new(20), Channel::new(30)))
        .await
        .unwrap();
    client
        .set_pilot(&PilotBuilder::new().temp(Kelvin::new(3000).unwrap()))
        .await
        .unwrap();

    let pilot = client.get_pilot().await.unwrap();
    assert_eq!(pilot.temp, Some(3000));
    assert!(pilot.r.is_none());
    assert_eq!(pilot.scene_id, Some(0));
}

#[tokio::test]
async fn a_conflicting_builder_never_reaches_the_wire() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let err = client
        .set_pilot(
            &PilotBuilder::new()
                .rgb(Channel::new(10), Channel::new(20), Channel::new(30))
                .temp(Kelvin::new(3000).unwrap()),
        )
        .await
        .expect_err("colour and temp together");
    assert!(matches!(err, Error::InvalidParam { .. }), "{err}");
    assert!(bulb.requests().is_empty());
}

#[tokio::test]
async fn empty_builder_is_rejected_before_the_wire() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let err = client
        .set_pilot(&PilotBuilder::new())
        .await
        .expect_err("empty builder");
    assert!(matches!(err, Error::InvalidParam { .. }));
    assert!(bulb.requests().is_empty());
}

#[tokio::test]
async fn system_model_and_user_config_parse() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let system = client.get_system_config().await.expect("system");
    assert_eq!(system.module_name.as_deref(), Some("ESP25_SHRGB_01"));
    assert_eq!(system.fw_version.as_deref(), Some("1.38.0"));
    assert_eq!(system.mac.as_deref(), Some(bulb.mac()));

    let model = client.get_model_config().await.expect("model");
    assert_eq!(model.kelvin_range(), Some((2200, 6500)));
    assert_eq!(model.wcr, Some(80));

    let user = client.get_user_config().await.expect("user");
    assert_eq!(user.dft_dim, Some(100));
}

#[tokio::test]
async fn kelvin_range_uses_model_config_when_present() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;
    assert_eq!(
        client.kelvin_range().await.expect("range"),
        Some((2200, 6500))
    );
}

#[tokio::test]
async fn kelvin_range_reads_a_legacy_model_config() {
    // ESP01_SHRGB_03 has getModelConfig but none of the fields 1.38.0 added,
    // and no getUserConfig at all — so the range has to come from the former.
    let bulb = MockBulb::builder()
        .personality(Personality::rgb_legacy())
        .start()
        .await;
    let client = connect(&bulb).await;

    assert!(matches!(
        client.get_user_config().await,
        Err(Error::NotSupported { .. })
    ));
    assert_eq!(
        client.kelvin_range().await.expect("range"),
        Some((2200, 6500))
    );
}

#[tokio::test]
async fn kelvin_range_falls_back_to_user_config() {
    let bulb = MockBulb::builder()
        .personality(Personality::tunable_white())
        .start()
        .await;
    let client = connect(&bulb).await;

    let err = client
        .get_model_config()
        .await
        .expect_err("no model config");
    assert!(matches!(err, Error::NotSupported { .. }));

    assert_eq!(
        client.kelvin_range().await.expect("range"),
        Some((2700, 6500))
    );
}

#[tokio::test]
async fn get_power_and_maintenance_methods() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let power = client.get_power().await.expect("power");
    assert_eq!(power.power, 0);

    client.reboot().await.expect("reboot");
    client.reset().await.expect("reset");

    let sent: Vec<Value> = bulb
        .requests()
        .iter()
        .map(|r| serde_json::from_str(r).expect("requests are JSON"))
        .collect();
    assert!(sent.contains(&json!({"method": "reboot", "params": {}})));
    assert!(sent.contains(&json!({"method": "reset", "params": {}})));
}

#[tokio::test]
async fn reboot_and_reset_forgive_a_bulb_that_says_nothing() {
    // Neither method has been run against hardware, and a device that is
    // rebooting or wiping itself has every reason not to answer. Silence must
    // not look like a failure. Nothing is bound to this address.
    let dead = {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
        socket.local_addr().expect("local_addr")
    };
    let client = Bulb::connect_to(dead)
        .await
        .expect("bind socket")
        .with_policy(RetryPolicy {
            attempts: 1,
            attempt_timeout: Duration::from_millis(50),
            min_interval: Duration::from_millis(1),
        });

    client
        .reboot()
        .await
        .expect("a silent reboot is still a reboot");
    client
        .reset()
        .await
        .expect("a silent reset is still a reset");

    // A read, by contrast, has nothing to return and must still time out.
    assert!(matches!(
        client.get_pilot().await,
        Err(Error::Timeout { .. })
    ));
}

#[tokio::test]
async fn get_power_missing_on_legacy_personalities() {
    let bulb = MockBulb::builder()
        .personality(Personality::rgb_legacy())
        .start()
        .await;
    let client = connect(&bulb).await;

    let err = client.get_power().await.expect_err("no power");
    assert!(matches!(err, Error::NotSupported { .. }));
}

#[tokio::test]
async fn socket_personality_reports_power() {
    let bulb = MockBulb::builder()
        .personality(Personality::socket())
        .start()
        .await;
    let client = connect(&bulb).await;
    assert_eq!(client.get_power().await.unwrap().power, 1_065_385);
}

#[tokio::test]
async fn pilot_survives_missing_optional_fields() {
    let bulb = MockBulb::builder()
        .pilot(json!({
            "mac": "aabbccddeeff",
            "state": false,
            "dimming": 10
        }))
        .start()
        .await;
    let client = connect(&bulb).await;
    let pilot = client.get_pilot().await.unwrap();
    assert_eq!(pilot.state, Some(false));
    assert_eq!(pilot.dimming, Some(10));
    assert!(pilot.temp.is_none());
    assert!(pilot.r.is_none());
    assert!(pilot.scene_id.is_none());
}

#[tokio::test]
async fn a_reported_dimming_the_builder_would_refuse_still_parses() {
    let bulb = MockBulb::builder()
        .pilot(json!({"mac": "aabbccddeeff", "state": false, "dimming": 0}))
        .start()
        .await;
    let client = connect(&bulb).await;
    assert_eq!(client.get_pilot().await.unwrap().dimming, Some(0));
    assert!(Dimming::new(0).is_err());
}
