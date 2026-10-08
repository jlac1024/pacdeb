//! pacdeb's core, shared by the pacdeb command line tool and pacdeb-gui.

/// The version and the commit it was built from, like "0.2.0 (133ec0e, 2026-10-07)".
pub fn version() -> String {
    format!("{} ({})", env!("CARGO_PKG_VERSION"), build())
}

/// The commit and date the binary was built from, with "modified" for uncommitted changes.
pub fn build() -> &'static str {
    env!("PACDEB_BUILD")
}

pub mod apps;
pub mod aptline;
pub mod aptrepos;
pub mod browse;
pub mod build;
pub mod clash;
pub mod completions;
pub mod cli;
pub mod control;
pub mod convert;
pub mod deb;
pub mod error;
pub mod help;
pub mod human;
pub mod inspect;
pub mod install;
pub mod model;
pub mod net;
pub mod notify;
pub mod paths;
pub mod progress;
pub mod registry;
pub mod relation;
pub mod repo;
pub mod sources;
pub mod style;
pub mod timer;
pub mod translate;
pub mod update;
pub mod version;
