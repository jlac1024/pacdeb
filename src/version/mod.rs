//! Version handling: Debian ordering, pacman's vercmp, and the mapping between them.

mod arch;
mod debian;

pub use arch::ArchVersion;
pub use debian::DebVersion;
