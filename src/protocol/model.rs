//! What a bulb *is*, derived from the name of the module inside it.
//!
//! Nothing on the wire says "this device has colour". `getSystemConfig`
//! answers with a `moduleName` like `ESP25_SHRGB_01`, and that name is the
//! only description of the hardware the protocol offers, so capabilities are
//! read out of it:
//!
//! ```text
//! ESP25_SHRGB_01
//! ^^^^^ ^^^^^ ^^
//!   |     |    `- hardware revision
//!   |     `------ head count (SH/DH) and class (RGB, TW, DW, SOCKET, …)
//!   `------------ module family
//! ```
//!
//! The one thing the name does not carry is the usable colour temperature
//! range, which comes from `getModelConfig` or `getUserConfig` — and never
//! from what the bulb is willing to accept. Measured on `ESP25_SHRGB_01` fw
//! 1.38.0: it reports a `cctRange` of 2200–6500, takes `temp: 12000` without
//! complaint, and then reports `6500`.
//!
//! What a name claims has been checked against that same hardware, which
//! derives `RGB`, single head: colour, colour temperature, scenes and dimming
//! all work, while `ratio` — the dual-head balance the map says it does not
//! have — is swallowed without appearing in `getPilot`, and addressing a
//! second head with `devices: 2` is refused outright.
//!
//! The grammar and the feature map are ported from `pywizlight`, which is the
//! only catalogue of these names that exists. Only the `ESP25_SHRGB_01` line
//! has been checked against hardware here; every other model is inherited and
//! unverified.

use std::fmt;
use std::str::FromStr;

use serde::{Serialize, Serializer};

use crate::error::{Error, Result};

/// The classes of device a `moduleName` can name.
///
/// The spelling in `--json` output and in [`Display`](fmt::Display) is the
/// token as it appears in a module name, so `ESP25_SHRGB_01` is `RGB`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum BulbClass {
    /// Full colour: RGB emitters plus white, tunable white included.
    #[serde(rename = "RGB")]
    Rgb,
    /// Tunable white: cool and warm white, no colour.
    #[serde(rename = "TW")]
    Tw,
    /// Dimmable white: brightness only. Most filament bulbs.
    #[serde(rename = "DW")]
    Dw,
    /// A smart socket: on and off, nothing to dim.
    #[serde(rename = "SOCKET")]
    Socket,
    /// A fan with a dimmable white light.
    #[serde(rename = "FANDIM")]
    FanDim,
    /// A fan with a tunable white light.
    #[serde(rename = "FANTW")]
    FanTw,
}

impl BulbClass {
    /// The class named by a module name's middle token, e.g. `SHRGB1C`.
    ///
    /// **The order of these tests is load-bearing**, and is why this is a
    /// chain rather than a table:
    ///
    /// - `RGB` first, because `SHRGB` would otherwise never be reached — and
    ///   a colour bulb misread as anything else loses its colour.
    /// - `DDTW` before `TW`, because `DDTW` *contains* `TW`: a tunable-white
    ///   fan read as a plain tunable-white bulb loses the fan.
    ///
    /// Anything that matches nothing is dimmable white, which is the class
    /// that needs no evidence: every WiZ device can at least be dimmed or
    /// switched.
    #[must_use]
    pub fn from_identifier(identifier: &str) -> Self {
        if identifier.contains("RGB") {
            Self::Rgb
        } else if identifier.contains("DDTW") {
            Self::FanTw
        } else if identifier.contains("TW") {
            Self::Tw
        } else if identifier.contains("SOCKET") {
            Self::Socket
        } else if identifier.contains("FANDIM") {
            Self::FanDim
        } else {
            Self::Dw
        }
    }

