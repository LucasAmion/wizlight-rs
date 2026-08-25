//! The commands that ask a bulb what it is, rather than what it is doing.

use serde_json::{Value, json};

use super::Report;
use crate::Bulb;
use crate::protocol::{BulbType, Scene};

/// `wizlight info <target>` — model, firmware and what the thing can do.
///
/// # Errors
///
/// [`Error::UnknownModel`](crate::Error::UnknownModel) if the bulb cannot
/// describe itself, and otherwise whatever the config reads failed with.
pub async fn info(bulb: &Bulb) -> anyhow::Result<Report> {
    let bulb_type = bulb.bulb_type().await?;
    let scenes = bulb_type.scenes().count();
    Ok(Report::new(
        json(&bulb_type, scenes),
        human(&bulb_type, scenes),
    ))
}

/// `wizlight scenes <target>` — only what this bulb will actually play.
///
/// The table is per class, so a tunable white bulb is not offered Party. That
/// is the point of asking the bulb rather than printing the whole table.
///
/// # Errors
///
/// As [`info`].
pub async fn scenes(bulb: &Bulb) -> anyhow::Result<Report> {
    let scenes = bulb.scenes().await?;
    let listing = scenes
        .iter()
        .map(|scene| format!("{:>3}  {}", scene.id().get(), scene.name()))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Report::new(
        Value::Array(scenes.iter().map(scene_json).collect()),
        listing,
    ))
}

fn scene_json(scene: &Scene) -> Value {
    json!({
        "id": scene.id().get(),
        "name": scene.name(),
        "category": scene.category(),
        // Two different questions, and conflating them is what every
        // published table does: Wake up animates and takes no speed.
        "animates": scene.animates(),
        "adjustable": scene.adjustable(),
    })
}

/// [`BulbType`] serialises itself; the scene count is the one thing worth
/// adding, since the list is a separate command.
fn json(bulb_type: &BulbType, scenes: usize) -> Value {
    let mut value = serde_json::to_value(bulb_type).unwrap_or_else(|_| json!({}));
    value["description"] = json!(bulb_type.class.description());
    value["scenes"] = json!(scenes);
    value
}

fn human(bulb_type: &BulbType, scenes: usize) -> String {
    let mut lines = vec![
        row(
            "model",
            bulb_type
                .module_name
                .as_ref()
                .map_or_else(|| "unknown".to_owned(), ToString::to_string),
        ),
        row(
            "firmware",
            bulb_type
                .fw_version
                .clone()
                .unwrap_or_else(|| "-".to_owned()),
        ),
        row(
            "class",
            format!(
                "{} ({})",
                bulb_type.class.description(),
                bulb_type.class.as_str()
            ),
        ),
    ];

    if let Some(range) = bulb_type.kelvin_range {
        lines.push(row("kelvin", format!("{}-{} K", range.min(), range.max())));
    }

    let features = bulb_type.features;
    let can: Vec<&str> = [
        ("colour", features.color),
        ("tunable white", features.color_tmp),
        ("scenes", features.effect),
        ("dimming", features.brightness),
        ("dual head", features.dual_head),
        ("fan", features.fan),
    ]
    .into_iter()
    .filter_map(|(name, yes)| yes.then_some(name))
    .collect();
    lines.push(row(
        "can",
        if can.is_empty() {
            "on and off only".to_owned()
        } else {
            can.join(", ")
        },
    ));
    lines.push(row("scenes", scenes.to_string()));

    // How the class was arrived at, because the three are not equally
    // trustworthy: a module name describes the device, while a fallback is a
    // guess that happens to be right for most of them.
    lines.push(row("derived from", format!("{:?}", bulb_type.derivation)));
    lines.join("\n")
}

fn row(label: &str, value: String) -> String {
    format!("{label:<13}{value}")
}
