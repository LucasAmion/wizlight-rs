//! Typed `setPilot` / `setState` params and the `getPilot` result.

use serde::{Deserialize, Serialize};

use super::types::{Channel, Devices, Dimming, Kelvin, Ratio, SceneId, Speed};
use crate::error::{Error, Result};
use crate::protocol::Request;

/// Which colour-bearing field a [`PilotBuilder`] will put on the wire.
///
/// `temp`, `r/g/b` and `sceneId` are mutually exclusive in a single request —
/// the bulb honours whichever arrives, and a later mode clears the previous one
/// from `getPilot`. The builder enforces the exclusion so the engine never has
/// to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColourMode {
    Rgb {
        r: Channel,
        g: Channel,
        b: Channel,
        c: Option<Channel>,
        w: Option<Channel>,
    },
    Temp(Kelvin),
    Scene(SceneId),
}

/// Builds the params object for `setPilot` or `setState`.
///
/// One builder, two messages: [`PilotBuilder::set_pilot`] and
/// [`PilotBuilder::set_state`] differ only in the method name. On the hardware
/// measured so far (`ESP25_SHRGB_01` fw 1.38.0) both turn the bulb on when the
/// request carries colour, temperature or a scene — the historical claim that
/// `setState` leaves power alone does not hold.
///
/// ```
/// use wizlight::protocol::{Channel, Dimming, PilotBuilder};
///
/// let request = PilotBuilder::new()
///     .rgb(
///         Channel::new(255)?,
///         Channel::new(80)?,
///         Channel::new(0)?,
///     )
///     .dimming(Dimming::new(40)?)
///     .set_pilot()?;
/// assert_eq!(
///     request.to_string(),
///     r#"{"method":"setPilot","params":{"r":255,"g":80,"b":0,"dimming":40}}"#
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
    /// Cold/warm white set without an RGB triple — allowed alongside nothing,
    /// or as the only colour-ish fields.
    cold_white: Option<Channel>,
    warm_white: Option<Channel>,
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

    /// Sets `r`/`g`/`b`, clearing any temperature or scene previously set on
    /// this builder.
    ///
    /// Does **not** run RGB→RGB+CW conversion; that is a separate concern. Send
    /// raw channels, or add cold/warm white with [`PilotBuilder::cold_white`] /
    /// [`PilotBuilder::warm_white`].
    #[must_use]
    pub fn rgb(mut self, r: Channel, g: Channel, b: Channel) -> Self {
        let (c, w) = match self.colour {
            Some(ColourMode::Rgb { c, w, .. }) => (c, w),
            _ => (self.cold_white, self.warm_white),
        };
        self.cold_white = None;
        self.warm_white = None;
        self.colour = Some(ColourMode::Rgb { r, g, b, c, w });
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

    /// Sets cold white (`c`). Cleared if a temperature or scene is set later.
    #[must_use]
    pub fn cold_white(mut self, c: Channel) -> Self {
        match &mut self.colour {
            Some(ColourMode::Rgb { c: slot, .. }) => *slot = Some(c),
            Some(ColourMode::Temp(_) | ColourMode::Scene(_)) => {
                self.colour = None;
                self.cold_white = Some(c);
            }
            None => self.cold_white = Some(c),
        }
        self
    }

    /// Sets warm white (`w`). Cleared if a temperature or scene is set later.
    #[must_use]
    pub fn warm_white(mut self, w: Channel) -> Self {
        match &mut self.colour {
            Some(ColourMode::Rgb { w: slot, .. }) => *slot = Some(w),
            Some(ColourMode::Temp(_) | ColourMode::Scene(_)) => {
                self.colour = None;
                self.warm_white = Some(w);
            }
            None => self.warm_white = Some(w),
        }
        self
    }

    /// Sets `temp`, clearing any RGB or scene previously set on this builder.
    #[must_use]
    pub fn temp(mut self, temp: Kelvin) -> Self {
        self.colour = Some(ColourMode::Temp(temp));
        self.cold_white = None;
        self.warm_white = None;
        self
    }

    /// Sets `sceneId`, clearing any RGB or temperature previously set on this
    /// builder.
    #[must_use]
    pub fn scene(mut self, scene: SceneId) -> Self {
        self.colour = Some(ColourMode::Scene(scene));
        self.cold_white = None;
        self.warm_white = None;
        self
    }

    /// Serialises into a `setPilot` request.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidParam`] if no field was set — an empty params
    /// object is refused by the bulb.
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
    /// Returns [`Error::InvalidParam`] if no field was set.
    pub fn set_state(&self) -> Result<Request> {
        self.to_request("setState")
    }

    /// The params object alone, for callers that want to inspect or merge it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidParam`] if no field was set.
    pub fn params(&self) -> Result<PilotParams> {
        if self.state.is_none()
            && self.dimming.is_none()
            && self.speed.is_none()
            && self.ratio.is_none()
            && self.devices.is_none()
            && self.colour.is_none()
            && self.cold_white.is_none()
            && self.warm_white.is_none()
        {
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
            Some(ColourMode::Rgb { r, g, b, c, w }) => {
                params.r = Some(r);
                params.g = Some(g);
                params.b = Some(b);
                params.c = c.or(self.cold_white);
                params.w = w.or(self.warm_white);
            }
            Some(ColourMode::Temp(temp)) => params.temp = Some(temp),
            Some(ColourMode::Scene(scene)) => params.scene_id = Some(scene),
            None => {
                params.c = self.cold_white;
                params.w = self.warm_white;
            }
        }

        Ok(params)
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
/// error.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct Pilot {
    /// Bulb MAC, lowercase hex with no separators.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    /// Received signal strength, dBm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rssi: Option<i32>,
    /// Whether the bulb is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<bool>,
    /// Red channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r: Option<u8>,
    /// Green channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub g: Option<u8>,
    /// Blue channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub b: Option<u8>,
    /// Cold white channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub c: Option<u8>,
    /// Warm white channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<u8>,
    /// Brightness percent. Present even when the bulb is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimming: Option<u8>,
    /// Colour temperature in Kelvin, when in white mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp: Option<u16>,
    /// Active scene. `0` means "no scene" while RGB is active.
    #[serde(default, rename = "sceneId", skip_serializing_if = "Option::is_none")]
    pub scene_id: Option<u16>,
    /// Scene animation speed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<u8>,
    /// Dual-head ratio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ratio: Option<u8>,
    /// Dual-head device selector, mostly seen on push traffic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devices: Option<u8>,
    /// Where the state came from (`udp`, `hb`, `wizc1`, …). Push only.
    #[serde(default, rename = "src", skip_serializing_if = "Option::is_none")]
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Success {
    /// Whether the bulb accepted the write.
    pub success: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::{Channel, Dimming, Kelvin, SceneId, Speed};

    #[test]
    fn rgb_temp_and_scene_replace_each_other() {
        let rgb = PilotBuilder::new()
            .temp(Kelvin::new(2700).unwrap())
            .rgb(
                Channel::new(1).unwrap(),
                Channel::new(2).unwrap(),
                Channel::new(3).unwrap(),
            )
            .params()
            .unwrap();
        assert!(rgb.temp.is_none());
        assert_eq!(rgb.r.unwrap().get(), 1);

        let temp = PilotBuilder::new()
            .rgb(
                Channel::new(1).unwrap(),
                Channel::new(2).unwrap(),
                Channel::new(3).unwrap(),
            )
            .temp(Kelvin::new(4000).unwrap())
            .params()
            .unwrap();
        assert!(temp.r.is_none());
        assert_eq!(temp.temp.unwrap().get(), 4000);

        let scene = PilotBuilder::new()
            .temp(Kelvin::new(4000).unwrap())
            .scene(SceneId::new(4).unwrap())
            .speed(Speed::new(100).unwrap())
            .params()
            .unwrap();
        assert!(scene.temp.is_none());
        assert_eq!(scene.scene_id.unwrap().get(), 4);
        assert_eq!(scene.speed.unwrap().get(), 100);
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
                r#"{"method":"setPilot","params":{"state":true}}"#,
            ),
            (
                PilotBuilder::new()
                    .dimming(Dimming::new(40).unwrap())
                    .set_pilot()
                    .unwrap(),
                r#"{"method":"setPilot","params":{"dimming":40}}"#,
            ),
            (
                PilotBuilder::new()
                    .rgb(
                        Channel::new(255).unwrap(),
                        Channel::new(80).unwrap(),
                        Channel::new(0).unwrap(),
                    )
                    .set_pilot()
                    .unwrap(),
                r#"{"method":"setPilot","params":{"r":255,"g":80,"b":0}}"#,
            ),
            (
                PilotBuilder::new()
                    .rgbww(
                        Channel::new(0).unwrap(),
                        Channel::new(128).unwrap(),
                        Channel::new(255).unwrap(),
                        Channel::new(0).unwrap(),
                        Channel::new(0).unwrap(),
                    )
                    .set_pilot()
                    .unwrap(),
                r#"{"method":"setPilot","params":{"r":0,"g":128,"b":255,"c":0,"w":0}}"#,
            ),
            (
                PilotBuilder::new()
                    .temp(Kelvin::new(5000).unwrap())
                    .set_pilot()
                    .unwrap(),
                r#"{"method":"setPilot","params":{"temp":5000}}"#,
            ),
            (
                PilotBuilder::new()
                    .scene(SceneId::new(4).unwrap())
                    .speed(Speed::new(100).unwrap())
                    .set_pilot()
                    .unwrap(),
                r#"{"method":"setPilot","params":{"sceneId":4,"speed":100}}"#,
            ),
            (
                PilotBuilder::new()
                    .rgb(
                        Channel::new(255).unwrap(),
                        Channel::new(0).unwrap(),
                        Channel::new(0).unwrap(),
                    )
                    .set_state()
                    .unwrap(),
                r#"{"method":"setState","params":{"r":255,"g":0,"b":0}}"#,
            ),
        ];

        for (request, expected) in cases {
            assert_eq!(request.to_string(), expected);
        }
    }

    #[test]
    fn pilot_parses_partial_results() {
        let colour: Pilot = serde_json::from_str(
            r#"{"mac":"9877d5230f0a","rssi":-53,"state":true,"sceneId":0,"r":255,"g":0,"b":0,"c":0,"w":0,"dimming":40}"#,
        )
        .unwrap();
        assert_eq!(colour.rgb(), Some((255, 0, 0)));
        assert_eq!(colour.rgbww(), Some((255, 0, 0, 0, 0)));
        assert!(colour.temp.is_none());

        let white: Pilot = serde_json::from_str(
            r#"{"mac":"9877d5230f0a","state":true,"sceneId":11,"temp":2700,"dimming":100}"#,
        )
        .unwrap();
        assert_eq!(white.temp, Some(2700));
        assert!(white.r.is_none());
        assert_eq!(white.scene_id, Some(11));
    }
}