    /// The long name, as WiZ's own documentation spells it.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Rgb => "RGB Tunable",
            Self::Tw => "Tunable White",
            Self::Dw => "Dimmable White",
            Self::Socket => "Socket",
            Self::FanDim => "Fan Dimmable",
            Self::FanTw => "Fan Tunable White",
        }
    }

    /// Whether a device of this class is expected to report a Kelvin range.
    ///
    /// A class that can be told a colour temperature and cannot say which
    /// temperatures it honours is not describable, so
    /// [`BulbType::from_data`] treats the omission as an error rather than
    /// inventing a range. The others may legitimately report none.
    #[must_use]
    pub const fn needs_kelvin_range(self) -> bool {
        matches!(self, Self::Rgb | Self::Tw | Self::FanTw)
    }

    /// The token as it appears inside a module name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rgb => "RGB",
            Self::Tw => "TW",
            Self::Dw => "DW",
            Self::Socket => "SOCKET",
            Self::FanDim => "FANDIM",
            Self::FanTw => "FANTW",
        }
    }
}

impl fmt::Display for BulbClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How many light heads a module name declares.
///
/// Most modules say `SH` or `DH`; sockets, wall switches and fans say neither,
/// which is why this is only ever reported as an [`Option`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Heads {
    /// `SH` — one head. Also how LED strips are named.
    #[serde(rename = "SH")]
    Single,
    /// `DH` — two independently addressable heads, e.g. an up/down lamp.
    #[serde(rename = "DH")]
    Dual,
}

impl Heads {
    /// The head count declared by a module name's middle token, if it
    /// declares one.
    #[must_use]
    pub fn from_identifier(identifier: &str) -> Option<Self> {
        if identifier.contains("DH") {
            Some(Self::Dual)
        } else if identifier.contains("SH") {
            Some(Self::Single)
        } else {
            None
        }
    }

    /// How many heads that is.
    #[must_use]
    pub const fn count(self) -> u8 {
        match self {
            Self::Single => 1,
            Self::Dual => 2,
        }
    }
}

/// A parsed `moduleName`, such as `ESP25_SHRGB_01`.
///
/// The grammar is `<family>_<identifier>[_<revision>]`. Only the identifier
/// carries capabilities; the family is the module's Wi-Fi hardware and the
/// revision is a manufacturing detail, both kept because they are the only way
/// to tell two otherwise identical models apart in a bug report.
///
/// The original string is preserved exactly, so a name that round-trips
/// through this type is byte-for-byte what the bulb said.
///
/// ```
/// use wizlight::protocol::{BulbClass, Heads, ModuleName};
///
/// let module: ModuleName = "ESP25_SHRGB_01".parse()?;
/// assert_eq!(module.family(), "ESP25");
/// assert_eq!(module.identifier(), "SHRGB");
/// assert_eq!(module.revision(), Some("01"));
/// assert_eq!(module.class(), BulbClass::Rgb);
/// assert_eq!(module.heads(), Some(Heads::Single));
/// # Ok::<(), wizlight::Error>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ModuleName(String);

impl ModuleName {
    /// Parses a module name.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownModel`] if there is no identifier token — a
    /// bare `INVALID` with no underscore says nothing about the hardware, and
    /// guessing from it would be worse than admitting the model is unknown.
    pub fn parse(name: &str) -> Result<Self> {
        let identifier = name.split('_').nth(1).unwrap_or_default();
        if identifier.is_empty() {
            return Err(Error::UnknownModel {
                message: format!(
                    "`{name}` is not a module name: expected <family>_<identifier>[_<revision>]"
                ),
            });
        }
        Ok(Self(name.to_owned()))
    }

    /// The whole name, as the bulb reported it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The module family, e.g. `ESP25`. Wi-Fi hardware, not capabilities.
    #[must_use]
    pub fn family(&self) -> &str {
        self.0.split('_').next().unwrap_or_default()
    }

    /// The middle token, e.g. `SHRGB` — the part capabilities come from.
    #[must_use]
    pub fn identifier(&self) -> &str {
        self.0.split('_').nth(1).unwrap_or_default()
    }

