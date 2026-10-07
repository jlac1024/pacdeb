mod cli;
mod control;
mod deb;
mod error;
mod inspect;
mod relation;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    cli::run(&args)
}
