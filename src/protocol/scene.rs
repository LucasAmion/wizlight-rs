//! The scene table: which light modes exist, what they are called, which
//! devices can play them, and which of them animate.
//!
//! A scene — WiZ calls them "light modes" — is an effect the bulb runs by
//! itself. One `setPilot` starts it and no further traffic is needed, which
//! makes it categorically different from driving colour frame by frame.
//!
//! # Where this table comes from
//!
//! Ids `1..=33` are WiZ's own [light-mode table][pro], published with the Pro
//! API. It is the only source that states, per mode, whether `speed` and
//! `dimming` apply — everything else in the ecosystem lists names and ids
//! only.
//!
//! Ids `34..=36` and `40` are not in it. Their names and availability come from
//! [`pywizlight`'s `scenes.py`][scenes], and nothing found so far says whether
//! they animate, so [`Scene::adjustable`] reports `None` for them rather than a
//! guess. `37`, `38` and `39` name nothing in any source consulted.
//!
//! **None of it is verified against hardware, and acceptance cannot verify it.**
//! Measured on `ESP25_SHRGB_01` fw 1.38.0: a write is accepted for every
//! `sceneId` in `1..=248`, including the ~200 that name no scene at all, so a
//! `success` says nothing about a scene existing. Settling this needs someone
//! looking at a bulb of each class, which is a job for the hardware pass rather
//! than for the test suite.
//!
//! # Where the sources disagree
//!
//! - **Dimmable white.** The vendor table also ticks `13` Cool white, `30`
//!   Golden white and `33` Diwali for DW, where `pywizlight` does not — 11 ids
//!   against its 8. The vendor's own prose, in the same document, contradicts
//!   its table again and claims DW gets only Wake up, Bedtime and Night light.
//!   The table is used here, being the primary source, and the whole
//!   disagreement is one of the things the hardware pass should settle.
//! - **Which modes animate.** Adafruit's CircuitPython library calls `6` Cozy
//!   static and `9` Wake up / `10` Bedtime dynamic; the vendor table says the
//!   opposite of all three. The vendor column is used, and note what it
//!   actually means: Wake up and Bedtime *do* change over time, but their
//!   duration is set in the app and there is no `speed` to send. So
//!   [`Adjustable::speed`] is "`speed` does something", which is the question a
//!   speed slider needs answered.
//! - **Tunable white.** Both sources agree exactly, which is the one part of
//!   the availability data with corroboration.
//!
//! # Rhythm and the user slots
//!
//! `pywizlight` also lists `1000` as Rhythm and `256..=265` as `Custom Mode
//! 1..10`. Neither is here, and neither can be expressed as a [`SceneId`]:
//! measured on `ESP25_SHRGB_01` fw 1.38.0, a **write** of `1000` or of anything
//! in `256..=265` is refused with `-32602`.
//!
//! The user slots are still real, on the read side and on other hardware:
//! `pywizlight`'s mapping comes from devices that **report** `256` and up while
//! playing a custom mode made in the app ([pywizlight#205][custom]). What
//! addresses them for a write on this firmware was not found, and Rhythm looks
//! like it stopped being a `sceneId` at all — WiZ documents it as an
//! automation, and neither the vendor's light-mode table nor its Pro API
//! mentions `1000`. So [`Scene::from_id`] answers `None` for a reported user
//! slot, which is the honest answer rather than a name we cannot address.
//!
//! [pro]: https://docs.pro.wizconnected.com/#light-modes
//! [scenes]: https://github.com/sbidy/pywizlight/blob/master/pywizlight/scenes.py
//! [custom]: https://github.com/sbidy/pywizlight/issues/205

use std::fmt;
use std::str::FromStr;

use serde::Serialize;

use super::model::BulbClass;
use super::types::SceneId;
use crate::error::{Error, Result};