    /// The hardware revision, e.g. `01` or `01ABI`, when the name carries one.
    ///
    /// Everything after the second separator, so a name with more than three
    /// tokens keeps the rest here rather than losing it.
    #[must_use]
    pub fn revision(&self) -> Option<&str> {
        let rest = self.0.splitn(3, '_').nth(2)?;
        (!rest.is_empty()).then_some(rest)
    }

    /// What class of device this is.
    #[must_use]
    pub fn class(&self) -> BulbClass {
        BulbClass::from_identifier(self.identifier())
    }

    /// The head count, when the name declares one.
    #[must_use]
    pub fn heads(&self) -> Option<Heads> {
        Heads::from_identifier(self.identifier())
    }
}

impl FromStr for ModuleName {
    type Err = Error;

    fn from_str(name: &str) -> Result<Self> {
        Self::parse(name)
    }
}

impl fmt::Display for ModuleName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Serialises as the plain string, not as a wrapper object: the name is what
/// consumers of `--json` recognise.
impl Serialize for ModuleName {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// The colour temperatures a device can usefully produce, in Kelvin.
///
/// This is the range the bulb *reports*, and it has nothing to do with the
/// range the wire accepts. Measured on `ESP25_SHRGB_01` fw 1.38.0, which
/// reports 2200–6500: `temp: 1000` and `temp: 12000` are both accepted with
/// `success`, and then read back as `2200` and `6500`. The bulb clamps into
/// this range in both directions and reports the clamped value, so a
/// temperature outside it produces light of the wrong colour however
/// cheerfully the write was acknowledged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct KelvinRange {
    min: u16,
    max: u16,
}

impl KelvinRange {
    /// Builds a range, ordering the two bounds.
    ///
    /// Firmware spells its ranges several ways — `[min, max]`,
    /// `[min, preferred_min, preferred_max, max]` — and nothing promises they
    /// arrive sorted, so this does not assume it either.
    #[must_use]
    pub const fn new(min: u16, max: u16) -> Self {
        if min <= max {
            Self { min, max }
        } else {
            Self { min: max, max: min }
        }
    }

    /// The coldest temperature the device honours.
    #[must_use]
    pub const fn min(self) -> u16 {
        self.min
    }

    /// The warmest temperature the device honours.
    #[must_use]
    pub const fn max(self) -> u16 {
        self.max
    }

    /// Whether a temperature falls inside the range, bounds included.
    #[must_use]
    pub const fn contains(self, kelvin: u16) -> bool {
        self.min <= kelvin && kelvin <= self.max
    }
}

impl fmt::Display for KelvinRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{} K", self.min, self.max)
    }
}

/// What a device can be asked to do.
///
/// Every flag is derived from the module name, not from anything the bulb was
/// observed to accept — the two are different questions. Measured on
/// `ESP25_SHRGB_01` fw 1.38.0: a single-head bulb takes the dual-head `ratio`
/// parameter and answers `success`, having nothing to balance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize)]
pub struct Features {
    /// RGB colour.
    pub color: bool,
    /// Colour temperature in Kelvin.
    pub color_tmp: bool,
    /// Scenes — effects the bulb animates by itself.
    pub effect: bool,
    /// Dimming.
    pub brightness: bool,
    /// Two independently addressable heads.
    pub dual_head: bool,
    /// A fan.
    pub fan: bool,
    /// The fan's breeze mode.
    pub fan_breeze_mode: bool,
    /// The fan running in reverse.
    pub fan_reverse: bool,
}

