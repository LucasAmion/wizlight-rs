//! The `wizlight` command-line tool.
//!
//! Everything lives in [`wizlight::cli`] so that argument parsing and output
//! rendering are unit-testable; this binary is only the entry point.

fn main() -> anyhow::Result<()> {
    wizlight::cli::run()
}