/// Which pilot parameters a scene responds to, from WiZ's own light-mode table.
///
/// Both flags describe the *scene*, not the bulb: every model takes `speed` and
/// `dimming` on the wire, and a scene that ignores one of them still answers
/// `success`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Adjustable {
    /// `speed` sets the animation rate, i.e. the scene animates.
    ///
    /// False for the static modes, and for the two — Wake up and Bedtime — that
    /// change over time at a rate configured in the app instead.
    pub speed: bool,
    /// `dimming` sets the brightness while the scene runs.
    ///
    /// False only for Night light, which is a fixed low level. That matters to
    /// anything modulating brightness underneath a running scene: the scene
    /// starts, the `dimming` packets are accepted, and nothing changes.
    pub dimming: bool,
}

/// One entry in the scene table: a light mode the bulb can play by itself.
///
/// Scenes are looked up by id with [`Scene::from_id`] or by name with
/// [`Scene::from_name`], listed for a class with [`Scene::for_class`], and the
/// whole table is [`Scene::all`]. Nothing here needs a bulb, or a network.
///
/// ```
/// use wizlight::protocol::{BulbClass, Scene};
///
/// let scene: Scene = "deep-dive".parse()?;
/// assert_eq!(scene.id().get(), 23);
/// assert_eq!(scene.name(), "Deep dive");
/// assert!(scene.takes_speed());
/// // A colour animation, so nothing without RGB emitters can play it.
/// assert!(!scene.available_for(BulbClass::Tw));
/// # Ok::<(), wizlight::Error>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Scene {
    id: u16,
    name: &'static str,
    /// Which classes can play it, as `RGB` / `TW` / `DW` bits. Not serialised:
    /// a caller with a bulb has already filtered by class, and one without has
    /// [`Scene::available_for`].
    #[serde(skip)]
    classes: u8,
    /// `None` where no source documents the scene, rather than a guess.
    adjustable: Option<Adjustable>,
}

/// Playable by full-colour devices.
const RGB: u8 = 1 << 0;
/// Playable by tunable white devices, and by the tunable-white fans.
const TW: u8 = 1 << 1;
/// Playable by dimmable white devices.
const DW: u8 = 1 << 2;

/// Animates, and takes `dimming`.
const DYNAMIC: Option<Adjustable> = Some(Adjustable {
    speed: true,
    dimming: true,
});
/// Holds still, and takes `dimming`.
const STATIC: Option<Adjustable> = Some(Adjustable {
    speed: false,
    dimming: true,
});
/// Holds still at a fixed brightness: neither parameter does anything.
const FIXED: Option<Adjustable> = Some(Adjustable {
    speed: false,
    dimming: false,
});
/// In no source that states it. Not "static".
const UNDOCUMENTED: Option<Adjustable> = None;

/// The table itself, in id order.
///
/// Ids `1..=33` — availability and both flags — are the vendor's light-mode
/// table. `34`, `35`, `36` and `40` are `pywizlight`'s, which documents neither
/// flag. See the [module docs](self).
const SCENES: &[Scene] = &[
    Scene::new(1, "Ocean", RGB, DYNAMIC),
    Scene::new(2, "Romance", RGB, DYNAMIC),
    Scene::new(3, "Sunset", RGB, DYNAMIC),
    Scene::new(4, "Party", RGB, DYNAMIC),
    Scene::new(5, "Fireplace", RGB, DYNAMIC),
    Scene::new(6, "Cozy", RGB | TW, DYNAMIC),
    Scene::new(7, "Forest", RGB, DYNAMIC),
    Scene::new(8, "Pastel colors", RGB, DYNAMIC),
    Scene::new(9, "Wake up", RGB | TW | DW, STATIC),
    Scene::new(10, "Bedtime", RGB | TW | DW, STATIC),
    Scene::new(11, "Warm white", RGB | TW, STATIC),
    Scene::new(12, "Daylight", RGB | TW, STATIC),
    Scene::new(13, "Cool white", RGB | TW | DW, STATIC),
    Scene::new(14, "Night light", RGB | TW | DW, FIXED),
    Scene::new(15, "Focus", RGB | TW, STATIC),
    Scene::new(16, "Relax", RGB | TW, STATIC),
    Scene::new(17, "True colors", RGB, STATIC),
    Scene::new(18, "TV time", RGB | TW, STATIC),
    Scene::new(19, "Plant growth", RGB, STATIC),
    Scene::new(20, "Spring", RGB, DYNAMIC),
    Scene::new(21, "Summer", RGB, DYNAMIC),
    Scene::new(22, "Fall", RGB, DYNAMIC),
    Scene::new(23, "Deep dive", RGB, DYNAMIC),
    Scene::new(24, "Jungle", RGB, DYNAMIC),
    Scene::new(25, "Mojito", RGB, DYNAMIC),
    Scene::new(26, "Club", RGB, DYNAMIC),
    Scene::new(27, "Christmas", RGB, DYNAMIC),
    Scene::new(28, "Halloween", RGB, DYNAMIC),
    Scene::new(29, "Candlelight", RGB | TW | DW, DYNAMIC),
    Scene::new(30, "Golden white", RGB | TW | DW, DYNAMIC),
    Scene::new(31, "Pulse", RGB | TW | DW, DYNAMIC),
    Scene::new(32, "Steampunk", RGB | TW | DW, DYNAMIC),
    Scene::new(33, "Diwali", RGB | TW | DW, DYNAMIC),
    Scene::new(34, "White", RGB | DW, UNDOCUMENTED),
    Scene::new(35, "Alarm", RGB | TW | DW, UNDOCUMENTED),
    Scene::new(36, "Snowy sky", RGB, UNDOCUMENTED),
    // 37, 38 and 39 are unaccounted for; 40 is not a typo.
    Scene::new(40, "Dim-to-warm", RGB | TW, UNDOCUMENTED),
];