impl Features {
    /// The feature map: what a device of `class` can do.
    ///
    /// `dual_head` and `effect` are properties of the individual module rather
    /// than of its class, so they are supplied rather than looked up — a
    /// dimmable white bulb plays the basic effects while a `DIMTRIACS` wall
    /// switch of the same class does not.
    #[must_use]
    pub const fn for_class(class: BulbClass, dual_head: bool, effect: bool) -> Self {
        let (color, color_tmp, brightness, fan) = match class {
            BulbClass::Rgb => (true, true, true, false),
            BulbClass::Tw => (false, true, true, false),
            BulbClass::Dw => (false, false, true, false),
            // A socket is on or off; there is nothing to dim.
            BulbClass::Socket => (false, false, false, false),
            BulbClass::FanDim => (false, false, true, true),
            BulbClass::FanTw => (false, true, true, true),
        };
        Self {
            color,
            color_tmp,
            effect,
            brightness,
            dual_head,
            fan,
            // Inherited from `pywizlight`, where every fan has both: no fan
            // has been seen here to check whether that always holds.
            fan_breeze_mode: fan,
            fan_reverse: fan,
        }
    }
}

/// Where a [`BulbType`]'s class came from.
///
/// Worth keeping, because the three are not equally trustworthy: a module name
/// is a description, while an unknown `typeId` is a guess that happens to be
/// right for most devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Derivation {
    /// Parsed from the reported `moduleName`.
    ModuleName,
    /// Firmware too old to report a `moduleName` — 1.8.0 does not — so the
    /// class came from a `typeId` this crate recognises.
    KnownTypeId(u32),
    /// A `typeId` this crate does not recognise: dimmable white is **assumed**,
    /// on the grounds that every WiZ device can at least be dimmed. Treat the
    /// resulting features as a floor rather than a description.
    AssumedTypeId(u32),
}

/// The `typeId` values this crate can name.
///
/// `pywizlight` knows exactly one, and nothing here has been able to add to it:
/// the hardware on hand reports a module name and no `typeId` at all.
const fn class_for_type_id(type_id: u32) -> Option<BulbClass> {
    match type_id {
        0 => Some(BulbClass::Dw),
        _ => None,
    }
}

/// What a bulb reported about itself, gathered from the config methods.
///
/// The input to [`BulbType::from_data`], for callers deriving a type from
/// stored or captured config rather than from a live bulb — otherwise use
/// [`Bulb::bulb_type`](crate::Bulb::bulb_type), which fills this in.
///
/// ```
/// use wizlight::protocol::{BulbClass, BulbData, BulbType, KelvinRange};
///
/// let bulb_type = BulbType::from_data(&BulbData {
///     module_name: Some("ESP25_SHRGB_01"),
///     kelvin_range: Some(KelvinRange::new(2200, 6500)),
///     ..BulbData::default()
/// })?;
/// assert_eq!(bulb_type.class, BulbClass::Rgb);
/// assert!(bulb_type.features.color);
/// # Ok::<(), wizlight::Error>(())
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BulbData<'a> {
    /// `moduleName` from `getSystemConfig`, absent on firmware before 1.9.
    pub module_name: Option<&'a str>,
    /// `typeId` from `getSystemConfig`, the fallback when there is no module
    /// name.
    pub type_id: Option<u32>,
    /// `fwVersion` from `getSystemConfig`.
    pub fw_version: Option<&'a str>,
    /// The Kelvin range from `getModelConfig` or `getUserConfig`.
    pub kelvin_range: Option<KelvinRange>,
    /// `nowc`, or the second entry of the older `drvConf`.
    pub white_channels: Option<u32>,
    /// `wcr`, or the first entry of the older `drvConf`.
    pub white_to_color_ratio: Option<u32>,
    /// `fanSpeed`, on the models that have a fan.
    pub fan_speed_range: Option<u32>,
}

/// Everything known about a device: its class, what it can do, and the
/// hardware details behind that.
///
/// Build one with [`Bulb::bulb_type`](crate::Bulb::bulb_type) or, from stored
/// config, with [`from_data`](BulbType::from_data).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct BulbType {
    /// What kind of device this is.
    pub class: BulbClass,
    /// What it can be asked to do.
    pub features: Features,
    /// The parsed module name, absent on firmware that does not report one.
    pub module_name: Option<ModuleName>,
    /// The colour temperatures it honours.
    pub kelvin_range: Option<KelvinRange>,
    /// Firmware version, as reported.
    pub fw_version: Option<String>,
    /// How many white emitters it has: 1 for warm-only, 2 for warm and cold.
    pub white_channels: Option<u32>,
    /// How much white is mixed in with colour, as a percentage.
    pub white_to_color_ratio: Option<u32>,
    /// The number of fan speeds, on a device with a fan.
    pub fan_speed_range: Option<u32>,
    /// How the class was arrived at, and so how much to trust it.
    pub derivation: Derivation,
}

