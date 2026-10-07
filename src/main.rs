mod cli;
mod control;
mod convert;
mod deb;
mod build;
mod error;
mod human;
mod inspect;
mod install;
mod model;
mod paths;
mod relation;
mod translate;
mod version;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    cli::run(&args)
}
