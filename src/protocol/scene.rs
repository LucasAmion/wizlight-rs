//! The scene table: which light modes exist, what they are called, how they
//! behave, and which devices can play them.
//!
//! A scene — WiZ calls them "light modes" — is an effect the bulb runs by
//! itself. One `setPilot` starts it and no further traffic is needed, which
//! makes it categorically different from driving colour frame by frame.
//!
//! # Where this table comes from
//!
//! Every id here was **written to an `ESP25_SHRGB_01` on fw 1.38.0 and read
//! back**, and the WiZ app was then walked through mode by mode with `getPilot`
//! polled throughout, so the vendor's own software supplied the names and the
//! grouping. That measurement is the authority for this module, and it
//! disagrees with every published source in at least one place.
//!
//! The bulb makes it measurable by reporting only the parameters that apply: a
//! running scene's `getPilot` carries `speed` if and only if its rate can be
//! set, and `dimming` if and only if its brightness can be. So
//! [`Adjustable`] is observed rather than transcribed, and it corrects
//! [WiZ's own light-mode table][pro] twice — that table claims an adjustable
//! speed for Cozy and Candlelight, and neither reports one. Cozy was confirmed
//! by eye to hold still. It also under-reports the undimmable scenes, flagging
//! only Night light where Wake up and Alarm ignore `dimming` too.
//!
//! # Two different questions
//!
//! [`Scene::animates`] and [`Adjustable::speed`] are not the same thing, and
//! conflating them is what the published sources do:
//!
//! - **Wake up** and **Bedtime** change over minutes, at a rate configured in
//!   the app. The WiZ app files them under their own heading, *Progresivo* —
//!   [`Category::Progressive`] here — and the bulb reports no `speed` for
//!   them. They animate; their rate is not ours to set.
//! - **Candlelight** and **Alarm** animate too, and likewise take no `speed`.
//! - Everything under [`Category::White`] and [`Category::Functional`] holds
//!   still.
//!
//! So hide a speed slider on [`Adjustable::speed`], and decide whether a scene
//! is "moving" with [`Scene::animates`].
//!
//! # What the sources got wrong
//!
//! - Ids **`38`, `39` and `41`** are real scenes that nothing in the ecosystem
//!   lists — static whites at 3500 K, 5000 K, and one that reports no `temp` at
//!   all. The app does not offer them, and neither does `pywizlight`, openHAB
//!   or WiZ's own table.
//! - Id **`37` is not a scene.** Writing it is accepted and puts the bulb in
//!   colour-temperature mode, reporting `sceneId: 0, temp: 2200`. It is an
//!   alias for a CCT, so [`SceneId`] refuses it and says what to send instead.
//! - Ids **`42..=248` are accepted and clamped to `41`**. They look valid — the
//!   bulb answers `success` — and do something no caller asked for, so
//!   [`SceneId`] refuses them too.
//! - The **user slots `256..=265` work**, contradicting an earlier measurement
//!   here that found `256` refused. The slot was simply empty: saving a custom
//!   mode in the app makes its id writable, and saving a second one makes `257`
//!   work while the rest stay refused. See [`SceneId::user_slot`].
//! - Per-class availability is the **one thing still unverified**, since only
//!   RGB hardware was on hand. It is [WiZ's table][pro] for ids `1..=33` and
//!   `pywizlight`'s for `34..=36` and `40`, and the two disagree about
//!   dimmable white — see [`Scene::available_for`]. `38`, `39` and `41` are
//!   marked colour-only because that is the only class they were seen on.
//!
//! [pro]: https://docs.pro.wizconnected.com/#light-modes

use std::fmt;
use std::str::FromStr;

use serde::Serialize;

use super::model::BulbClass;
use super::types::SceneId;
use crate::error::{Error, Result};

/// How a scene behaves, as the WiZ app groups its light modes.
///
/// Taken from the app's own headings, which are the only source that separates
/// a scene that animates from one whose rate can be set. The four scenes the
/// app does not offer are grouped by what they were measured to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// A static white. Most report the colour temperature they run at; see
    /// [`Scene::kelvin`].
    White,
    /// A static scene for a purpose — reading, watching television, growing
    /// plants.
    Functional,
    /// Changes over minutes, at a rate set in the WiZ app rather than by
    /// `speed`. Wake up and Bedtime.
    Progressive,
    /// An animation. Most, but not all, take a `speed`.
    Dynamic,
}

