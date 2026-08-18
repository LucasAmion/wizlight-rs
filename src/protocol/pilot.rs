//! Typed `setPilot` / `setState` params and the `getPilot` result.

use serde::{Deserialize, Serialize};

use super::types::{Channel, Devices, Dimming, Kelvin, Ratio, SceneId, Speed};
use crate::error::{Error, Result};
use crate::protocol::Request;

/// Which colour-bearing field a [`PilotBuilder`] will put on the wire.
///
/// `r`/`g`/`b`/`c`/`w`, `temp` and `sceneId` are mutually exclusive in a single
/// request: the bulb honours whichever it finds, and a later mode clears the
/// previous one from `getPilot`. Rather than guess which one the caller meant,
/// the builder records the clash and [`PilotBuilder::params`] refuses to
/// produce anything at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColourMode {
    /// Raw channels. `r`/`g`/`b` and `c`/`w` compose, and either half may be
    /// sent without the other.
    Channels {
        r: Option<Channel>,
        g: Option<Channel>,
        b: Option<Channel>,
        c: Option<Channel>,
        w: Option<Channel>,
    },
    Temp(Kelvin),
    Scene(SceneId),
}

impl ColourMode {
    const CHANNELS: &'static str = "r/g/b/c/w";

    const fn empty_channels() -> Self {
        Self::Channels {
            r: None,
            g: None,
            b: None,
            c: None,
            w: None,
        }
    }

    /// How this mode is named in a conflict message.
    const fn name(self) -> &'static str {
        match self {
            Self::Channels { .. } => Self::CHANNELS,
            Self::Temp(_) => "temp",
            Self::Scene(_) => "sceneId",
        }
    }

    /// True for the placeholder created before any channel is filled in.
    const fn is_empty(self) -> bool {
        matches!(
            self,
            Self::Channels {
                r: None,
                g: None,
                b: None,
                c: None,
                w: None
            }
        )
    }
}

/// Builds the params object for `setPilot` or `setState`.
///
/// One builder, two messages: [`PilotBuilder::set_pilot`] and
/// [`PilotBuilder::set_state`] differ only in the method name. On the hardware
/// measured so far (`ESP25_SHRGB_01` fw 1.38.0) both turn the bulb on when the
/// request carries colour, temperature or a scene — the historical claim that
/// `setState` leaves power alone does not hold.
///
/// Colour, colour temperature and scene are mutually exclusive. Asking for two
/// of them is an error at build time rather than a silent choice between them,
/// because a caller that sets both has a bug and a bulb that receives both has
/// no defined behaviour:
///
/// ```
/// use wizlight::protocol::{Channel, Kelvin, PilotBuilder};
///
/// let clash = PilotBuilder::new()
///     .rgb(Channel::new(255), Channel::new(0), Channel::new(0))
///     .temp(Kelvin::new(2700)?)
///     .set_pilot();
/// assert!(clash.is_err());
/// # Ok::<(), wizlight::Error>(())
/// ```
///
/// Fields that do not carry colour — `state`, `dimming`, `speed`, `ratio`,
/// `devices` — compose with any of the three:
///
/// ```
/// use wizlight::protocol::{Channel, Dimming, PilotBuilder};
///
/// let request = PilotBuilder::new()
///     .rgb(Channel::new(255), Channel::new(80), Channel::new(0))
///     .dimming(Dimming::new(40)?)
///     .set_pilot()?;
/// assert_eq!(
///     serde_json::to_value(&request)?,
///     serde_json::json!({
///         "method": "setPilot",
///         "params": {"r": 255, "g": 80, "b": 0, "dimming": 40},
///     }),
/// );
/// # Ok::<(), wizlight::Error>(())
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PilotBuilder {
    state: Option<bool>,
    dimming: Option<Dimming>,
    speed: Option<Speed>,
    ratio: Option<Ratio>,
    devices: Option<Devices>,
    colour: Option<ColourMode>,
    /// The first clash seen, as `(rejected, already set)`. Reported by
    /// [`PilotBuilder::params`]; kept rather than acted on so the setters can
    /// stay chainable and infallible.
    conflict: Option<(&'static str, &'static str)>,
}

impl PilotBuilder {
    /// An empty builder. Building it without any field set is an error: the
    /// bulb rejects both a missing and an empty `params` object.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets `state`.
    #[must_use]
    pub fn state(mut self, on: bool) -> Self {
        self.state = Some(on);
        self
    }

    /// Sets `dimming` (1–100).
    #[must_use]
    pub fn dimming(mut self, dimming: Dimming) -> Self {
        self.dimming = Some(dimming);
        self
    }

