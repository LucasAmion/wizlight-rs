//! Typed protocol methods and the pilot builder against the mock bulb.

mod common;

use std::time::Duration;

use common::mock_bulb::{MockBulb, Personality};
use serde_json::{Value, json};
use wizlight::protocol::{
    BulbClass, Channel, Derivation, Devices, Dimming, Kelvin, KelvinRange, PilotBuilder, Ratio,
    SceneId, Speed,
};
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
        Some(KelvinRange::new(2700, 6500))
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
    assert_eq!(model.kelvin_range(), Some(KelvinRange::new(2200, 6500)));
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
        Some(KelvinRange::new(2200, 6500))
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
        Some(KelvinRange::new(2200, 6500))
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
        Some(KelvinRange::new(2700, 6500))
    );
}

#[tokio::test]
async fn bulb_type_describes_the_measured_hardware() {
    // ESP25_SHRGB_01 on 1.38.0. Its getSystemConfig carries neither `drvConf`
    // nor `typeId`, so everything but the module name comes from
    // getModelConfig: `nowc`, `wcr` and the `cctRange` of 2200-6500.
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let bulb_type = client.bulb_type().await.expect("bulb type");
    assert_eq!(bulb_type.class, BulbClass::Rgb);
    assert_eq!(bulb_type.derivation, Derivation::ModuleName);
    assert_eq!(
        bulb_type.module_name.as_ref().map(ToString::to_string),
        Some("ESP25_SHRGB_01".to_owned())
    );
    assert_eq!(bulb_type.fw_version.as_deref(), Some("1.38.0"));
    assert_eq!(bulb_type.kelvin_range, Some(KelvinRange::new(2200, 6500)));
    assert_eq!(bulb_type.white_channels, Some(1));
    assert_eq!(bulb_type.white_to_color_ratio, Some(80));
    assert_eq!(bulb_type.fan_speed_range, None);

    let features = bulb_type.features;
    assert!(features.color && features.color_tmp && features.effect && features.brightness);
    assert!(!features.dual_head && !features.fan);
}

#[tokio::test]
async fn bulb_type_reads_white_channels_from_drv_conf_on_old_firmware() {
    // ESP14_SHTW1C_01 on 1.18.0: no getModelConfig, so `nowc` and `wcr` are
    // only available as the two entries of `drvConf`, and the Kelvin range
    // only from getUserConfig.
    let bulb = MockBulb::builder()
        .personality(Personality::tunable_white())
        .start()
        .await;
    let bulb_type = connect(&bulb).await.bulb_type().await.expect("bulb type");

    assert_eq!(bulb_type.class, BulbClass::Tw);
    assert_eq!(bulb_type.kelvin_range, Some(KelvinRange::new(2700, 6500)));
    assert_eq!(bulb_type.white_channels, Some(1));
    assert_eq!(bulb_type.white_to_color_ratio, Some(20));
    assert!(!bulb_type.features.color && bulb_type.features.color_tmp);
}

#[tokio::test]
async fn bulb_type_of_a_dual_head_bulb_reports_two_heads() {
    let bulb = MockBulb::builder()
        .personality(Personality::dual_head())
        .start()
        .await;
    let bulb_type = connect(&bulb).await.bulb_type().await.expect("bulb type");

    assert_eq!(bulb_type.class, BulbClass::Rgb);
    assert!(bulb_type.features.dual_head);
    // `nowc` from getModelConfig wins over the `drvConf` of [30, 1] that the
    // same bulb's getSystemConfig still carries.
    assert_eq!(bulb_type.white_channels, Some(2));
    assert_eq!(bulb_type.white_to_color_ratio, Some(20));
}

#[tokio::test]
async fn a_socket_has_nothing_to_dim() {
    let bulb = MockBulb::builder()
        .personality(Personality::socket())
        .start()
        .await;
    let bulb_type = connect(&bulb).await.bulb_type().await.expect("bulb type");

    assert_eq!(bulb_type.class, BulbClass::Socket);
    let features = bulb_type.features;
    assert!(!features.brightness && !features.color && !features.color_tmp && !features.effect);
}

#[tokio::test]
async fn a_fan_reports_its_speed_range() {
    let bulb = MockBulb::builder()
        .personality(Personality::fan())
        .start()
        .await;
    let bulb_type = connect(&bulb).await.bulb_type().await.expect("bulb type");

    assert_eq!(bulb_type.class, BulbClass::FanDim);
    assert_eq!(bulb_type.fan_speed_range, Some(6));
    // A single-temperature light: the range is four copies of one value.
    assert_eq!(bulb_type.kelvin_range, Some(KelvinRange::new(2700, 2700)));
    assert!(bulb_type.features.fan && bulb_type.features.brightness);
    assert!(!bulb_type.features.effect && !bulb_type.features.color_tmp);
}

