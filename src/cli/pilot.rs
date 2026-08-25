//! The commands that read and write a bulb's pilot state.

use anyhow::Context as _;
use clap::Args;
use serde_json::{Value, json};

use super::Report;
use crate::protocol::{BulbType, Channel, Dimming, Kelvin, Scene, SceneId, Speed};
use crate::{Bulb, Pilot, PilotBuilder};

/// The state options `on` and `set` share.
#[derive(Args, Debug, Clone, PartialEq, Eq, Default)]
pub struct StateOptions {
    /// The colour to show, in one of the several ways of saying it.
    #[command(flatten)]
    pub colour: ColourOptions,

    /// Scene animation speed, 10-200.
    #[arg(long, value_name = "SPEED", value_parser = speed)]
    pub speed: Option<Speed>,

    /// Brightness percent, 1-100.
    #[arg(long, short = 'b', value_name = "PERCENT", value_parser = brightness)]
    pub brightness: Option<Dimming>,
}

/// The mutually exclusive ways of saying what a bulb should show.
///
/// clap enforces the exclusivity, so `--rgb ... --kelvin ...` is a usage error
/// before anything is sent. [`PilotBuilder`] refuses the same combination, but
/// it can only do so once the command is already running, and its message has
/// no `--flag` in it to point at.
#[derive(Args, Debug, Clone, PartialEq, Eq, Default)]
#[group(multiple = false)]
pub struct ColourOptions {
    /// Colour as `R,G,B`, each 0-255.
    #[arg(long, value_name = "R,G,B", value_parser = rgb)]
    pub rgb: Option<[Channel; 3]>,

    /// Colour as `H,S,V` — hue 0-360, saturation and value 0-100.
    #[arg(long, value_name = "H,S,V", value_parser = hsv)]
    pub hsv: Option<[Channel; 3]>,

    /// White colour temperature, in Kelvin.
    #[arg(long, short = 'k', value_name = "KELVIN", value_parser = kelvin)]
    pub kelvin: Option<Kelvin>,

    /// Scene, by id or by name.
    #[arg(long, short = 's', value_name = "SCENE", value_parser = scene)]
    pub scene: Option<SceneId>,
}

