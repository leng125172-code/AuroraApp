//! Host-only build and verification entry point.

mod commands;
mod digest;
mod error;
mod schema;
mod supply_chain;

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser;

use crate::commands::Arguments;

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    match commands::execute(arguments.command) {
        Ok(message) => {
            let mut stdout = io::stdout().lock();
            if writeln!(stdout, "{message}").is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            let mut stderr = io::stderr().lock();
            let _write_result = writeln!(stderr, "error: {error}");
            ExitCode::FAILURE
        }
    }
}
