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

/// Where a node's content comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// An entry in the deb's data archive, by its path there.
    Deb(String),
    /// Bytes a maintainer script would have written, such as a heredoc.
    Inline(Vec<u8>),
    /// Nothing to copy: directories and symlinks Ferry adds itself.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Final absolute path in the package.
    pub path: String,
    pub kind: NodeKind,
    pub mode: u32,
    pub size: u64,
    pub source: Source,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.kind == NodeKind::Dir
    }

    pub fn deb_path(&self) -> Option<&str> {
        match &self.source {
            Source::Deb(p) => Some(p),
            _ => None,
        }
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
    pub provides: Vec<String>,
    pub conflicts: Vec<String>,
    /// Config files pacman should keep on upgrade, relative to / as PKGBUILD wants them.
    pub backup: Vec<String>,
    /// Text pacman shows after a fresh install, such as which services to enable.
    pub install_note: Vec<String>,
    /// Sorted by path, every parent directory present.
    pub nodes: Vec<Node>,
}