impl StateOptions {
    /// Whether nothing at all was asked for.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Rejects a `set` with nothing to set, as a usage error.
    ///
    /// `on` is complete on its own and `set` is not, which no single clap
    /// group can express: the options live in one struct shared by both. So
    /// the rule is checked after parsing and reported through clap anyway,
    /// which keeps the message in clap's voice and the exit code at 2 with
    /// every other usage error.
    ///
    /// # Errors
    ///
    /// A clap error naming the options that would have made it valid.
    pub fn require_something(&self, command: &mut clap::Command) -> Result<(), clap::Error> {
        if self.is_empty() {
            return Err(command.error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "`set` needs something to set: one of --rgb, --hsv, --kelvin, --scene, --speed \
                 or --brightness",
            ));
        }
        Ok(())
    }

    /// The colour triple, however it was spelled.
    fn channels(&self) -> Option<[Channel; 3]> {
        self.colour.rgb.or(self.colour.hsv)
    }

    /// Adds everything that was asked for to a builder.
    ///
    /// # Errors
    ///
    /// Whatever [`PilotBuilder`] refuses, though clap has already rejected the
    /// combinations it would refuse for.
    pub fn apply(&self, mut builder: PilotBuilder) -> PilotBuilder {
        if let Some([r, g, b]) = self.channels() {
            builder = builder.rgb(r, g, b);
        }
        if let Some(kelvin) = self.colour.kelvin {
            builder = builder.temp(kelvin);
        }
        if let Some(id) = self.colour.scene {
            builder = builder.scene(id);
            // Sent anyway — the bulb takes it and discards it — but worth
            // saying, because the flag looks like it did something.
            if self.speed.is_some() && !id.takes_speed() {
                let name = id.scene().map_or("that scene", Scene::name);
                tracing::warn!("{name} has no adjustable speed; --speed will be ignored");
            }
        }
        if let Some(speed) = self.speed {
            builder = builder.speed(speed);
        }
        if let Some(brightness) = self.brightness {
            builder = builder.dimming(brightness);
        }
        builder
    }

    /// Refuses an option the bulb has no hardware for, naming the class.
    ///
    /// The bulb will not do this itself. Measured on `ESP25_SHRGB_01` fw
    /// 1.38.0: it accepts the dual-head `ratio` parameter and answers
    /// `success`, having nothing to balance — so a device answering `success`
    /// is not evidence that anything happened, and a dimmable white bulb told
    /// to go red would report exactly the same.
    ///
    /// # Errors
    ///
    /// A message naming the class and what it lacks.
    pub fn check(&self, bulb_type: &BulbType) -> anyhow::Result<()> {
        let features = bulb_type.features;
        let refuse = |what: &str| -> anyhow::Error {
            anyhow::anyhow!("{}: it cannot {what}", describe(bulb_type))
        };

        if self.channels().is_some() && !features.color {
            return Err(refuse("show a colour"));
        }
        if self.colour.kelvin.is_some() && !features.color_tmp {
            return Err(refuse("set a colour temperature"));
        }
        if self.brightness.is_some() && !features.brightness {
            return Err(refuse("be dimmed"));
        }
        if let Some(id) = self.colour.scene {
            if !features.effect {
                return Err(refuse("play scenes"));
            }
            // A user slot holds whatever its owner saved into it, so there is
            // nothing to check it against; a named scene is only playable by
            // the classes the table lists it for.
            if id.as_user_slot().is_none() && !bulb_type.scenes().any(|scene| scene.id() == id) {
                let name = id.scene().map_or_else(
                    || format!("scene {}", id.get()),
                    |scene| format!("`{}`", scene.name()),
                );
                return Err(anyhow::anyhow!(
                    "{}: it does not play {name}",
                    describe(bulb_type)
                ));
            }
        }
        Ok(())
    }
}

/// How an error refers to the device it is complaining about.
fn describe(bulb_type: &BulbType) -> String {
    let class = bulb_type.class;
    match &bulb_type.module_name {
        Some(name) => format!(
            "{name} is a {} bulb ({})",
            class.description(),
            class.as_str()
        ),
        None => format!(
            "this is a {} bulb ({})",
            class.description(),
            class.as_str()
        ),
    }
}

/// `wizlight status <target>` — what the bulb says it is doing.
///
/// # Errors
///
/// Whatever `getPilot` failed with.
pub async fn status(bulb: &Bulb) -> anyhow::Result<Report> {
    let pilot = bulb.get_pilot().await?;
    Ok(Report::new(status_json(&pilot), describe_pilot(&pilot)))
}

/// `wizlight on|off <target>`.
///
/// # Errors
///
/// Whatever the write failed with, or a capability the bulb does not have.
pub async fn power(bulb: &Bulb, on: bool, options: &StateOptions) -> anyhow::Result<Report> {
    if !options.is_empty() {
        options.check(&bulb.bulb_type().await?)?;
    }
    let builder = options.apply(PilotBuilder::new().state(on));
    bulb.set_pilot(&builder).await?;
    Ok(written(on, options))
}

/// `wizlight toggle <target>`.
///
/// # Errors
///
/// As [`power`], plus a bulb that does not report a power state to invert.
pub async fn toggle(bulb: &Bulb) -> anyhow::Result<Report> {
    let pilot = bulb.get_pilot().await?;
    let was_on = pilot
        .state
        .context("the bulb did not report whether it is on, so there is nothing to toggle")?;
    bulb.set_pilot(&PilotBuilder::new().state(!was_on)).await?;
    Ok(written(!was_on, &StateOptions::default()))
}

