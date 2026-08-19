//! Validated pilot parameter newtypes.
//!
//! The bulb is not a reliable validator: an out-of-range `dimming` is silently
//! clamped and still reports success, while an out-of-range `temp` errors.
//! Ranges are therefore enforced here, before anything is serialised. One of
//! these is not a range at all — a [`SceneId`] is checked against the scene
//! table, because the ids the bulb accepts are a superset of the scenes it can
//! play.
//!
//! These are **write-side** types. Nothing here implements `Deserialize`, and
//! that is deliberate: a bulb is free to *report* a value these constructors
//! would refuse — `dimming: 0` on an off bulb is the known case — and a
//! validating parse would turn that into a hard error. Results use the plain
//! integer, per the forward-compatibility rule in [`super`].

use serde::Serialize;

use super::scene::Scene;
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

/// A WiZ scene identifier: an id that names a scene in [`Scene`]'s table.
///
/// Construction goes through the table rather than through a range, because a
/// range is not the question. Measured on `ESP25_SHRGB_01` fw 1.38.0: a write
/// is accepted for the whole of `1..=248` and refused with `-32602` outside it,
/// and the accepted set includes the ~200 ids that name no scene at all. So the
/// bulb's own bound proves nothing, and `SceneId::new(100)` failing is the
/// point — the alternative is a `success` for a scene that does not exist.
///
/// ```
/// use wizlight::protocol::SceneId;
///
/// assert_eq!(SceneId::new(4)?.scene().name(), "Party");
/// // Inside the range the bulb accepts, but no scene has this id.
/// assert!(SceneId::new(100).is_err());
/// # Ok::<(), wizlight::Error>(())
/// ```
///
/// This is the **write** side. A bulb reports ids this type cannot hold — `0`
/// for "no scene, colour is active", and `256..=265` for a custom mode made in
/// the app — so [`Pilot::scene_id`](super::Pilot) is a plain `u16` and
/// [`Scene::from_id`] is how it gets a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct SceneId(u16);

impl SceneId {
    /// Builds a `sceneId`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownScene`] if no scene in the table has this id.
    pub fn new(value: u16) -> Result<Self> {
        Scene::from_id(value)
            .map(Scene::id)
            .ok_or_else(|| Error::UnknownScene {
                message: format!(
                    "no scene has id {value}; ids run 1..=36 and 40, and the bulb accepts \
                     far more than it can play"
                ),
            })
    }

    /// Builds one from an id already known to be in the table.
    pub(super) const fn new_unchecked(value: u16) -> Self {
        Self(value)
    }

    /// The raw value.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }

    /// The scene it names.
    ///
    /// Infallible: an id that does not name a scene cannot be a `SceneId`.
    #[must_use]
    pub fn scene(self) -> Scene {
        Scene::from_id(self.0).expect("a SceneId is only built from the scene table")
    }
}

impl TryFrom<u16> for SceneId {
    type Error = Error;

    fn try_from(value: u16) -> Result<Self> {
        Self::new(value)
    }
}

impl From<SceneId> for u16 {
    fn from(value: SceneId) -> Self {
        value.0
    }
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
    /// enforces this one itself. WiZ's own Pro API documents the range as
    /// 20–200; the hardware takes `10`, so the measurement wins.
    ///
    /// It only means something while a scene that animates is running — see
    /// [`Scene::takes_speed`] and
    /// [`PilotBuilder::speed`](super::PilotBuilder::speed).
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
    fn every_channel_value_is_a_valid_one() {
        assert_eq!(Channel::new(0).get(), 0);
        assert_eq!(Channel::new(255).get(), 255);
        assert_eq!(Channel::from(12u8).get(), 12);
    }

    /// A `sceneId` is checked against the scene table rather than against the
    /// range the bulb accepts, which is far wider than the scenes it can play.
    #[test]
    fn a_scene_id_has_to_name_a_scene() {
        assert_eq!(SceneId::new(4).unwrap().get(), 4);
        assert_eq!(u16::from(SceneId::try_from(40).unwrap()), 40);
        assert_eq!(SceneId::new(23).unwrap().scene().name(), "Deep dive");

        // Accepted by the measured hardware, and still not a scene.
        for id in [0, 37, 100, 248, 256, 1000] {
            assert!(SceneId::new(id).is_err(), "{id}");
        }
        let message = SceneId::new(100).unwrap_err().to_string();
        assert!(message.contains("no scene has id 100"), "{message}");
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