impl Scene {
    const fn new(id: u16, name: &'static str, classes: u8, adjustable: Option<Adjustable>) -> Self {
        Self {
            id,
            name,
            classes,
            adjustable,
        }
    }

    /// Every scene this crate knows, in id order.
    ///
    /// The table is a `const`, so a scene picker can be built with no bulb
    /// connected and nothing to await.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        SCENES
    }

    /// The scene with this id, or `None` if the table does not name one.
    ///
    /// This is the read-side lookup, for naming the `sceneId` in a
    /// [`Pilot`](super::Pilot). It takes a plain `u16` for that reason — a bulb
    /// may report an id that could not be sent, including `0` for "no scene,
    /// colour is active" and the `256..=265` user slots.
    #[must_use]
    pub fn from_id(id: u16) -> Option<Self> {
        SCENES.iter().copied().find(|scene| scene.id == id)
    }

    /// The scene with this name, matched leniently.
    ///
    /// Case is ignored, and so is anything that is not a letter or a digit, so
    /// `Deep dive`, `deep-dive` and `DEEPDIVE` are the same scene. That also
    /// makes the other spellings in the ecosystem resolve —
    /// `pywizlight`'s `Wake-up` and `Plantgrowth`, openHAB's `Bed Time` — which
    /// is why the rule strips separators instead of canonicalising them.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownScene`] if no scene matches.
    pub fn from_name(name: &str) -> Result<Self> {
        SCENES
            .iter()
            .copied()
            .find(|scene| normalised(scene.name).eq(normalised(name)))
            .ok_or_else(|| Error::UnknownScene {
                message: format!(
                    "no scene is called `{name}`; names are matched ignoring case, spaces \
                     and punctuation"
                ),
            })
    }

    /// Every scene a device of `class` can play, in id order.
    ///
    /// This is the class's table, not a given device's: a dimmable white
    /// *module* plays these, while a `DIMTRIACS` wall switch of the same class
    /// plays nothing. [`BulbType::scenes`](super::BulbType::scenes) is the one
    /// that knows the difference.
    pub fn for_class(class: BulbClass) -> impl Iterator<Item = Self> {
        SCENES
            .iter()
            .copied()
            .filter(move |scene| scene.available_for(class))
    }

    /// Its id, ready to be sent.
    #[must_use]
    pub const fn id(self) -> SceneId {
        // Every entry in the table is by definition a scene the table names,
        // which is all `SceneId` validates.
        SceneId::new_unchecked(self.id)
    }

    /// Its display name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Which parameters it responds to, or `None` where no source says.
    ///
    /// The three states are the point: `Some(Adjustable { speed: false, .. })`
    /// means a source states the scene is static, while `None` means nobody
    /// documents it. Treating the second as the first would hide a speed
    /// control that may well work.
    #[must_use]
    pub const fn adjustable(self) -> Option<Adjustable> {
        self.adjustable
    }

    /// Whether a `speed` may sensibly accompany this scene.
    ///
    /// True when the scene animates, and also when nothing documents it —
    /// refusing on the strength of an absent row would be worse than sending a
    /// parameter the bulb ignores. This is what
    /// [`PilotBuilder`](super::PilotBuilder) enforces.
    #[must_use]
    pub const fn takes_speed(self) -> bool {
        match self.adjustable {
            Some(adjustable) => adjustable.speed,
            None => true,
        }
    }

    /// Whether a device of `class` can play it.
    ///
    /// Sockets and dimmable fans can play nothing. A tunable-white fan gets the
    /// tunable-white list, since its light is one — `pywizlight` returns
    /// nothing for either kind of fan, which looks like an omission in its
    /// table rather than a fact about the hardware.
    #[must_use]
    pub const fn available_for(self, class: BulbClass) -> bool {
        let bit = match class {
            BulbClass::Rgb => RGB,
            BulbClass::Tw | BulbClass::FanTw => TW,
            BulbClass::Dw => DW,
            BulbClass::Socket | BulbClass::FanDim => return false,
        };
        self.classes & bit != 0
    }
}