/// `wizlight set <target>` — the same options as `on`, sent as `setState`.
///
/// This does **not** leave the bulb alone if it was off. Measured on
/// `ESP25_SHRGB_01` fw 1.38.0: `setState` turns the bulb on exactly as
/// `setPilot` does. The command is kept because the method exists and is worth
/// being able to send; the promise that used to go with it is not.
///
/// # Errors
///
/// As [`power`]. An invocation that asked for nothing is caught earlier, as
/// the usage error it is — see [`StateOptions::require_something`].
pub async fn set(bulb: &Bulb, options: &StateOptions) -> anyhow::Result<Report> {
    options.check(&bulb.bulb_type().await?)?;
    bulb.set_state(&options.apply(PilotBuilder::new())).await?;
    Ok(written(true, options))
}

/// What a write reports back.
///
/// The bulb's acknowledgement carries no state, so this describes the request
/// rather than pretending to have read anything: `status` is how you see what
/// the bulb settled on, and on a scene or a temperature that is not what was
/// asked for.
fn written(on: bool, options: &StateOptions) -> Report {
    let mut parts = vec![if on {
        "on".to_owned()
    } else {
        "off".to_owned()
    }];
    if let Some([r, g, b]) = options.channels() {
        parts.push(format!("rgb {},{},{}", r.get(), g.get(), b.get()));
    }
    if let Some(kelvin) = options.colour.kelvin {
        parts.push(format!("{} K", kelvin.get()));
    }
    if let Some(id) = options.colour.scene {
        parts.push(match id.scene() {
            Some(scene) => format!("scene {} ({})", scene.name(), id.get()),
            None => format!("scene {}", id.get()),
        });
    }
    if let Some(speed) = options.speed {
        parts.push(format!("speed {}", speed.get()));
    }
    if let Some(brightness) = options.brightness {
        parts.push(format!("{}%", brightness.get()));
    }

    let mut json = json!({ "state": on });
    if let Some([r, g, b]) = options.channels() {
        json["rgb"] = json!([r.get(), g.get(), b.get()]);
    }
    if let Some(kelvin) = options.colour.kelvin {
        json["temp"] = json!(kelvin.get());
    }
    if let Some(id) = options.colour.scene {
        json["sceneId"] = json!(id.get());
        json["scene"] = id.scene().map_or(Value::Null, |scene| json!(scene.name()));
    }
    if let Some(speed) = options.speed {
        json["speed"] = json!(speed.get());
    }
    if let Some(brightness) = options.brightness {
        json["dimming"] = json!(brightness.get());
    }
    Report::new(json, parts.join("  "))
}

/// The reported state, with the scene named where the id names one.
fn status_json(pilot: &Pilot) -> Value {
    let mut value = serde_json::to_value(pilot).unwrap_or_else(|_| json!({}));
    if let Some(scene) = pilot.scene() {
        value["scene"] = json!(scene.name());
    }
    value
}

/// One line: whether it is on, what it is showing, and how well it hears us.
fn describe_pilot(pilot: &Pilot) -> String {
    let mut parts = vec![match pilot.state {
        Some(true) => "on".to_owned(),
        Some(false) => "off".to_owned(),
        None => "unknown".to_owned(),
    }];

    // Scene first: while one is playing it is what the bulb is doing, and the
    // colour channels it also reports are the current frame of it.
    if let Some(id) = pilot.scene_id.filter(|id| *id != 0) {
        parts.push(match pilot.scene() {
            Some(scene) => format!("scene {} ({id})", scene.name()),
            None => format!("scene {id}"),
        });
    } else if let Some((r, g, b)) = pilot.rgb() {
        parts.push(format!("rgb {r},{g},{b}"));
    }
    if let Some(temp) = pilot.temp {
        parts.push(format!("{temp} K"));
    }
    if let Some(dimming) = pilot.dimming {
        parts.push(format!("{dimming}%"));
    }
    if let Some(speed) = pilot.speed {
        parts.push(format!("speed {speed}"));
    }
    if let Some(rssi) = pilot.rssi {
        parts.push(format!("{rssi} dBm"));
    }
    parts.join("  ")
}