    /// Sets scene animation `speed` (10–200).
    #[must_use]
    pub fn speed(mut self, speed: Speed) -> Self {
        self.speed = Some(speed);
        self
    }

    /// Sets dual-head `ratio` (0–100).
    #[must_use]
    pub fn ratio(mut self, ratio: Ratio) -> Self {
        self.ratio = Some(ratio);
        self
    }

    /// Sets the dual-head `devices` selector.
    #[must_use]
    pub fn devices(mut self, devices: Devices) -> Self {
        self.devices = Some(devices);
        self
    }

    /// Sets `r`/`g`/`b`.
    ///
    /// Does **not** run RGB→RGB+CW conversion; that is a separate concern.
    /// Send raw channels, or add cold/warm white with
    /// [`cold_white`](PilotBuilder::cold_white) /
    /// [`warm_white`](PilotBuilder::warm_white).
    ///
    /// Conflicts with [`temp`](PilotBuilder::temp) and
    /// [`scene`](PilotBuilder::scene).
    #[must_use]
    pub fn rgb(mut self, r: Channel, g: Channel, b: Channel) -> Self {
        if let Some(ColourMode::Channels {
            r: rs,
            g: gs,
            b: bs,
            ..
        }) = self.channels()
        {
            *rs = Some(r);
            *gs = Some(g);
            *bs = Some(b);
        }
        self
    }

    /// Sets `r`/`g`/`b`/`w`.
    #[must_use]
    pub fn rgbw(self, r: Channel, g: Channel, b: Channel, w: Channel) -> Self {
        self.rgb(r, g, b).warm_white(w)
    }

    /// Sets `r`/`g`/`b`/`c`/`w`.
    #[must_use]
    pub fn rgbww(self, r: Channel, g: Channel, b: Channel, c: Channel, w: Channel) -> Self {
        self.rgb(r, g, b).cold_white(c).warm_white(w)
    }

    /// Sets cold white (`c`), with or without an RGB triple.
    ///
    /// Conflicts with [`temp`](PilotBuilder::temp) and
    /// [`scene`](PilotBuilder::scene).
    #[must_use]
    pub fn cold_white(mut self, c: Channel) -> Self {
        if let Some(ColourMode::Channels { c: slot, .. }) = self.channels() {
            *slot = Some(c);
        }
        self
    }

    /// Sets warm white (`w`), with or without an RGB triple.
    ///
    /// Conflicts with [`temp`](PilotBuilder::temp) and
    /// [`scene`](PilotBuilder::scene).
    #[must_use]
    pub fn warm_white(mut self, w: Channel) -> Self {
        if let Some(ColourMode::Channels { w: slot, .. }) = self.channels() {
            *slot = Some(w);
        }
        self
    }

    /// Sets `temp`.
    ///
    /// Conflicts with the raw channels and with
    /// [`scene`](PilotBuilder::scene).
    #[must_use]
    pub fn temp(mut self, temp: Kelvin) -> Self {
        self.set_colour(ColourMode::Temp(temp));
        self
    }

    /// Sets `sceneId`.
    ///
    /// Conflicts with the raw channels and with [`temp`](PilotBuilder::temp).
    #[must_use]
    pub fn scene(mut self, scene: SceneId) -> Self {
        self.set_colour(ColourMode::Scene(scene));
        self
    }

    /// Serialises into a `setPilot` request.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidParam`] if two colour modes were set, or if no
    /// field was set at all — the bulb refuses an empty params object.
    pub fn set_pilot(&self) -> Result<Request> {
        self.to_request("setPilot")
    }

    /// Serialises into a `setState` request.
    ///
    /// Same params shape as [`PilotBuilder::set_pilot`]. On measured firmware
    /// this still turns the bulb on when colour / temp / scene is present.
    ///
    /// # Errors
    ///
    /// As [`set_pilot`](PilotBuilder::set_pilot).
    pub fn set_state(&self) -> Result<Request> {
        self.to_request("setState")
    }

    /// The params object alone, for callers that want to inspect or merge it.
    ///
    /// # Errors
    ///
    /// As [`set_pilot`](PilotBuilder::set_pilot).
    pub fn params(&self) -> Result<PilotParams> {
        if let Some((rejected, existing)) = self.conflict {
            return Err(Error::InvalidParam {
                message: format!(
                    "`{rejected}` conflicts with `{existing}`: colour, colour temperature \
                     and scene are mutually exclusive in one request"
                ),
            });
        }

        if self.is_empty() {
            return Err(Error::InvalidParam {
                message: "pilot params must set at least one field".into(),
            });
        }

        let mut params = PilotParams {
            state: self.state,
            dimming: self.dimming,
            speed: self.speed,
            ratio: self.ratio,
            devices: self.devices,
            ..PilotParams::default()
        };

        match self.colour {
            Some(ColourMode::Channels { r, g, b, c, w }) => {
                params.r = r;
                params.g = g;
                params.b = b;
                params.c = c;
                params.w = w;
            }
            Some(ColourMode::Temp(temp)) => params.temp = Some(temp),
            Some(ColourMode::Scene(scene)) => params.scene_id = Some(scene),
            None => {}
        }

        Ok(params)
    }