impl FromStr for Scene {
    type Err = Error;

    fn from_str(name: &str) -> Result<Self> {
        Self::from_name(name)
    }
}

impl fmt::Display for Scene {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)
    }
}

impl From<Scene> for SceneId {
    fn from(scene: Scene) -> Self {
        scene.id()
    }
}

/// A name reduced to what matching cares about: letters and digits, lowercased.
fn normalised(name: &str) -> impl Iterator<Item = char> + '_ {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The counts are the shape of the table, and a wrong one is the failure
    /// mode of a hand-copied table. RGB plays everything; the other two are the
    /// vendor's columns, plus the four ids the vendor table does not cover.
    #[test]
    fn per_class_counts_match_the_sources() {
        assert_eq!(Scene::all().len(), 37);
        assert_eq!(Scene::for_class(BulbClass::Rgb).count(), 37);
        assert_eq!(Scene::for_class(BulbClass::Tw).count(), 17);
        assert_eq!(Scene::for_class(BulbClass::Dw).count(), 11);
        // A tunable-white fan is a tunable-white light with a fan attached.
        assert_eq!(Scene::for_class(BulbClass::FanTw).count(), 17);
        assert_eq!(Scene::for_class(BulbClass::Socket).count(), 0);
        assert_eq!(Scene::for_class(BulbClass::FanDim).count(), 0);
    }

    /// `pywizlight`'s `TW_SCENES` is the one availability list two sources
    /// agree on, so it is pinned id by id rather than by count.
    #[test]
    fn the_tunable_white_list_is_the_one_both_sources_agree_on() {
        let tw: Vec<u16> = Scene::for_class(BulbClass::Tw)
            .map(|scene| scene.id().get())
            .collect();
        assert_eq!(
            tw,
            [
                6, 9, 10, 11, 12, 13, 14, 15, 16, 18, 29, 30, 31, 32, 33, 35, 40
            ]
        );
    }

    /// The vendor's DW column, plus `34` and `35` from `pywizlight`. The three
    /// ids `pywizlight` leaves out — `13`, `30`, `33` — are the divergence, and
    /// this test is where it is visible.
    #[test]
    fn the_dimmable_white_list_follows_the_vendor_table() {
        let dw: Vec<u16> = Scene::for_class(BulbClass::Dw)
            .map(|scene| scene.id().get())
            .collect();
        assert_eq!(dw, [9, 10, 13, 14, 29, 30, 31, 32, 33, 34, 35]);
    }

    #[test]
    fn ids_and_names_round_trip() {
        for scene in Scene::all() {
            let id = scene.id().get();
            assert_eq!(Scene::from_id(id), Some(*scene));
            assert_eq!(Scene::from_name(scene.name()).unwrap(), *scene);
            assert_eq!(scene.to_string(), scene.name());
        }
    }

    #[test]
    fn names_are_unique_and_the_table_is_sorted() {
        let mut previous = 0;
        for scene in Scene::all() {
            let id = scene.id().get();
            assert!(id > previous, "the table is out of order at {id}");
            previous = id;
        }
        for scene in Scene::all() {
            let clashes = Scene::all()
                .iter()
                .filter(|other| normalised(other.name()).eq(normalised(scene.name())))
                .count();
            assert_eq!(clashes, 1, "`{}` is not a unique name", scene.name());
        }
    }

    #[test]
    fn names_are_matched_leniently() {
        let deep_dive = Scene::from_id(23).unwrap();
        for spelling in [
            "Deep dive",
            "deep dive",
            "deep-dive",
            "DEEPDIVE",
            "Deep_Dive",
        ] {
            assert_eq!(Scene::from_name(spelling).unwrap(), deep_dive, "{spelling}");
        }

        // The spellings the rest of the ecosystem uses have to resolve, since
        // they are what a user reads elsewhere and types at us.
        for (spelling, id) in [
            // pywizlight
            ("Wake-up", 9),
            ("Plantgrowth", 19),
            ("Pastel colors", 8),
            ("TV time", 18),
            ("Dim-to-warm", 40),
            // openHAB
            ("Bed Time", 10),
            ("Plant Growth", 19),
            ("Wakeup", 9),
            // Adafruit
            ("Deepdive", 23),
            ("Wake up", 9),
        ] {
            assert_eq!(
                Scene::from_name(spelling).unwrap().id().get(),
                id,
                "{spelling}"
            );
        }
    }

    #[test]
    fn an_unknown_name_names_itself_in_the_error() {
        let err = Scene::from_name("Cozy Whitest").unwrap_err();
        assert!(
            matches!(&err, Error::UnknownScene { message } if message.contains("`Cozy Whitest`")),
            "{err}"
        );
    }

    #[test]
    fn unknown_ids_are_none_rather_than_an_error() {
        // 0 is "no scene" on a read, 37..=39 are unaccounted for, 256 is a user
        // slot this firmware will not take, 1000 was Rhythm.
        for id in [0, 37, 38, 39, 41, 100, 256, 265, 1000] {
            assert_eq!(Scene::from_id(id), None, "{id}");
        }
    }

    #[test]
    fn the_speed_flag_is_three_state() {
        // Documented as animating.
        let party = Scene::from_id(4).unwrap();
        assert_eq!(party.adjustable().map(|a| a.speed), Some(true));
        assert!(party.takes_speed());

        // Documented as static: a `speed` alongside it does nothing.
        let warm_white = Scene::from_id(11).unwrap();
        assert_eq!(warm_white.adjustable().map(|a| a.speed), Some(false));
        assert!(!warm_white.takes_speed());

        // Not documented either way, so `speed` is allowed through.
        let snowy_sky = Scene::from_id(36).unwrap();
        assert_eq!(snowy_sky.adjustable(), None);
        assert!(snowy_sky.takes_speed());
    }

    /// Night light is the only scene the vendor table says ignores `dimming`,
    /// which is exactly the case that breaks modulating brightness under a
    /// running scene.
    #[test]
    fn night_light_is_the_only_scene_that_ignores_dimming() {
        let undimmable: Vec<&'static str> = Scene::all()
            .iter()
            .filter(|scene| scene.adjustable().is_some_and(|a| !a.dimming))
            .map(|scene| scene.name())
            .collect();
        assert_eq!(undimmable, ["Night light"]);
    }

    #[test]
    fn a_scene_serialises_as_its_id_name_and_flags() {
        let json = serde_json::to_value(Scene::from_id(23).unwrap()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "id": 23,
                "name": "Deep dive",
                "adjustable": {"speed": true, "dimming": true},
            })
        );
    }
}
