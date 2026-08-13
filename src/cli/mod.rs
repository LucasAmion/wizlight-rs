//! The `wizlight` command-line interface (feature `cli`, on by default).
//!
//! The CLI lives inside the library rather than in `src/bin/` so that argument
//! parsing and output rendering can be exercised from the test suite.
//! `src/main.rs` is a wrapper around [`run`].

/// Parses the command line, runs the requested command and renders its output.
///
/// This is the whole of the binary: `main` does nothing but call it and let
/// `anyhow` print the error chain.
///
/// # Errors
///
/// Returns an error if the arguments are invalid, no bulb could be reached, or
/// the bulb rejected the request.
pub fn run() -> anyhow::Result<()> {
    todo!("no commands are implemented yet")
}