/// Which pilot parameters a scene responds to.
///
/// Measured, not documented: the bulb reports a parameter in `getPilot` if and
/// only if the running scene uses it. A parameter that does not apply is still
/// **accepted with `success` and silently discarded**, which is why this is
/// worth knowing before sending one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Adjustable {
    /// `speed` sets the animation rate.
    ///
    /// False for every static scene, and also for the animating ones whose rate
    /// is not exposed: Wake up, Bedtime, Candlelight and Alarm.
    pub speed: bool,
    /// `dimming` sets the brightness while the scene runs.
    ///
    /// False for Wake up, Night light and Alarm, which drive their own
    /// brightness. Anything modulating brightness underneath a scene has to
    /// avoid those three: the packets are accepted and nothing changes.
    pub dimming: bool,
}

/// One entry in the scene table: a light mode the bulb can play by itself.
///
/// Looked up by id with [`Scene::from_id`] or by name with
/// [`Scene::from_name`], listed for a class with [`Scene::for_class`], and the
/// whole table is [`Scene::all`]. Nothing here needs a bulb, or a network.
///
/// ```
/// use wizlight::protocol::{BulbClass, Category, Scene};
///
/// let scene: Scene = "deep-dive".parse()?;
/// assert_eq!(scene.id().get(), 23);
/// assert_eq!(scene.name(), Some("Deep dive"));
/// assert_eq!(scene.category(), Category::Dynamic);
/// assert!(scene.animates() && scene.adjustable().speed);
/// // A colour animation, so nothing without RGB emitters can play it.
/// assert!(!scene.available_for(BulbClass::Tw));
///
/// // Wake up animates too, but its rate is set in the app, not by us.
/// let wake_up = Scene::from_id(9).expect("9 is Wake up");
/// assert!(wake_up.animates() && !wake_up.adjustable().speed);
/// # Ok::<(), wizlight::Error>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Scene {
    id: u16,
    /// `None` for the three ids no source names. [`Display`](fmt::Display)
    /// falls back to `Scene 38`.
    name: Option<&'static str>,
    category: Category,
    /// The colour temperature the bulb reports while it runs, where it reports
    /// one.
    kelvin: Option<u16>,
    /// Which classes can play it, as `RGB` / `TW` / `DW` bits. Not serialised:
    /// a caller with a bulb has already filtered by class, and one without has
    /// [`Scene::available_for`].
    #[serde(skip)]
    classes: u8,
    adjustable: Adjustable,
}

/// Playable by full-colour devices.
const RGB: u8 = 1 << 0;
/// Playable by tunable white devices, and by the tunable-white fans.
const TW: u8 = 1 << 1;
/// Playable by dimmable white devices.
const DW: u8 = 1 << 2;

/// Takes both `speed` and `dimming`.
const PACED: Adjustable = Adjustable {
    speed: true,
    dimming: true,
};
/// Takes `dimming` only — either static, or animating at a rate it owns.
const DIMMABLE: Adjustable = Adjustable {
    speed: false,
    dimming: true,
};
/// Takes neither. Wake up, Night light and Alarm.
const FIXED: Adjustable = Adjustable {
    speed: false,
    dimming: false,
};

