//! Typed protocol methods and the pilot builder against the mock bulb.

mod common;

use common::mock_bulb::{MockBulb, Personality};
use wizlight::protocol::{Channel, Dimming, Kelvin, PilotBuilder, SceneId, Speed};
use wizlight::{Bulb, Error};

async fn connect(bulb: &MockBulb) -> Bulb {
    Bulb::connect_to(bulb.addr()).await.expect("bind socket")
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
        .rgb(
            Channel::new(255).unwrap(),
            Channel::new(80).unwrap(),
            Channel::new(0).unwrap(),
        )
        .dimming(Dimming::new(40).unwrap());

    let success = client.set_pilot(&builder).await.expect("set_pilot");
    assert!(success.success);
    assert_eq!(
        bulb.requests().last().map(String::as_str),
        Some(r#"{"method":"setPilot","params":{"r":255,"g":80,"b":0,"dimming":40}}"#)
    );

    let pilot = client.get_pilot().await.expect("get_pilot");
    assert_eq!(pilot.rgb(), Some((255, 80, 0)));
    assert_eq!(pilot.dimming, Some(40));
    assert_eq!(pilot.state, Some(true));
    assert!(pilot.temp.is_none());
}

#[tokio::test]
async fn set_state_uses_the_set_state_method() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let builder = PilotBuilder::new().temp(Kelvin::new(4000).unwrap());
    client.set_state(&builder).await.expect("set_state");

    assert_eq!(
        bulb.requests().last().map(String::as_str),
        Some(r#"{"method":"setState","params":{"temp":4000}}"#)
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
        .scene(SceneId::new(4).unwrap())
        .speed(Speed::new(100).unwrap());
    client.set_pilot(&builder).await.expect("set_pilot");

    let pilot = client.get_pilot().await.expect("get_pilot");
    assert_eq!(pilot.scene_id, Some(4));
    assert_eq!(pilot.speed, Some(100));
    assert!(pilot.temp.is_none());
    assert!(pilot.r.is_none());
}

#[tokio::test]
async fn colour_modes_are_mutually_exclusive_on_the_bulb() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    client
        .set_pilot(&PilotBuilder::new().rgb(
            Channel::new(10).unwrap(),
            Channel::new(20).unwrap(),
            Channel::new(30).unwrap(),
        ))
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

    assert!(client.reboot().await.expect("reboot").success);
    assert!(client.reset().await.expect("reset").success);
    assert!(
        bulb.requests()
            .iter()
            .any(|r| r == r#"{"method":"reboot","params":{}}"#)
    );
    assert!(
        bulb.requests()
            .iter()
            .any(|r| r == r#"{"method":"reset","params":{}}"#)
    );
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
        .pilot(serde_json::json!({
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