/// Parses `R,G,B`.
fn rgb(input: &str) -> Result<[Channel; 3], String> {
    let [r, g, b] = triple(input, "R,G,B")?;
    let mut channels = [Channel::new(0); 3];
    for (channel, part) in channels.iter_mut().zip([r, g, b]) {
        let value: u8 = part
            .parse()
            .map_err(|_| format!("`{part}` in `{input}` is not a channel value 0-255"))?;
        *channel = Channel::new(value);
    }
    Ok(channels)
}

/// Parses `H,S,V` and converts it, because the protocol has no HSV.
///
/// `V` scales the colour channels; it is not the bulb's own brightness, which
/// is `--brightness` and a separate parameter on the wire.
fn hsv(input: &str) -> Result<[Channel; 3], String> {
    let parts = triple(input, "H,S,V")?;
    let mut numbers = [0.0f64; 3];
    for (slot, part) in numbers.iter_mut().zip(parts) {
        *slot = part
            .parse()
            .map_err(|_| format!("`{part}` in `{input}` is not a number"))?;
    }
    let [h, s, v] = numbers;
    if !(0.0..=360.0).contains(&h) || !(0.0..=100.0).contains(&s) || !(0.0..=100.0).contains(&v) {
        return Err(format!(
            "`{input}`: hue is 0-360, saturation and value are 0-100"
        ));
    }
    let (r, g, b) = hsv_to_rgb(h, s / 100.0, v / 100.0);
    Ok([Channel::new(r), Channel::new(g), Channel::new(b)])
}

/// Three comma-separated fields.
fn triple<'a>(input: &'a str, shape: &str) -> Result<[&'a str; 3], String> {
    let parts: Vec<&str> = input.split(',').map(str::trim).collect();
    match parts.as_slice() {
        [a, b, c] => Ok([a, b, c]),
        _ => Err(format!("`{input}` is not `{shape}`")),
    }
}

/// The standard conversion, with `s` and `v` as fractions.
fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (u8, u8, u8) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h {
        h if h < 60.0 => (c, x, 0.0),
        h if h < 120.0 => (x, c, 0.0),
        h if h < 180.0 => (0.0, c, x),
        h if h < 240.0 => (0.0, x, c),
        h if h < 300.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let byte = |value: f64| ((value + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (byte(r), byte(g), byte(b))
}

/// Parses a Kelvin value against the wire bound.
fn kelvin(input: &str) -> Result<Kelvin, String> {
    let value: u16 = input
        .parse()
        .map_err(|_| format!("`{input}` is not a colour temperature"))?;
    Kelvin::new(value).map_err(|err| err.to_string())
}

fn speed(input: &str) -> Result<Speed, String> {
    let value: u8 = input
        .parse()
        .map_err(|_| format!("`{input}` is not a speed"))?;
    Speed::new(value).map_err(|err| err.to_string())
}

fn brightness(input: &str) -> Result<Dimming, String> {
    let value: u8 = input
        .parse()
        .map_err(|_| format!("`{input}` is not a percentage"))?;
    Dimming::new(value).map_err(|err| err.to_string())
}

/// Parses a scene, by id or by name.
///
/// Both are checked here rather than against the bulb: the id has to be one
/// the hardware will actually play — several it accepts do something else
/// entirely — and the name has to be in the table. What is left for the bulb
/// is whether *its* class plays that scene, which is [`StateOptions::check`].
fn scene(input: &str) -> Result<SceneId, String> {
    if let Ok(id) = input.parse::<u16>() {
        return SceneId::new(id).map_err(|err| err.to_string());
    }
    Scene::from_name(input)
        .map(Scene::id)
        .map_err(|err| err.to_string())
}