    /// The channel mode to write into, or `None` if a different colour mode is
    /// already set — in which case the clash has been recorded.
    fn channels(&mut self) -> Option<&mut ColourMode> {
        match self.colour {
            Some(ColourMode::Channels { .. }) => self.colour.as_mut(),
            Some(other) => {
                self.note_conflict(ColourMode::CHANNELS, other.name());
                None
            }
            None => {
                self.colour = Some(ColourMode::empty_channels());
                self.colour.as_mut()
            }
        }
    }

    /// Sets a whole-mode colour field, recording a clash with any other mode.
    /// Replacing a mode with the same kind — two `temp` calls — is not a
    /// clash; the last one wins, as with every other setter.
    fn set_colour(&mut self, mode: ColourMode) {
        match self.colour {
            Some(existing) if existing.name() != mode.name() => {
                self.note_conflict(mode.name(), existing.name());
            }
            _ => self.colour = Some(mode),
        }
    }

    /// Keeps the first clash: it is the one that explains what the caller did.
    fn note_conflict(&mut self, rejected: &'static str, existing: &'static str) {
        self.conflict.get_or_insert((rejected, existing));
    }

    fn is_empty(&self) -> bool {
        self.state.is_none()
            && self.dimming.is_none()
            && self.speed.is_none()
            && self.ratio.is_none()
            && self.devices.is_none()
            && self.colour.is_none_or(ColourMode::is_empty)
    }

    fn to_request(&self, method: &str) -> Result<Request> {
        Request::with_params(method, &self.params()?)
    }
}

/// The params object of a `setPilot` / `setState` request.
///
/// Absent fields are omitted on the wire. Construct this through
/// [`PilotBuilder`] so mutual exclusion and ranges are enforced.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PilotParams {
    /// On/off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<bool>,
    /// Red channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r: Option<Channel>,
    /// Green channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub g: Option<Channel>,
    /// Blue channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b: Option<Channel>,
    /// Cold white channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub c: Option<Channel>,
    /// Warm white channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub w: Option<Channel>,
    /// Brightness percent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimming: Option<Dimming>,
    /// Colour temperature in Kelvin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp: Option<Kelvin>,
    /// Scene id.
    #[serde(rename = "sceneId", skip_serializing_if = "Option::is_none")]
    pub scene_id: Option<SceneId>,
    /// Scene animation speed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<Speed>,
    /// Dual-head ratio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio: Option<Ratio>,
    /// Dual-head device selector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub devices: Option<Devices>,
}

/// The `result` of a successful `getPilot`, and the `params` of a `syncPilot`
/// push.
///
/// Every field is optional: the bulb only returns what the active mode needs,
/// and firmware revisions add keys. A missing field is `None`, never a parse
/// error. Values are the plain integers the bulb reported, not the validated
/// newtypes a request uses — a bulb may report what it would not accept.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct Pilot {
    /// Bulb MAC, lowercase hex with no separators.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    /// Received signal strength, dBm.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rssi: Option<i32>,
    /// Whether the bulb is on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<bool>,
    /// Red channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r: Option<u8>,
    /// Green channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub g: Option<u8>,
    /// Blue channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub b: Option<u8>,
    /// Cold white channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub c: Option<u8>,
    /// Warm white channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub w: Option<u8>,
    /// Brightness percent. Present even when the bulb is off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimming: Option<u8>,
    /// Colour temperature in Kelvin, when in white mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temp: Option<u16>,
    /// Active scene. `0` means "no scene" while RGB is active.
    #[serde(rename = "sceneId", skip_serializing_if = "Option::is_none")]
    pub scene_id: Option<u16>,
    /// Scene animation speed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<u8>,
    /// Dual-head ratio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio: Option<u8>,
    /// Dual-head head tag. Zero-based on a `getPilot` answer, one-based in
    /// `syncPilot` push traffic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub devices: Option<u8>,
    /// Where the state came from (`udp`, `hb`, `wizc1`, …). Push only.
    #[serde(rename = "src", skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl Pilot {
    /// RGB triple when all three channels are present.
    #[must_use]
    pub fn rgb(&self) -> Option<(u8, u8, u8)> {
        Some((self.r?, self.g?, self.b?))
    }

    /// RGBW when `r`/`g`/`b`/`w` are all present.
    #[must_use]
    pub fn rgbw(&self) -> Option<(u8, u8, u8, u8)> {
        Some((self.r?, self.g?, self.b?, self.w?))
    }

    /// RGBWW when `r`/`g`/`b`/`c`/`w` are all present.
    #[must_use]
    pub fn rgbww(&self) -> Option<(u8, u8, u8, u8, u8)> {
        Some((self.r?, self.g?, self.b?, self.c?, self.w?))
    }
}

