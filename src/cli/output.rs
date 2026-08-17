use std::io::{self, IsTerminal};

use serde_json::Value;

/// The renderer contract for CLI output.
pub trait OutputRenderer {
    /// Renders the payload to a string.
    fn render(&self, value: &Value) -> String;
}

/// Renderer for the scripted JSON contract.
pub struct JsonRenderer;

impl OutputRenderer for JsonRenderer {
    fn render(&self, value: &Value) -> String {
        serde_json::to_string(value).expect("JSON values always serialise")
    }
}

/// Renderer for human-readable terminal output.
///
/// A string is emitted as-is, which is what the scaffold produces. Anything
/// structured falls back to indented JSON until there are typed payloads worth
/// laying out properly — the commands that will produce them do not exist yet,
/// and a table formatter with nothing to format would only have to be guessed
/// at twice.
pub struct HumanRenderer;

impl OutputRenderer for HumanRenderer {
    fn render(&self, value: &Value) -> String {
        match value {
            Value::String(s) => s.clone(),
            _ => serde_json::to_string_pretty(value)
                .unwrap_or_else(|_| "{\n  \"error\": \"rendering failed\"\n}".to_owned()),
        }
    }
}

/// Renders a JSON value in the stable scripting shape used by the CLI.
pub fn render_json(value: &Value) -> String {
    JsonRenderer.render(value)
}

/// Whether ANSI colour is suppressed regardless of where output is going.
///
/// `--json` is machine-readable by definition, and [`NO_COLOR`] disables
/// colour when it is present and non-empty, whatever its value.
///
/// [`NO_COLOR`]: https://no-color.org/
fn colour_suppressed(json: bool) -> bool {
    json || std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty())
}

/// Whether to colour output written to stderr — logs and errors.
#[must_use]
pub fn colour_on_stderr(json: bool) -> bool {
    !colour_suppressed(json) && io::stderr().is_terminal()
}

/// Whether to colour output written to stdout — command results.
#[must_use]
pub fn colour_on_stdout(json: bool) -> bool {
    !colour_suppressed(json) && io::stdout().is_terminal()
}