/// The table, in id order.
///
/// Note the gap: `37` is missing because it is not a scene, and `41` is the
/// last one because everything above it clamps onto it. See the
/// [module docs](self).
const SCENES: &[Scene] = &[
    Scene::new(1, "Ocean", Category::Dynamic, RGB, PACED),
    Scene::new(2, "Romance", Category::Dynamic, RGB, PACED),
    Scene::new(3, "Sunset", Category::Dynamic, RGB, PACED),
    Scene::new(4, "Party", Category::Dynamic, RGB, PACED),
    Scene::new(5, "Fireplace", Category::Dynamic, RGB, PACED),
    // WiZ's table says this one takes a speed. It does not, and it holds still.
    Scene::new(6, "Cozy", Category::Functional, RGB | TW, DIMMABLE),
    Scene::new(7, "Forest", Category::Dynamic, RGB, PACED),
    Scene::new(8, "Pastel colors", Category::Dynamic, RGB, PACED),
    Scene::new(9, "Wake up", Category::Progressive, RGB | TW | DW, FIXED),
    Scene::new(
        10,
        "Bedtime",
        Category::Progressive,
        RGB | TW | DW,
        DIMMABLE,
    ),
    Scene::white(11, "Warm white", 2700, RGB | TW),
    Scene::white(12, "Daylight", 4200, RGB | TW),
    Scene::white(13, "Cool white", 6500, RGB | TW | DW),
    Scene::new(
        14,
        "Night light",
        Category::Functional,
        RGB | TW | DW,
        FIXED,
    ),
    Scene::new(15, "Focus", Category::Functional, RGB | TW, DIMMABLE),
    Scene::new(16, "Relax", Category::Functional, RGB | TW, DIMMABLE),
    Scene::new(17, "True colors", Category::Functional, RGB, DIMMABLE),
    Scene::new(18, "TV time", Category::Functional, RGB | TW, DIMMABLE),
    Scene::new(19, "Plant growth", Category::Functional, RGB, DIMMABLE),
    Scene::new(20, "Spring", Category::Dynamic, RGB, PACED),
    Scene::new(21, "Summer", Category::Dynamic, RGB, PACED),
    Scene::new(22, "Fall", Category::Dynamic, RGB, PACED),
    Scene::new(23, "Deep dive", Category::Dynamic, RGB, PACED),
    Scene::new(24, "Jungle", Category::Dynamic, RGB, PACED),
    Scene::new(25, "Mojito", Category::Dynamic, RGB, PACED),
    Scene::new(26, "Club", Category::Dynamic, RGB, PACED),
    Scene::new(27, "Christmas", Category::Dynamic, RGB, PACED),
    Scene::new(28, "Halloween", Category::Dynamic, RGB, PACED),
    // Animates, but the rate is not exposed — WiZ's table says otherwise.
    Scene::new(
        29,
        "Candlelight",
        Category::Dynamic,
        RGB | TW | DW,
        DIMMABLE,
    ),
    Scene::new(30, "Golden white", Category::Dynamic, RGB | TW | DW, PACED),
    Scene::new(31, "Pulse", Category::Dynamic, RGB | TW | DW, PACED),
    Scene::new(32, "Steampunk", Category::Dynamic, RGB | TW | DW, PACED),
    Scene::new(33, "Diwali", Category::Dynamic, RGB | TW | DW, PACED),
    Scene::white(34, "White", 4000, RGB | DW),
    Scene::new(35, "Alarm", Category::Dynamic, RGB | TW | DW, FIXED),
    Scene::new(36, "Snowy sky", Category::Dynamic, RGB, PACED),
    // 37 is not a scene: it writes a 2200 K colour temperature.
    Scene::unnamed(38, Some(3500)),
    Scene::unnamed(39, Some(5000)),
    Scene::new(40, "Dim-to-warm", Category::White, RGB | TW, DIMMABLE),
    // A white that reports no `temp`, unlike every other one.
    Scene::unnamed(41, None),
];

impl Scene {
    const fn new(
        id: u16,
        name: &'static str,
        category: Category,
        classes: u8,
        adjustable: Adjustable,
    ) -> Self {
        Self {
            id,
            name: Some(name),
            category,
            kelvin: None,
            classes,
            adjustable,
        }
    }

    /// A static white that reports the temperature it runs at.
    const fn white(id: u16, name: &'static str, kelvin: u16, classes: u8) -> Self {
        Self {
            id,
            name: Some(name),
            category: Category::White,
            kelvin: Some(kelvin),
            classes,
            adjustable: DIMMABLE,
        }
    }

