//! Process entry point for the host-only Aurora ST compiler CLI.

use std::io;
use std::process::ExitCode;

use aurora_cli::{Arguments, execute};
use clap::Parser as _;

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    match execute(arguments) {
        Ok(output) => write_output(&mut io::stdout().lock(), &output, ExitCode::SUCCESS),
        Err(error) => write_output(
            &mut io::stderr().lock(),
            error.to_string().as_bytes(),
            ExitCode::FAILURE,
        ),
    }
}

fn write_output(writer: &mut impl io::Write, output: &[u8], status: ExitCode) -> ExitCode {
    if writer.write_all(output).is_err()
        || (!output.ends_with(b"\n") && writer.write_all(b"\n").is_err())
    {
        return ExitCode::FAILURE;
    }
    status
}
