//! Validated pilot parameter newtypes.
//!
//! The bulb is not a reliable validator: an out-of-range `dimming` is silently
//! clamped and still reports success, while an out-of-range `temp` errors. Ranges
//! are therefore enforced here, before anything is serialised.

use crate::error::{Error, Result};

/// An 8-bit channel value (`r` / `g` / `b` / `c` / `w`), in `0..=255`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Channel(u8);

impl Channel {
    /// The inclusive range the wire format accepts.
    pub const MIN: u8 = 0;
    /// The inclusive upper bound.
    pub const MAX: u8 = 255;

    /// Builds a channel value.
    ///
    /// # Errors
    ///
    /// Never fails for a `u8`; the constructor exists so every pilot field goes
    /// through the same shape of API.
    pub fn new(value: u8) -> Result<Self> {
        Ok(Self(value))
    }

    /// The raw value.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl From<Channel> for u8 {
    fn from(value: Channel) -> Self {
        value.0
    }
}

impl serde::Serialize for Channel {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> serde::Deserialize<'de> for Channel {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = u8::deserialize(deserializer)?;
        Ok(Self(value))
    }
}

/// Brightness as the bulb understands it: `dimming` in `1..=100`.
///
/// `0` is out of range on the wire; an off bulb is expressed with `state: false`,
/// not `dimming: 0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Dimming(u8);

impl Dimming {
    /// The inclusive lower bound.
    pub const MIN: u8 = 1;
    /// The inclusive upper bound.
    pub const MAX: u8 = 100;

    /// Builds a dimming value.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidParam`] if `value` is outside `1..=100`.
    pub fn new(value: u8) -> Result<Self> {
        if (Self::MIN..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(Error::InvalidParam {
                message: format!("dimming must be {}..={}, got {value}", Self::MIN, Self::MAX),
            })
        }
    }

    /// The raw percent.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl From<Dimming> for u8 {
    fn from(value: Dimming) -> Self {
        value.0
    }
}

impl serde::Serialize for Dimming {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> serde::Deserialize<'de> for Dimming {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = u8::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Scene animation speed, in `10..=200`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Speed(u8);

impl Speed {
    /// The inclusive lower bound.
    pub const MIN: u8 = 10;
    /// The inclusive upper bound.
    pub const MAX: u8 = 200;

    /// Builds a speed value.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidParam`] if `value` is outside `10..=200`.
    pub fn new(value: u8) -> Result<Self> {
        if (Self::MIN..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(Error::InvalidParam {
                message: format!("speed must be {}..={}, got {value}", Self::MIN, Self::MAX),
            })
        }
    }

    /// The raw value.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl From<Speed> for u8 {
    fn from(value: Speed) -> Self {
        value.0
    }
}

impl serde::Serialize for Speed {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> serde::Deserialize<'de> for Speed {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = u8::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Colour temperature in Kelvin, in `1000..=10000`.
///
/// A bulb's usable range is usually narrower and comes from
/// [`ModelConfig`](super::ModelConfig) / [`UserConfig`](super::UserConfig); this
/// is only the wire-format bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Kelvin(u16);

impl Kelvin {
    /// The inclusive lower bound the protocol accepts.
    pub const MIN: u16 = 1000;
    /// The inclusive upper bound the protocol accepts.
    pub const MAX: u16 = 10_000;

    /// Builds a Kelvin value.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidParam`] if `value` is outside `1000..=10000`.
    pub fn new(value: u16) -> Result<Self> {
        if (Self::MIN..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(Error::InvalidParam {
                message: format!("temp must be {}..={}, got {value}", Self::MIN, Self::MAX),
            })
        }
    }

    /// The raw Kelvin value.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl From<Kelvin> for u16 {
    fn from(value: Kelvin) -> Self {
        value.0
    }
}

impl serde::Serialize for Kelvin {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_u16(self.0)
    }
}

impl<'de> serde::Deserialize<'de> for Kelvin {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = u16::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Dual-head balance (`ratio`), in `0..=100`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ratio(u8);

impl Ratio {
    /// The inclusive lower bound.
    pub const MIN: u8 = 0;
    /// The inclusive upper bound.
    pub const MAX: u8 = 100;

    /// Builds a ratio value.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidParam`] if `value` is outside `0..=100`.
    pub fn new(value: u8) -> Result<Self> {
        if (Self::MIN..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(Error::InvalidParam {
                message: format!("ratio must be {}..={}, got {value}", Self::MIN, Self::MAX),
            })
        }
    }

    /// The raw percent.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl From<Ratio> for u8 {
    fn from(value: Ratio) -> Self {
        value.0
    }
}

impl serde::Serialize for Ratio {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> serde::Deserialize<'de> for Ratio {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = u8::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// A WiZ scene identifier.
///
/// Preset scenes are `1..=40`, custom slots `256..=265`, and `1000` is Rhythm.
/// Per-class availability is a separate concern; this type only carries the id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SceneId(u16);

impl SceneId {
    /// Rhythm, the special scene the official app uses for music modes.
    pub const RHYTHM: Self = Self(1000);

    /// Builds a scene id.
    ///
    /// # Errors
    ///
    /// Never fails today — the id space is sparse and firmware-dependent, so
    /// unknown ids are left for the bulb (or a later scene table) to reject.
    pub fn new(value: u16) -> Result<Self> {
        Ok(Self(value))
    }

    /// The raw id.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl From<SceneId> for u16 {
    fn from(value: SceneId) -> Self {
        value.0
    }
}

impl serde::Serialize for SceneId {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_u16(self.0)
    }
}

impl<'de> serde::Deserialize<'de> for SceneId {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = u16::deserialize(deserializer)?;
        Ok(Self(value))
    }
}

/// Dual-head device selector (`devices`).
///
/// `1` and `2` address each head; `3` addresses both. Measured only as a field
/// that appears in push traffic so far — the range follows `pywizlight`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Devices(u8);

impl Devices {
    /// The inclusive lower bound.
    pub const MIN: u8 = 1;
    /// The inclusive upper bound (`1`, `2`, or both).
    pub const MAX: u8 = 3;

    /// Builds a device selector.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidParam`] if `value` is outside `1..=3`.
    pub fn new(value: u8) -> Result<Self> {
        if (Self::MIN..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(Error::InvalidParam {
                message: format!("devices must be {}..={}, got {value}", Self::MIN, Self::MAX),
            })
        }
    }

    /// The raw selector.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl From<Devices> for u8 {
    fn from(value: Devices) -> Self {
        value.0
    }
}

impl serde::Serialize for Devices {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> serde::Deserialize<'de> for Devices {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = u8::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimming_rejects_zero_and_over_100() {
        assert!(Dimming::new(0).is_err());
        assert!(Dimming::new(101).is_err());
        assert_eq!(Dimming::new(1).unwrap().get(), 1);
        assert_eq!(Dimming::new(100).unwrap().get(), 100);
    }

    #[test]
    fn speed_and_kelvin_enforce_their_ranges() {
        assert!(Speed::new(9).is_err());
        assert!(Speed::new(201).is_err());
        assert!(Kelvin::new(999).is_err());
        assert!(Kelvin::new(10_001).is_err());
        assert_eq!(Kelvin::new(2700).unwrap().get(), 2700);
    }
}
