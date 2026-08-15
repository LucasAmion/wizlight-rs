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

/// Returns `true` when the output stream should disable ANSI colour.
pub fn color_disabled() -> bool {
    !io::stdout().is_terminal() || std::env::var_os("NO_COLOR").is_some()
}

/// Renders a JSON value in the stable scripting shape used by the CLI.
pub fn render_json(value: &Value) -> String {
    JsonRenderer.render(value)
}
