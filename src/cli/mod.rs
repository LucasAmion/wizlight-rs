//! The `wizlight` command-line interface (feature `cli`, on by default).
//!
//! The CLI lives inside the library rather than in `src/bin/` so that argument
//! parsing and output rendering can be exercised from the test suite.
//! `src/main.rs` is a wrapper around [`run`].
//!
//! **No commands are implemented yet.** The binary exists so that the crate's
//! packaging, installation and release path can be exercised ahead of the
//! commands themselves; until they land, [`run`] does nothing but explain that.

/// Parses the command line, runs the requested command and renders its output.
///
/// This is the whole of the binary: `main` does nothing but call it and let
/// `anyhow` print the error chain.
///
/// # Errors
///
/// Currently always fails: no commands are implemented. Once they are, this
/// returns an error if the arguments are invalid, no bulb could be reached, or
/// the bulb rejected the request.
pub fn run() -> anyhow::Result<()> {
    anyhow::bail!(
        "the wizlight CLI has no commands yet — this is a prerelease that ships \
         the library only.\nUse the crate as a dependency for now: \
         https://docs.rs/wizlight\nProgress: https://github.com/LucasAmion/wizlight-rs"
    )
}