/// `{"success": true}` — the usual write acknowledgement.
///
/// The convenience methods on [`Bulb`](crate::Bulb) check this and turn a
/// `false` into an error, so it is only interesting to callers driving
/// [`Response::parse_result`](super::Response::parse_result) themselves.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Success {
    /// Whether the bulb accepted the write.
    pub success: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wire(request: &Request) -> serde_json::Value {
        serde_json::to_value(request).expect("a request always serialises")
    }

    #[test]
    fn conflicting_colour_modes_are_rejected() {
        let cases = [
            PilotBuilder::new().temp(Kelvin::new(2700).unwrap()).rgb(
                Channel::new(1),
                Channel::new(2),
                Channel::new(3),
            ),
            PilotBuilder::new()
                .rgb(Channel::new(1), Channel::new(2), Channel::new(3))
                .temp(Kelvin::new(4000).unwrap()),
            PilotBuilder::new()
                .temp(Kelvin::new(4000).unwrap())
                .scene(SceneId::new(4)),
            PilotBuilder::new().scene(SceneId::new(4)).rgb(
                Channel::new(1),
                Channel::new(2),
                Channel::new(3),
            ),
            // Standalone white channels are part of the channel mode, so they
            // clash with temp exactly as an RGB triple would.
            PilotBuilder::new()
                .temp(Kelvin::new(4000).unwrap())
                .warm_white(Channel::new(10)),
        ];

        for builder in cases {
            let err = builder.set_pilot().unwrap_err();
            assert!(
                matches!(&err, Error::InvalidParam { message } if message.contains("mutually exclusive")),
                "{err}"
            );
        }
    }

    #[test]
    fn the_conflict_message_names_both_modes() {
        let err = PilotBuilder::new()
            .temp(Kelvin::new(2700).unwrap())
            .scene(SceneId::new(4))
            .set_pilot()
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("`sceneId`"), "{message}");
        assert!(message.contains("`temp`"), "{message}");
    }

    #[test]
    fn resetting_the_same_mode_is_not_a_conflict() {
        let params = PilotBuilder::new()
            .temp(Kelvin::new(2700).unwrap())
            .temp(Kelvin::new(4000).unwrap())
            .params()
            .unwrap();
        assert_eq!(params.temp.unwrap().get(), 4000);

        let params = PilotBuilder::new()
            .rgb(Channel::new(1), Channel::new(2), Channel::new(3))
            .rgb(Channel::new(4), Channel::new(5), Channel::new(6))
            .params()
            .unwrap();
        assert_eq!(params.r.unwrap().get(), 4);
    }

    #[test]
    fn non_colour_fields_compose_with_every_mode() {
        let params = PilotBuilder::new()
            .scene(SceneId::new(4))
            .speed(Speed::new(100).unwrap())
            .dimming(Dimming::new(40).unwrap())
            .state(true)
            .params()
            .unwrap();
        assert_eq!(params.scene_id.unwrap().get(), 4);
        assert_eq!(params.speed.unwrap().get(), 100);
        assert_eq!(params.dimming.unwrap().get(), 40);
        assert_eq!(params.state, Some(true));
    }

    #[test]
    fn white_channels_compose_with_rgb_in_either_order() {
        let after = PilotBuilder::new()
            .rgb(Channel::new(1), Channel::new(2), Channel::new(3))
            .cold_white(Channel::new(4))
            .warm_white(Channel::new(5))
            .params()
            .unwrap();
        let before = PilotBuilder::new()
            .cold_white(Channel::new(4))
            .warm_white(Channel::new(5))
            .rgb(Channel::new(1), Channel::new(2), Channel::new(3))
            .params()
            .unwrap();
        assert_eq!(after, before);
        assert_eq!(after.c.unwrap().get(), 4);
        assert_eq!(after.w.unwrap().get(), 5);
        assert_eq!(after.r.unwrap().get(), 1);
    }

    #[test]
    fn empty_builder_is_rejected() {
        let err = PilotBuilder::new().set_pilot().unwrap_err();
        assert!(matches!(err, Error::InvalidParam { .. }));
    }

    #[test]
    fn wire_format_matches_recorded_traffic() {
        let cases = [
            (
                PilotBuilder::new().state(true).set_pilot().unwrap(),
                json!({"method": "setPilot", "params": {"state": true}}),
            ),
            (
                PilotBuilder::new()
                    .dimming(Dimming::new(40).unwrap())
                    .set_pilot()
                    .unwrap(),
                json!({"method": "setPilot", "params": {"dimming": 40}}),
            ),
            (
                PilotBuilder::new()
                    .rgb(Channel::new(255), Channel::new(80), Channel::new(0))
                    .set_pilot()
                    .unwrap(),
                json!({"method": "setPilot", "params": {"r": 255, "g": 80, "b": 0}}),
            ),
            (
                PilotBuilder::new()
                    .rgbw(
                        Channel::new(255),
                        Channel::new(80),
                        Channel::new(0),
                        Channel::new(12),
                    )
                    .set_pilot()
                    .unwrap(),
                json!({"method": "setPilot", "params": {"r": 255, "g": 80, "b": 0, "w": 12}}),
            ),
            (
                PilotBuilder::new()
                    .rgbww(
                        Channel::new(0),
                        Channel::new(128),
                        Channel::new(255),
                        Channel::new(0),
                        Channel::new(0),
                    )
                    .set_pilot()
                    .unwrap(),
                json!({
                    "method": "setPilot",
                    "params": {"r": 0, "g": 128, "b": 255, "c": 0, "w": 0},
                }),
            ),
            (
                PilotBuilder::new()
                    .warm_white(Channel::new(200))
                    .set_pilot()
                    .unwrap(),
                json!({"method": "setPilot", "params": {"w": 200}}),
            ),
            (
                PilotBuilder::new()
                    .temp(Kelvin::new(5000).unwrap())
                    .set_pilot()
                    .unwrap(),
                json!({"method": "setPilot", "params": {"temp": 5000}}),
            ),
            (
                PilotBuilder::new()
                    .scene(SceneId::new(4))
                    .speed(Speed::new(100).unwrap())
                    .set_pilot()
                    .unwrap(),
                json!({"method": "setPilot", "params": {"sceneId": 4, "speed": 100}}),
            ),
            (
                PilotBuilder::new()
                    .state(true)
                    .ratio(Ratio::new(75).unwrap())
                    .devices(Devices::new(2).unwrap())
                    .set_pilot()
                    .unwrap(),
                json!({
                    "method": "setPilot",
                    "params": {"state": true, "ratio": 75, "devices": 2},
                }),
            ),
            (
                PilotBuilder::new()
                    .rgb(Channel::new(255), Channel::new(0), Channel::new(0))
                    .set_state()
                    .unwrap(),
                json!({"method": "setState", "params": {"r": 255, "g": 0, "b": 0}}),
            ),
        ];

        for (request, expected) in cases {
            assert_eq!(wire(&request), expected);
        }
    }

    #[test]
    fn pilot_parses_partial_results() {
        let colour: Pilot = serde_json::from_str(
            r#"{"mac":"9877d5230f0a","rssi":-53,"state":true,"sceneId":0,"r":255,"g":0,"b":0,"c":0,"w":0,"dimming":40}"#,
        )
        .unwrap();
        assert_eq!(colour.rgb(), Some((255, 0, 0)));
        assert_eq!(colour.rgbw(), Some((255, 0, 0, 0)));
        assert_eq!(colour.rgbww(), Some((255, 0, 0, 0, 0)));
        assert!(colour.temp.is_none());

        let white: Pilot = serde_json::from_str(
            r#"{"mac":"9877d5230f0a","state":true,"sceneId":11,"temp":2700,"dimming":100}"#,
        )
        .unwrap();
        assert_eq!(white.temp, Some(2700));
        assert!(white.r.is_none());
        assert_eq!(white.scene_id, Some(11));
        assert_eq!(white.rgb(), None);
        assert_eq!(white.rgbw(), None);
    }

    #[test]
    fn a_reported_value_the_builder_would_refuse_still_parses() {
        // `Dimming` refuses 0, so parsing must not. The measured hardware
        // clamps and never reports 0 itself, but it does *accept* 0, so a
        // model that echoed it back would still be within the protocol —
        // results must not inherit the write-side bound.
        let pilot: Pilot = serde_json::from_str(r#"{"state":false,"dimming":0}"#).unwrap();
        assert_eq!(pilot.dimming, Some(0));
    }
}
