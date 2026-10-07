//! The neutral package description that both backends build from.

use crate::version::{ArchVersion, DebVersion};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Dir,
    Symlink(String),
    /// Points at another node's final path.
    Hardlink(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Final absolute path in the package.
    pub path: String,
    pub kind: NodeKind,
    pub mode: u32,
    pub size: u64,
    /// Path of the entry in the deb's data archive. None for nodes Ferry adds itself,
    /// such as symlinks from update-alternatives or missing parent directories.
    pub source: Option<String>,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.kind == NodeKind::Dir
    }
}

#[derive(Debug, Clone)]
pub struct Package {
    pub name: String,
    pub version: ArchVersion,
    pub deb_version: DebVersion,
    pub arch: String,
    pub description: String,
    pub url: Option<String>,
    pub license: String,
    pub depends: Vec<String>,
    /// (package, reason)
    pub optdepends: Vec<(String, String)>,
    /// Config files pacman should keep on upgrade, relative to / as PKGBUILD wants them.
    pub backup: Vec<String>,
    /// Sorted by path, every parent directory present.
    pub nodes: Vec<Node>,
}
