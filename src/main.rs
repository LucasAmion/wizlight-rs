//! The `wizlight` command-line tool.
//!
//! Everything lives in [`wizlight::cli`] so that argument parsing and output
//! rendering are unit-testable; this binary is only the entry point.

use std::process::ExitCode;

fn main() -> ExitCode {
    wizlight::cli::run()
}