    /// One of the three scenes no source names, seen only on colour hardware.
    const fn unnamed(id: u16, kelvin: Option<u16>) -> Self {
        Self {
            id,
            name: None,
            category: Category::White,
            kelvin,
            classes: RGB,
            adjustable: DIMMABLE,
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
    /// reports ids that are not scenes, including `0` for "no scene, colour is
    /// active" and `256..=265` for a custom mode made in the app.
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
    /// The three unnamed scenes cannot be found this way; use
    /// [`from_id`](Scene::from_id).
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownScene`] if no scene matches.
    pub fn from_name(name: &str) -> Result<Self> {
        SCENES
            .iter()
            .copied()
            .find(|scene| match scene.name {
                Some(known) => normalised(known).eq(normalised(name)),
                None => false,
            })
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
        // which is what `SceneId` checks.
        SceneId::new_unchecked(self.id)
    }

    /// Its display name, where anything names it.
    ///
    /// `None` for `38`, `39` and `41`, which are real scenes that no
    /// documentation, no library and not even the WiZ app gives a name to.
    /// [`Display`](fmt::Display) writes `Scene 38` for those.
    #[must_use]
    pub const fn name(self) -> Option<&'static str> {
        self.name
    }

    /// How it behaves, as the WiZ app groups its light modes.
    #[must_use]
    pub const fn category(self) -> Category {
        self.category
    }

    /// Whether the light changes over time.
    ///
    /// **Not the same as [`Adjustable::speed`]**: Wake up, Bedtime, Candlelight
    /// and Alarm all animate without taking a `speed`. Use this to decide
    /// whether a scene is "moving", and `adjustable().speed` to decide whether
    /// to offer a rate control.
    #[must_use]
    pub const fn animates(self) -> bool {
        matches!(self.category, Category::Dynamic | Category::Progressive)
    }

    /// Which parameters it responds to.
    #[must_use]
    pub const fn adjustable(self) -> Adjustable {
        self.adjustable
    }

    /// The colour temperature the bulb reports while this scene runs.
    ///
    /// Only the whites report one, and not even all of them — `41` renders
    /// white and reports nothing, and Dim-to-warm varies its own temperature by
    /// brightness, which is the whole point of it.
    #[must_use]
    pub const fn kelvin(self) -> Option<u16> {
        self.kelvin
    }

    /// Whether a device of `class` can play it.
    ///
    /// **The one part of this module that is not measured.** Only colour
    /// hardware was available, so the tunable-white and dimmable-white lists
    /// are inherited: [WiZ's own table][pro] for `1..=33`, `pywizlight` for
    /// `34..=36` and `40`. The two agree exactly on tunable white and disagree
    /// on dimmable white, where WiZ also lists Cool white, Golden white and
    /// Diwali; WiZ is followed here, though its prose contradicts its own
    /// table. The three unnamed scenes are reported colour-only because that is
    /// the only class they have been seen on, not because anything says so.
    ///
    /// Sockets and dimmable fans play nothing. A tunable-white fan gets the
    /// tunable-white list, since its light is one — `pywizlight` returns
    /// nothing for either kind of fan, which looks like an omission in its
    /// table rather than a fact about the hardware.
    ///
    /// [pro]: https://docs.pro.wizconnected.com/#light-modes
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
        match self.name {
            Some(name) => f.write_str(name),
            None => write!(f, "Scene {}", self.id),
        }
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

    /// The shape of the table, and the id gaps that are the whole story: no
    /// `37`, and nothing above `41`.
    #[test]
    fn the_table_covers_the_measured_scene_space() {
        let ids: Vec<u16> = Scene::all().iter().map(|s| s.id().get()).collect();
        let expected: Vec<u16> = (1..=36).chain(38..=41).collect();
        assert_eq!(ids, expected);
    }

    #[test]
    fn per_class_counts() {
        assert_eq!(Scene::for_class(BulbClass::Rgb).count(), 40);
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

    /// WiZ's DW column, plus `34` and `35` from `pywizlight`. The three ids
    /// `pywizlight` leaves out — `13`, `30`, `33` — are the divergence, and this
    /// test is where it is visible.
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
            assert_eq!(Scene::from_id(scene.id().get()), Some(*scene));
            match scene.name() {
                Some(name) => {
                    assert_eq!(Scene::from_name(name).unwrap(), *scene);
                    assert_eq!(scene.to_string(), name);
                }
                // The unnamed three are findable by id only, and label
                // themselves rather than pretending to a name.
                None => assert_eq!(scene.to_string(), format!("Scene {}", scene.id().get())),
            }
        }
    }

    #[test]
    fn names_are_unique() {
        for scene in Scene::all() {
            let Some(name) = scene.name() else { continue };
            let clashes = Scene::all()
                .iter()
                .filter_map(|other| other.name())
                .filter(|other| normalised(other).eq(normalised(name)))
                .count();
            assert_eq!(clashes, 1, "`{name}` is not a unique name");
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
            ("Wake-up", 9),      // pywizlight
            ("Plantgrowth", 19), // pywizlight
            ("TV time", 18),
            ("Dim-to-warm", 40),
            ("Bed Time", 10), // openHAB
            ("Wakeup", 9),    // openHAB
            ("Deepdive", 23), // Adafruit
            ("Pastel Colors", 8),
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
    fn ids_that_are_not_scenes_are_none() {
        // 0 means "no scene" on a read; 37 writes a colour temperature rather
        // than a scene; 42 and up clamp onto 41; 256 is a user slot; 1000 was
        // Rhythm, and is refused outright.
        for id in [0, 37, 42, 100, 248, 256, 265, 1000] {
            assert_eq!(Scene::from_id(id), None, "{id}");
        }
    }

    /// Animating and taking a `speed` are different questions, and the four
    /// scenes where they differ are the reason both exist.
    #[test]
    fn animating_is_not_the_same_as_taking_a_speed() {
        let animates_without_a_speed = [9, 10, 29, 35];
        for id in animates_without_a_speed {
            let scene = Scene::from_id(id).unwrap();
            assert!(scene.animates(), "{scene} should animate");
            assert!(!scene.adjustable().speed, "{scene} should take no speed");
        }

        let party = Scene::from_id(4).unwrap();
        assert!(party.animates() && party.adjustable().speed);

        // Measured: WiZ's table claims a speed for Cozy, the bulb reports none,
        // and the light was watched holding still.
        let cozy = Scene::from_id(6).unwrap();
        assert_eq!(cozy.category(), Category::Functional);
        assert!(!cozy.animates() && !cozy.adjustable().speed);
    }

    /// Wake up, Night light and Alarm swallow `dimming`. Anything modulating
    /// brightness under a scene has to know.
    #[test]
    fn the_undimmable_scenes_are_the_measured_three() {
        let undimmable: Vec<u16> = Scene::all()
            .iter()
            .filter(|scene| !scene.adjustable().dimming)
            .map(|scene| scene.id().get())
            .collect();
        assert_eq!(undimmable, [9, 14, 35]);
    }

    #[test]
    fn the_whites_report_the_temperature_they_run_at() {
        let ladder: Vec<(u16, u16)> = Scene::all()
            .iter()
            .filter_map(|scene| Some((scene.id().get(), scene.kelvin()?)))
            .collect();
        assert_eq!(
            ladder,
            [
                (11, 2700),
                (12, 4200),
                (13, 6500),
                (34, 4000),
                (38, 3500),
                (39, 5000)
            ]
        );

        // Every scene reporting a temperature is a white, but not every white
        // reports one: Dim-to-warm varies its own, and 41 reports nothing.
        for scene in Scene::all().iter().filter(|s| s.kelvin().is_some()) {
            assert_eq!(scene.category(), Category::White, "{scene}");
        }
        assert_eq!(Scene::from_id(40).unwrap().kelvin(), None);
        assert_eq!(Scene::from_id(41).unwrap().kelvin(), None);
    }

    #[test]
    fn a_scene_serialises_as_its_id_name_and_behaviour() {
        let json = serde_json::to_value(Scene::from_id(23).unwrap()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "id": 23,
                "name": "Deep dive",
                "category": "dynamic",
                "kelvin": null,
                "adjustable": {"speed": true, "dimming": true},
            })
        );
    }
}
