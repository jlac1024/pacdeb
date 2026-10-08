// SPDX-License-Identifier: AGPL-3.0-or-later
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    pacdeb::cli::run(&args)
}
