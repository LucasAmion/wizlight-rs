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
    /// Measured on `ESP25_SHRGB_01` fw 1.38.0: a write is accepted for
    /// `1..=248` and refused with `-32602` outside it. That is a range check
    /// and not a scene table — the bulb takes ids far beyond the scenes it can
    /// actually play — so which ids do something useful is a per-model
    /// question, and one this type does not try to answer.
    ///
    /// `0` is refused on a write but *is* reported on a read, where it means
    /// "no scene, colour is active"; see [`Pilot::scene_id`](super::Pilot).
    ///
    /// Construction stays infallible despite the measured bound. Scene
    /// availability varies by firmware and model, and pinning the type to one
    /// bulb's range would refuse ids that another may accept. The bulb is the
    /// authority until there is a scene table to consult.
    SceneId(u16)
}

bounded_newtype! {
    /// Brightness, in `1..=100`.
    ///
    /// This is a **client-side policy, not a wire bound**. Measured on
    /// `ESP25_SHRGB_01` fw 1.38.0: the bulb accepts every `u8` here — `0` and
    /// `255` both answer `success` — and then silently clamps into `1..=100`,
    /// which is also the only range it ever reports back. So the bound exists
    /// to stop a caller believing something happened when it did not, and
    /// `Dimming::new(0)` failing is the point rather than an inconvenience.
    ///
    /// `dimming: 0` does not switch the bulb off; it leaves it on at `1`. Use
    /// [`PilotBuilder::state`](super::PilotBuilder::state) for that.
    Dimming(u8), "dimming", 1..=100
}

bounded_newtype! {
    /// Scene animation speed, in `10..=200`.
    ///
    /// Measured on `ESP25_SHRGB_01` fw 1.38.0: `9` and `201` are refused with
    /// `-32602`, `10` and `200` are accepted. Unlike `dimming`, the bulb
    /// enforces this one itself.
    Speed(u8), "speed", 10..=200
}

bounded_newtype! {
    /// Colour temperature in Kelvin, in `1000..=12000`.
    ///
    /// Measured on `ESP25_SHRGB_01` fw 1.38.0: `999` and `15000` are refused
    /// with `-32602`, `1000` and `12000` are accepted.
    ///
    /// This is the **wire** bound, and it is far wider than any bulb's usable
    /// range: the same hardware reports a `cctRange` of 2200–6500, accepts
    /// `12000`, and then reports `6500` back. Everything inside the wire bound
    /// is clamped into the model's own range, in both directions — `1000`
    /// reads back as `2200` — so an acknowledgement says nothing about the
    /// temperature having been honoured. For the range that actually means
    /// something, ask [`ModelConfig`](super::ModelConfig) /
    /// [`UserConfig`](super::UserConfig), or
    /// [`Bulb::kelvin_range`](crate::Bulb::kelvin_range).
    Kelvin(u16), "temp", 1000..=12_000
}

bounded_newtype! {
    /// Dual-head balance (`ratio`), in `0..=100`.
    ///
    /// Measured on `ESP25_SHRGB_01` fw 1.38.0: `101` is refused with `-32602`,
    /// `0`, `50` and `100` are accepted — by a single-head bulb, which takes
    /// the parameter despite having nothing to balance.
    Ratio(u8), "ratio", 0..=100
}

bounded_newtype! {
    /// Head selector (`devices`) for a **write**, in `1..=3`.
    ///
    /// Measured on `ESP25_SHRGB_01` fw 1.38.0, which is single-head: `1` and
    /// `3` are accepted, while `0`, `2` and `4` are refused with `-32602`.
    /// That `3` works where `2` — the second head — does not is the evidence
    /// for `3` meaning "every head"; it was previously a guess.
    ///
    /// **Writes only.** `getPilot` uses a zero-based index for the same key:
    /// `{"devices": 0}` is accepted and answers with `"devices": 1`, while
    /// `1`, `2` and `3` are all refused. This type cannot express `0`, and
    /// that is deliberate — reusing it for reads would be wrong.
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

    /// The edges below are the ones the bulb was actually probed at; see
    /// `docs/captures/param-ranges-esp25-shrgb-01-fw1.38.0.json` in the
    /// workspace repo.
    #[test]
    fn the_measured_edges_are_the_enforced_edges() {
        assert!(Speed::new(9).is_err());
        assert_eq!(Speed::new(10).unwrap().get(), 10);
        assert_eq!(Speed::new(200).unwrap().get(), 200);
        assert!(Speed::new(201).is_err());

        assert!(Kelvin::new(999).is_err());
        assert_eq!(Kelvin::new(1000).unwrap().get(), 1000);
        assert_eq!(Kelvin::new(2700).unwrap().get(), 2700);
        // Accepted by the bulb, and previously refused by this crate.
        assert_eq!(Kelvin::new(10_001).unwrap().get(), 10_001);
        assert_eq!(Kelvin::new(12_000).unwrap().get(), 12_000);
        assert!(Kelvin::new(12_001).is_err());

        assert_eq!(Ratio::new(0).unwrap().get(), 0);
        assert_eq!(Ratio::new(100).unwrap().get(), 100);
        assert!(Ratio::new(101).is_err());

        assert!(Devices::new(0).is_err());
        assert_eq!(Devices::new(1).unwrap().get(), 1);
        assert_eq!(Devices::new(3).unwrap().get(), 3);
        assert!(Devices::new(4).is_err());
    }

    #[test]
    fn open_newtypes_accept_their_whole_range() {
        assert_eq!(Channel::new(0).get(), 0);
        assert_eq!(Channel::new(255).get(), 255);
        assert_eq!(Channel::from(12u8).get(), 12);
        // Infallible on purpose: the measured 1..=248 write range is one
        // firmware's, and the bulb is left to refuse what it does not know.
        assert_eq!(SceneId::new(4).get(), 4);
        assert_eq!(u16::from(SceneId::new(1000)), 1000);
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
