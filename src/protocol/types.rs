//! Validated pilot parameter newtypes.
//!
//! The bulb is not a reliable validator: an out-of-range `dimming` is silently
//! clamped and still reports success, while an out-of-range `temp` errors.
//! Ranges are therefore enforced here, before anything is serialised.
//!
//! These are **write-side** types. Nothing here implements `Deserialize`, and
//! that is deliberate: a bulb is free to *report* a value these constructors
//! would refuse — `dimming: 0` on an off bulb is the known case — and a
//! validating parse would turn that into a hard error. Results use the plain
//! integer, per the forward-compatibility rule in [`super`].

use serde::Serialize;

use crate::error::{Error, Result};

/// Defines a newtype whose range is checked on construction.
macro_rules! bounded_newtype {
    (
        $(#[$attr:meta])*
        $name:ident($repr:ty), $wire:literal, $min:literal ..= $max:literal
    ) => {
        $(#[$attr])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        pub struct $name($repr);

        impl $name {
            /// The inclusive lower bound.
            pub const MIN: $repr = $min;
            /// The inclusive upper bound.
            pub const MAX: $repr = $max;

            #[doc = concat!("Builds a `", $wire, "` value.")]
            ///
            /// # Errors
            ///
            /// Returns [`Error::InvalidParam`] if `value` is outside
            #[doc = concat!("`", stringify!($min), "..=", stringify!($max), "`.")]
            pub fn new(value: $repr) -> Result<Self> {
                if (Self::MIN..=Self::MAX).contains(&value) {
                    Ok(Self(value))
                } else {
                    Err(Error::InvalidParam {
                        message: format!(
                            concat!($wire, " must be {}..={}, got {}"),
                            Self::MIN,
                            Self::MAX,
                            value,
                        ),
                    })
                }
            }

            /// The raw value.
            #[must_use]
            pub const fn get(self) -> $repr {
                self.0
            }
        }

        impl From<$name> for $repr {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

/// Defines a newtype that accepts every value its representation can hold.
macro_rules! open_newtype {
    ($(#[$attr:meta])* $name:ident($repr:ty)) => {
        $(#[$attr])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        pub struct $name($repr);

        impl $name {
            /// Builds a value. Every
            #[doc = concat!("`", stringify!($repr), "`")]
            /// is accepted, so this cannot fail.
            #[must_use]
            pub const fn new(value: $repr) -> Self {
                Self(value)
            }

            /// The raw value.
            #[must_use]
            pub const fn get(self) -> $repr {
                self.0
            }
        }

        impl From<$repr> for $name {
            fn from(value: $repr) -> Self {
                Self(value)
            }
        }

        impl From<$name> for $repr {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

open_newtype! {
    /// An 8-bit channel value (`r` / `g` / `b` / `c` / `w`).
    ///
    /// The wire range is the whole of `u8`, so construction is infallible.
    Channel(u8)
}

open_newtype! {
    /// A WiZ scene identifier.
    ///
    /// Preset scenes are `1..=40`, custom slots `256..=265`, and `1000` is
    /// Rhythm. The id space is sparse and firmware-dependent, so unknown ids
    /// are left for the bulb — or a later scene table — to reject rather than
    /// being refused here.
    SceneId(u16)
}

impl SceneId {
    /// Rhythm, the special scene the official app uses for music modes.
    pub const RHYTHM: Self = Self::new(1000);
}

bounded_newtype! {
    /// Brightness as the bulb understands it: `dimming` in `1..=100`.
    ///
    /// `0` is out of range on the wire; an off bulb is expressed with
    /// `state: false`, not `dimming: 0`.
    Dimming(u8), "dimming", 1..=100
}

bounded_newtype! {
    /// Scene animation speed, in `10..=200`.
    ///
    /// Unverified: inherited from `pywizlight`, not measured on hardware.
    Speed(u8), "speed", 10..=200
}

bounded_newtype! {
    /// Colour temperature in Kelvin, in `1000..=10000`.
    ///
    /// A bulb's usable range is usually narrower and comes from
    /// [`ModelConfig`](super::ModelConfig) / [`UserConfig`](super::UserConfig);
    /// this is only the wire-format bound.
    Kelvin(u16), "temp", 1000..=10_000
}

bounded_newtype! {
    /// Dual-head balance (`ratio`), in `0..=100`.
    ///
    /// Unverified: inherited from `pywizlight`, not measured on hardware.
    Ratio(u8), "ratio", 0..=100
}

bounded_newtype! {
    /// Dual-head device selector (`devices`) for a **write**, in `1..=3`.
    ///
    /// Unverified. The bound is `pywizlight`'s (`1 <= value < 4`); what each
    /// value selects is not documented there and has not been measured here.
    /// `1` and `2` appear as per-head tags in `syncPilot` push traffic.
    ///
    /// Note that `getPilot` uses a *different*, zero-based convention for the
    /// same key — `pywizlight` polls heads with `{"devices": 0}` and
    /// `{"devices": 1}` — so this type deliberately does not cover reads.
    Devices(u8), "devices", 1..=3
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
    fn speed_kelvin_ratio_and_devices_enforce_their_ranges() {
        assert!(Speed::new(9).is_err());
        assert!(Speed::new(201).is_err());
        assert!(Kelvin::new(999).is_err());
        assert!(Kelvin::new(10_001).is_err());
        assert_eq!(Kelvin::new(2700).unwrap().get(), 2700);
        assert!(Ratio::new(101).is_err());
        assert_eq!(Ratio::new(0).unwrap().get(), 0);
        assert!(Devices::new(0).is_err());
        assert!(Devices::new(4).is_err());
        assert_eq!(Devices::new(3).unwrap().get(), 3);
    }

    #[test]
    fn open_newtypes_accept_their_whole_range() {
        assert_eq!(Channel::new(0).get(), 0);
        assert_eq!(Channel::new(255).get(), 255);
        assert_eq!(Channel::from(12u8).get(), 12);
        assert_eq!(SceneId::RHYTHM.get(), 1000);
        assert_eq!(u16::from(SceneId::new(40)), 40);
    }

    #[test]
    fn the_error_names_the_wire_field_and_the_bound() {
        let message = Dimming::new(0).unwrap_err().to_string();
        assert!(
            message.contains("dimming must be 1..=100, got 0"),
            "{message}"
        );
    }
}
