//! Typed results for the config and maintenance methods.

use serde::{Deserialize, Serialize};

/// `getSystemConfig` result.
///
/// Unknown fields are ignored. Older firmware omits several of the keys the
/// 1.38.0 capture carries, so everything interesting is optional.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct SystemConfig {
    /// Bulb MAC.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
    /// Home the bulb is paired to.
    #[serde(rename = "homeId", skip_serializing_if = "Option::is_none")]
    pub home_id: Option<u64>,
    /// Room inside that home.
    #[serde(rename = "roomId", skip_serializing_if = "Option::is_none")]
    pub room_id: Option<u64>,
    /// Region code, e.g. `eu`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rgn: Option<String>,
    /// Module name, e.g. `ESP25_SHRGB_01`. Capability parsing is a later concern.
    #[serde(rename = "moduleName", skip_serializing_if = "Option::is_none")]
    pub module_name: Option<String>,
    /// Firmware version string.
    #[serde(rename = "fwVersion", skip_serializing_if = "Option::is_none")]
    pub fw_version: Option<String>,
    /// Group id.
    #[serde(rename = "groupId", skip_serializing_if = "Option::is_none")]
    pub group_id: Option<u64>,
    /// Pre-1.22 driver config: `[white_to_color_ratio, white_channels]`.
    #[serde(rename = "drvConf", skip_serializing_if = "Option::is_none")]
    pub drv_conf: Option<Vec<u32>>,
    /// Type id on some older firmware.
    #[serde(rename = "typeId", skip_serializing_if = "Option::is_none")]
    pub type_id: Option<u32>,
}

/// `getModelConfig` result (firmware &gt; 1.22).
///
/// The 1.38.0 firmware returns many more fields than any `pywizlight` fixture;
/// only the ones callers need today are typed. The rest stay ignored.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct ModelConfig {
    /// White-to-colour ratio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wcr: Option<u32>,
    /// Number of white channels.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nowc: Option<u32>,
    /// Colour-temperature range: `[min, start, end, max]` on current firmware.
    #[serde(rename = "cctRange", skip_serializing_if = "Option::is_none")]
    pub cct_range: Option<Vec<u16>>,
    /// PWM duty range.
    #[serde(rename = "pwmRange", skip_serializing_if = "Option::is_none")]
    pub pwm_range: Option<Vec<u32>>,
    /// Fan speed range, when the device is a fan.
    #[serde(rename = "fanSpeed", skip_serializing_if = "Option::is_none")]
    pub fan_speed: Option<u32>,
    /// How many logical devices the module exposes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub devices: Option<u32>,
}

impl ModelConfig {
    /// The usable Kelvin range, when `cctRange` is present.
    ///
    /// Uses the outer bounds of the four-element form
    /// `[min, preferred_min, preferred_max, max]`.
    #[must_use]
    pub fn kelvin_range(&self) -> Option<(u16, u16)> {
        range_bounds(self.cct_range.as_deref())
    }
}

/// `getUserConfig` result.
///
/// On firmware before `getModelConfig` existed, the white range lives here
/// (`extRange` preferred, then `whiteRange`).
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct UserConfig {
    /// Extended white range.
    #[serde(rename = "extRange", skip_serializing_if = "Option::is_none")]
    pub ext_range: Option<Vec<u16>>,
    /// White range on older firmware.
    #[serde(rename = "whiteRange", skip_serializing_if = "Option::is_none")]
    pub white_range: Option<Vec<u16>>,
    /// Default dimming.
    #[serde(rename = "dftDim", skip_serializing_if = "Option::is_none")]
    pub dft_dim: Option<u8>,
    /// Fade-in ms.
    #[serde(rename = "fadeIn", skip_serializing_if = "Option::is_none")]
    pub fade_in: Option<u32>,
    /// Fade-out ms.
    #[serde(rename = "fadeOut", skip_serializing_if = "Option::is_none")]
    pub fade_out: Option<u32>,
    /// Fan speed range on some models.
    #[serde(rename = "fanSpeed", skip_serializing_if = "Option::is_none")]
    pub fan_speed: Option<u32>,
}

impl UserConfig {
    /// The white/Kelvin range from `extRange`, falling back to `whiteRange`.
    #[must_use]
    pub fn kelvin_range(&self) -> Option<(u16, u16)> {
        range_bounds(self.ext_range.as_deref())
            .or_else(|| range_bounds(self.white_range.as_deref()))
    }
}

/// `getPower` result.
///
/// The unit is whatever the firmware reports. On the measured `ESP25_SHRGB_01`
/// the method exists and always returns `0`; on a socket fixture it is a large
/// integer. Callers that want watts should treat the value as opaque until a
/// given model is characterised.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Power {
    /// Reported power reading.
    pub power: u64,
}

/// The outer bounds of a reported range.
///
/// Firmware spells these several ways — `[min, max]` on `whiteRange`,
/// `[min, preferred_min, preferred_max, max]` on a modern `cctRange` — so the
/// smallest and largest entries are taken rather than the first and last.
/// That is what `pywizlight` does (`min(kelvin_list)` / `max(kelvin_list)`),
/// and it does not assume the bulb sorted the list.
fn range_bounds(range: Option<&[u16]>) -> Option<(u16, u16)> {
    let range = range?;
    Some((*range.iter().min()?, *range.iter().max()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_config_reads_cct_range_and_ignores_unknown_fields() {
        let config: ModelConfig = serde_json::from_str(
            r#"{
                "devTotal":1,
                "hasGradient":1,
                "wcr":80,
                "nowc":1,
                "cctRange":[2200,2700,6500,6500],
                "i2cDrv":[{"chip":"BP5768D"}]
            }"#,
        )
        .unwrap();
        assert_eq!(config.wcr, Some(80));
        assert_eq!(config.kelvin_range(), Some((2200, 6500)));
    }

    #[test]
    fn user_config_prefers_ext_range() {
        let config: UserConfig = serde_json::from_str(
            r#"{"whiteRange":[2700,6500],"extRange":[2200,6500],"dftDim":100}"#,
        )
        .unwrap();
        assert_eq!(config.kelvin_range(), Some((2200, 6500)));
    }

    #[test]
    fn missing_kelvin_range_is_none_not_an_error() {
        let config: ModelConfig = serde_json::from_str(r#"{"wcr":20}"#).unwrap();
        assert_eq!(config.kelvin_range(), None);
    }

    #[test]
    fn range_bounds_does_not_assume_the_list_is_sorted_or_four_long() {
        assert_eq!(range_bounds(Some(&[2700, 6500])), Some((2700, 6500)));
        assert_eq!(range_bounds(Some(&[6500, 2200, 2700])), Some((2200, 6500)));
        assert_eq!(range_bounds(Some(&[2700])), Some((2700, 2700)));
        assert_eq!(range_bounds(Some(&[])), None);
        assert_eq!(range_bounds(None), None);
    }

    #[test]
    fn a_single_temperature_model_reports_a_degenerate_range() {
        // The socket fixture: `cctRange` is four copies of one value.
        let config: ModelConfig =
            serde_json::from_str(r#"{"cctRange":[2700,2700,2700,2700]}"#).unwrap();
        assert_eq!(config.kelvin_range(), Some((2700, 2700)));
    }
}