impl BulbType {
    /// Derives what a device can do from what it reported about itself.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownModel`] when the device cannot be described:
    ///
    /// - it reported neither a `moduleName` nor a `typeId`;
    /// - its module name has no identifier token, e.g. a bare `INVALID`;
    /// - its class must report a Kelvin range — colour and tunable white both
    ///   must — and no config method gave one.
    ///
    /// The last is the surprising one, and it is deliberate: a tunable white
    /// bulb with an invented range would take temperature commands and quietly
    /// light up the wrong colour.
    pub fn from_data(data: &BulbData<'_>) -> Result<Self> {
        // An empty `moduleName` is treated as no module name. Firmware that
        // does not know its own module reports the key as missing, but an
        // empty string means exactly as much.
        let name = data.module_name.filter(|name| !name.is_empty());
        let (module_name, class, derivation) = match (name, data.type_id) {
            (Some(name), _) => {
                let module = ModuleName::parse(name)?;
                let class = module.class();
                (Some(module), class, Derivation::ModuleName)
            }
            (None, Some(type_id)) => match class_for_type_id(type_id) {
                Some(class) => (None, class, Derivation::KnownTypeId(type_id)),
                // Every WiZ device can at least be dimmed, so dimmable white
                // is a floor rather than a description. `derivation` is what
                // says which of the two this is.
                None => (None, BulbClass::Dw, Derivation::AssumedTypeId(type_id)),
            },
            (None, None) => {
                return Err(Error::UnknownModel {
                    message: "the device reported neither a moduleName nor a typeId".to_owned(),
                });
            }
        };
        let heads = module_name.as_ref().and_then(ModuleName::heads);

        // Effects follow the class, except for dimmable white, where the name
        // decides: a module that declares a head plays the basic effects,
        // while a DIMTRIACS wall switch does not. A device identified by
        // `typeId` alone is assumed to play them.
        let effect = match class {
            BulbClass::Rgb | BulbClass::Tw | BulbClass::FanTw => true,
            BulbClass::Socket | BulbClass::FanDim => false,
            BulbClass::Dw => module_name.is_none() || heads.is_some(),
        };

        if data.kelvin_range.is_none() && class.needs_kelvin_range() {
            return Err(Error::UnknownModel {
                message: format!(
                    "a {} device must report a Kelvin range, and this one did not",
                    class.description()
                ),
            });
        }

        Ok(Self {
            class,
            features: Features::for_class(class, heads == Some(Heads::Dual), effect),
            module_name,
            kelvin_range: data.kelvin_range,
            fw_version: data.fw_version.map(ToOwned::to_owned),
            white_channels: data.white_channels,
            white_to_color_ratio: data.white_to_color_ratio,
            fan_speed_range: data.fan_speed_range,
            derivation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `moduleName` in `pywizlight`'s fixtures, with the type it derives
    /// there, plus the one model we own.
    ///
    /// `ESP25_SHRGB_01` is measured; the rest are inherited, and are here to
    /// pin the port rather than to describe hardware anyone has seen.
    /// `ESP03_DDTW_01` is synthetic — no fixture, no capture, and no device
    /// covers the `DDTW` branch, so this is what the ported rule *would* do.
    const CORPUS: &[(&str, BulbClass, Option<Heads>, bool)] = {
        use BulbClass::{Dw, FanDim, FanTw, Rgb, Socket, Tw};
        use Heads::{Dual, Single};

        &[
            ("ESP25_SHRGB_01", Rgb, Some(Single), true),
            ("ESP01_SHRGB_03", Rgb, Some(Single), true),
            ("ESP20_DHRGB_01", Rgb, Some(Dual), true),
            ("ESP20_DHRGB_01B", Rgb, Some(Dual), true),
            ("ESP20_SHRGB_01ABI", Rgb, Some(Single), true),
            ("ESP03_SHRGB3_01ABI", Rgb, Some(Single), true),
            ("ESP20_SHRGBC_01", Rgb, Some(Single), true),
            ("ESP20_SHRGB_01BT", Rgb, Some(Single), true),
            ("ESP01_SHRGB1C_31", Rgb, Some(Single), true),
            ("ESP21_SHTW_01", Tw, Some(Single), true),
            ("ESP05_SHTW_21", Tw, Some(Single), true),
            ("ESP14_SHTW1C_01", Tw, Some(Single), true),
            ("ESP05_SHDW_21", Dw, Some(Single), true),
            ("ESP06_SHDW9_01", Dw, Some(Single), true),
            ("ESP01_SHDW1C_31", Dw, Some(Single), true),
            ("ESP01_DIMTRIACS_01", Dw, None, false),
            ("ESP10_SOCKET_06", Socket, None, false),
            ("ESP25_SOCKET_01", Socket, None, false),
            ("ESP03_FANDIMS_31", FanDim, None, false),
            ("ESP03_DDTW_01", FanTw, None, true),
        ]
    };

    fn from_name(name: &str) -> BulbType {
        BulbType::from_data(&BulbData {
            module_name: Some(name),
            // Wide enough to satisfy the classes that must report one, and
            // ignored by those that need not.
            kelvin_range: Some(KelvinRange::new(2200, 6500)),
            ..BulbData::default()
        })
        .expect("the corpus parses")
    }

    #[test]
    fn every_known_module_name_derives_the_same_type_pywizlight_gives_it() {
        for &(name, class, heads, effect) in CORPUS {
            let bulb_type = from_name(name);
            let module = bulb_type.module_name.as_ref().expect("a module name");
            assert_eq!(module.class(), class, "{name}");
            assert_eq!(module.heads(), heads, "{name}");
            assert_eq!(bulb_type.class, class, "{name}");
            assert_eq!(bulb_type.features.effect, effect, "{name}");
            assert_eq!(
                bulb_type.features.dual_head,
                heads == Some(Heads::Dual),
                "{name}"
            );
            assert_eq!(bulb_type.derivation, Derivation::ModuleName, "{name}");
            assert_eq!(module.as_str(), name, "{name}");
        }
    }

    #[test]
    fn the_feature_map_matches_the_class() {
        let rgb = from_name("ESP25_SHRGB_01").features;
        assert_eq!(
            rgb,
            Features {
                color: true,
                color_tmp: true,
                effect: true,
                brightness: true,
                dual_head: false,
                fan: false,
                fan_breeze_mode: false,
                fan_reverse: false,
            }
        );

        let tunable = from_name("ESP21_SHTW_01").features;
        assert!(!tunable.color && tunable.color_tmp && tunable.brightness);

        let dimmable = from_name("ESP05_SHDW_21").features;
        assert!(!dimmable.color && !dimmable.color_tmp && dimmable.brightness);

        // A socket has nothing to dim, which is the one class that turns
        // brightness off.
        let socket = from_name("ESP10_SOCKET_06").features;
        assert!(!socket.brightness && !socket.effect);

        let fan = from_name("ESP03_FANDIMS_31").features;
        assert!(fan.fan && fan.fan_breeze_mode && fan.fan_reverse);
        assert!(fan.brightness && !fan.color_tmp && !fan.effect);

        let fan_tw = from_name("ESP03_DDTW_01").features;
        assert!(fan_tw.fan && fan_tw.color_tmp && fan_tw.effect && !fan_tw.color);
    }

    #[test]
    fn match_order_is_load_bearing() {
        // Both tokens contain another class's token. Reordering the chain in
        // `from_identifier` breaks exactly these two.
        assert_eq!(BulbClass::from_identifier("SHRGB"), BulbClass::Rgb);
        assert_eq!(BulbClass::from_identifier("DDTW"), BulbClass::FanTw);
        // …and the tokens they must not be mistaken for still work alone.
        assert_eq!(BulbClass::from_identifier("SHTW1C"), BulbClass::Tw);
        assert_eq!(BulbClass::from_identifier("FANDIMS"), BulbClass::FanDim);
        assert_eq!(BulbClass::from_identifier("SOCKET"), BulbClass::Socket);
        assert_eq!(BulbClass::from_identifier("DIMTRIACS"), BulbClass::Dw);
    }

    #[test]
    fn a_module_name_keeps_its_three_parts() {
        let module = ModuleName::parse("ESP01_SHRGB1C_31").unwrap();
        assert_eq!(module.family(), "ESP01");
        assert_eq!(module.identifier(), "SHRGB1C");
        assert_eq!(module.revision(), Some("31"));

        // A revision may itself contain separators; keep the lot rather than
        // silently truncating a name we would then fail to recognise again.
        let long = ModuleName::parse("ESP20_SHRGB_01_ABI").unwrap();
        assert_eq!(long.revision(), Some("01_ABI"));
        assert_eq!(long.to_string(), "ESP20_SHRGB_01_ABI");

        // No revision at all is legal; the identifier is the only part that
        // has to be there.
        let short = ModuleName::parse("ESP20_SHRGB").unwrap();
        assert_eq!(short.revision(), None);
        assert_eq!(short.class(), BulbClass::Rgb);
        assert_eq!(ModuleName::parse("ESP20_SHRGB_").unwrap().revision(), None);
    }

    #[test]
    fn a_name_with_no_identifier_is_refused() {
        // The `INVALID` case: no separator, so nothing to read.
        for name in ["INVALID", "", "_", "ESP20_"] {
            let err = ModuleName::parse(name).expect_err("no identifier");
            assert!(matches!(err, Error::UnknownModel { .. }), "{name}: {err}");
        }
        let err = BulbType::from_data(&BulbData {
            module_name: Some("INVALID"),
            ..BulbData::default()
        })
        .expect_err("no identifier");
        assert!(err.to_string().contains("INVALID"), "{err}");
    }

    #[test]
    fn a_class_that_must_report_a_kelvin_range_and_does_not_is_an_error() {
        for name in ["ESP21_SHTW_01", "ESP25_SHRGB_01", "ESP03_DDTW_01"] {
            let err = BulbType::from_data(&BulbData {
                module_name: Some(name),
                ..BulbData::default()
            })
            .expect_err("no kelvin range");
            assert!(
                err.to_string().contains("must report a Kelvin range"),
                "{name}: {err}"
            );
        }

        // The classes that need no range are unbothered by its absence.
        for name in ["ESP05_SHDW_21", "ESP10_SOCKET_06", "ESP03_FANDIMS_31"] {
            let bulb_type = BulbType::from_data(&BulbData {
                module_name: Some(name),
                ..BulbData::default()
            })
            .expect("no range needed");
            assert_eq!(bulb_type.kelvin_range, None, "{name}");
        }
    }

    #[test]
    fn type_id_stands_in_for_a_missing_module_name() {
        // Firmware 1.8.0 reports no moduleName at all.
        let known = BulbType::from_data(&BulbData {
            type_id: Some(0),
            fw_version: Some("1.8.0"),
            white_channels: Some(1),
            white_to_color_ratio: Some(20),
            ..BulbData::default()
        })
        .expect("typeId 0 is known");
        assert_eq!(known.class, BulbClass::Dw);
        assert_eq!(known.derivation, Derivation::KnownTypeId(0));
        assert_eq!(known.module_name, None);
        assert_eq!(known.kelvin_range, None);
        // Unlike a DIMTRIACS wall switch, a device known only by typeId is
        // assumed to play effects.
        assert!(known.features.effect && known.features.brightness);
        assert!(!known.features.color && !known.features.color_tmp);

        // An unrecognised typeId still produces a usable type, and says that
        // it is a guess.
        let assumed = BulbType::from_data(&BulbData {
            type_id: Some(1),
            ..BulbData::default()
        })
        .expect("an unknown typeId still yields dimmable white");
        assert_eq!(assumed.class, BulbClass::Dw);
        assert_eq!(assumed.derivation, Derivation::AssumedTypeId(1));

        // An empty module name is no module name.
        let empty = BulbType::from_data(&BulbData {
            module_name: Some(""),
            type_id: Some(0),
            ..BulbData::default()
        })
        .expect("falls through to the typeId");
        assert_eq!(empty.derivation, Derivation::KnownTypeId(0));
    }

    #[test]
    fn a_device_with_neither_a_module_name_nor_a_type_id_is_unknowable() {
        let err = BulbType::from_data(&BulbData::default()).expect_err("nothing to go on");
        assert!(
            err.to_string()
                .contains("neither a moduleName nor a typeId"),
            "{err}"
        );
    }

    #[test]
    fn the_measured_bulb_derives_what_the_hardware_does() {
        // ESP25_SHRGB_01 on fw 1.38.0, read from 192.168.0.5: no `drvConf`
        // and no `typeId` in getSystemConfig, so the white channel count and
        // ratio come from getModelConfig's `nowc` and `wcr`, and the range
        // from its `cctRange` of [2200, 2700, 6500, 6500].
        let bulb_type = BulbType::from_data(&BulbData {
            module_name: Some("ESP25_SHRGB_01"),
            fw_version: Some("1.38.0"),
            kelvin_range: Some(KelvinRange::new(2200, 6500)),
            white_channels: Some(1),
            white_to_color_ratio: Some(80),
            ..BulbData::default()
        })
        .expect("the bulb on the desk");

        assert_eq!(bulb_type.class, BulbClass::Rgb);
        assert_eq!(
            bulb_type.kelvin_range,
            Some(KelvinRange::new(2200, 6500)),
            "the reported range, not the 1000-12000 the wire accepts"
        );
        assert_eq!(bulb_type.white_channels, Some(1));
        assert_eq!(bulb_type.white_to_color_ratio, Some(80));
        assert_eq!(bulb_type.fan_speed_range, None);
        // Corroborated by the same getModelConfig: headTotal 1, devTotal 1.
        assert!(!bulb_type.features.dual_head);
    }

    #[test]
    fn kelvin_range_orders_its_bounds_and_answers_what_it_covers() {
        let range = KelvinRange::new(6500, 2200);
        assert_eq!((range.min(), range.max()), (2200, 6500));
        assert!(range.contains(2200) && range.contains(6500) && range.contains(4000));
        assert!(!range.contains(2199) && !range.contains(6501));
        assert_eq!(range.to_string(), "2200-6500 K");
    }

    #[test]
    fn json_uses_the_names_the_protocol_uses() {
        let bulb_type = from_name("ESP20_DHRGB_01");
        let json = serde_json::to_value(&bulb_type).unwrap();
        assert_eq!(json["class"], "RGB");
        assert_eq!(json["module_name"], "ESP20_DHRGB_01");
        assert_eq!(json["derivation"], "module_name");
        assert_eq!(json["features"]["dual_head"], true);
        assert_eq!(
            json["kelvin_range"],
            serde_json::json!({"min":2200,"max":6500})
        );

        let assumed = BulbType::from_data(&BulbData {
            type_id: Some(7),
            ..BulbData::default()
        })
        .unwrap();
        let json = serde_json::to_value(&assumed).unwrap();
        assert_eq!(
            json["derivation"],
            serde_json::json!({"assumed_type_id": 7})
        );
        assert_eq!(json["module_name"], serde_json::Value::Null);
    }
}
