mod clash;
mod cli;
mod control;
mod convert;
mod deb;
mod apps;
mod build;
mod error;
mod help;
mod human;
mod inspect;
mod install;
mod model;
mod net;
mod paths;
mod registry;
mod relation;
mod sources;
mod style;
mod translate;
mod update;
mod version;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    cli::run(&args)
}