#[tokio::test]
async fn firmware_too_old_for_a_module_name_falls_back_to_its_type_id() {
    // 1.8.0 reports no moduleName at all, so the class comes from `typeId: 0`
    // and the white channel count from `drvConf: [20, 1]`.
    let bulb = MockBulb::builder()
        .personality(Personality::firmware_1_8_0())
        .start()
        .await;
    let bulb_type = connect(&bulb).await.bulb_type().await.expect("bulb type");

    assert_eq!(bulb_type.class, BulbClass::Dw);
    assert_eq!(bulb_type.derivation, Derivation::KnownTypeId(0));
    assert_eq!(bulb_type.module_name, None);
    assert_eq!(bulb_type.white_channels, Some(1));
    assert_eq!(bulb_type.white_to_color_ratio, Some(20));
    assert!(bulb_type.features.brightness && bulb_type.features.effect);

    // `pywizlight` reports no range for this fixture, because it reads
    // `extRange` and `cctRange` only while 1.8.0 sends `whiteRange`. Reading
    // the range the bulb did send is a deliberate difference: a reported
    // range beats no range at all.
    assert_eq!(bulb_type.kelvin_range, Some(KelvinRange::new(2700, 2700)));
}

#[tokio::test]
async fn a_bulb_that_cannot_describe_itself_says_so() {
    // Neither a moduleName nor a typeId, and then a module name with no
    // identifier token in it. Both are refused rather than guessed at.
    for system_config in [
        r#"{"method":"getSystemConfig","env":"pro","result":{"mac":"9877d5230f0a","fwVersion":"1.38.0"}}"#,
        r#"{"method":"getSystemConfig","env":"pro","result":{"mac":"9877d5230f0a","moduleName":"INVALID","fwVersion":"1.38.0"}}"#,
    ] {
        let bulb = MockBulb::builder()
            .personality(Personality::rgb().with_system_config(system_config))
            .start()
            .await;
        let err = connect(&bulb)
            .await
            .bulb_type()
            .await
            .expect_err("nothing to go on");
        assert!(matches!(err, Error::UnknownModel { .. }), "{err}");
    }
}

#[tokio::test]
async fn a_colour_bulb_that_reports_no_kelvin_range_is_an_error() {
    // A getModelConfig with no `cctRange` and no getUserConfig to fall back
    // on. An RGB bulb whose usable range is unknown cannot be given a colour
    // temperature safely, so this is refused rather than guessed.
    let bulb = MockBulb::builder()
        .personality(Personality::rgb_legacy().with_model_config(
            r#"{"method":"getModelConfig","env":"pro","result":{"ps":1,"wcr":30,"nowc":1}}"#,
        ))
        .start()
        .await;
    let client = connect(&bulb).await;

    assert_eq!(client.kelvin_range().await.expect("no range"), None);
    let err = client.bulb_type().await.expect_err("no kelvin range");
    assert!(
        err.to_string().contains("must report a Kelvin range"),
        "{err}"
    );
}

#[tokio::test]
async fn get_power_is_answered_by_this_model() {
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let power = client.get_power().await.expect("power");
    assert_eq!(power.power, 0);
}

#[tokio::test]
async fn reboot_is_refused_by_the_hardware_and_says_so() {
    // Measured on ESP25_SHRGB_01 fw 1.38.0: `reboot` comes back -32600
    // Invalid Request and the bulb does not reboot. Not -32601, so the
    // firmware knows the method and declines it. The harness answers the same
    // way, and this test exists so nobody "fixes" it back to success.
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let err = client
        .reboot()
        .await
        .expect_err("the hardware refuses reboot");
    assert!(
        matches!(&err, Error::Device { code: -32600, method, .. } if method == "reboot"),
        "{err}"
    );
    // A refusal must not be mistaken for the silence that fire-and-forget
    // forgives.
    assert!(!matches!(err, Error::Timeout { .. }));

    let sent: Vec<Value> = bulb
        .requests()
        .iter()
        .map(|r| serde_json::from_str(r).expect("requests are JSON"))
        .collect();
    assert!(sent.contains(&json!({"method": "reboot", "params": {}})));
}

#[tokio::test]
async fn reset_is_assumed_to_behave_like_reboot() {
    // Untested on hardware, and staying that way: measuring a factory reset
    // costs a re-paired bulb.
    let bulb = MockBulb::start().await;
    let client = connect(&bulb).await;

    let err = client.reset().await.expect_err("assumed refused");
    assert!(matches!(err, Error::Device { code: -32600, .. }), "{err}");
}

#[tokio::test]
async fn silence_is_still_forgiven_for_reboot_and_reset() {
    // The other half of fire-and-forget: a bulb that really did reboot has an
    // obvious reason not to answer. Nothing is bound to this address.
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
