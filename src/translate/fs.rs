//! Filesystem fixes: merged /usr, the multiarch lib dir, apt leftovers, link targets,
//! ownership and modes.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::pathutil::{basename, is_under, parent, relative, replace_prefix, resolve};
use crate::deb::{DataEntry, EntryKind};
use crate::model::{Node, NodeKind};

/// Arch's filesystem package makes these symlinks into /usr, so packages must not ship them.
const MERGED_USR: [(&str, &str); 6] = [
    ("/bin", "/usr/bin"),
    ("/sbin", "/usr/bin"),
    ("/usr/sbin", "/usr/bin"),
    ("/lib64", "/usr/lib"),
    ("/lib", "/usr/lib"),
    ("/usr/lib64", "/usr/lib"),
];

const CRON_DIRS: [&str; 5] = [
    "/etc/cron.d",
    "/etc/cron.daily",
    "/etc/cron.hourly",
    "/etc/cron.weekly",
    "/etc/cron.monthly",
];

pub fn triplet_dir(arch: &str) -> Option<&'static str> {
    match arch {
        "x86_64" => Some("/usr/lib/x86_64-linux-gnu"),
        "aarch64" => Some("/usr/lib/aarch64-linux-gnu"),
        _ => None,
    }
}

/// Old path to new path for everything the fixes moved.
#[derive(Debug, Default)]
pub struct PathMap {
    moved: BTreeMap<String, String>,
    triplet: Option<&'static str>,
}

impl PathMap {
    /// Where `path` ends up. Paths inside the package follow the actual moves; other
    /// paths (system files a script or link refers to) get the general rewrite rules.
    pub fn apply(&self, path: &str) -> String {
        if let Some(new) = self.moved.get(path) {
            return new.clone();
        }
        let mut dir = path;
        while dir != "/" {
            dir = parent(dir);
            if let Some(new) = self.moved.get(dir) {
                return format!("{new}{}", &path[dir.len()..]);
            }
        }
        system_path(path, self.triplet)
    }
}

/// The merged-usr and multiarch rewrite on its own.
fn system_path(path: &str, triplet: Option<&str>) -> String {
    let merged = MERGED_USR
        .iter()
        .find_map(|(from, to)| replace_prefix(path, from, to))
        .unwrap_or_else(|| path.to_string());
    triplet
        .and_then(|t| replace_prefix(&merged, t, "/usr/lib"))
        .unwrap_or(merged)
}

#[derive(Debug, Default)]
pub struct FsResult {
    pub nodes: Vec<Node>,
    pub map: PathMap,
    pub changes: Vec<String>,
    pub warnings: Vec<String>,
}

/// `contents` holds the bytes of small files the fixes need to look at (cron jobs),
/// keyed by their path in the deb.
pub fn apply(entries: &[DataEntry], arch: &str, contents: &HashMap<String, Vec<u8>>) -> FsResult {
    let triplet = triplet_dir(arch);
    let mut out = FsResult {
        map: PathMap { moved: BTreeMap::new(), triplet },
        ..FsResult::default()
    };

    let mut removed = Vec::new();
    let mut kept: Vec<&DataEntry> = Vec::new();
    for e in entries {
        if is_under(&e.path, "/etc/apt") {
            if e.kind != EntryKind::Dir {
                out.changes.push(format!("removed apt file {}", e.path));
            }
            removed.push(e.path.clone());
        } else if is_apt_cron(e, contents) {
            out.changes.push(format!("removed cron job {} (it manages apt repositories)", e.path));
            removed.push(e.path.clone());
        } else if let EntryKind::Other(t) = e.kind {
            out.warnings.push(format!("{}: dropped, special file type '{}' can not be packaged", e.path, t as char));
            removed.push(e.path.clone());
        } else {
            kept.push(e);
        }
    }

    // Where every entry wants to go. Entries that stay put claim their paths first so a
    // moved file never pushes out one that was already in the right place.
    let targets: Vec<(String, String)> = kept
        .iter()
        .map(|e| {
            let merged = MERGED_USR
                .iter()
                .find_map(|(from, to)| replace_prefix(&e.path, from, to))
                .unwrap_or_else(|| e.path.clone());
            let full = triplet
                .and_then(|t| replace_prefix(&merged, t, "/usr/lib"))
                .unwrap_or_else(|| merged.clone());
            (merged, full)
        })
        .collect();
    let mut taken: HashMap<String, bool> = kept
        .iter()
        .zip(&targets)
        .filter(|(e, (_, full))| e.path == *full)
        .map(|(e, _)| (e.path.clone(), e.kind == EntryKind::Dir))
        .collect();

    let mut moved_from: BTreeMap<&str, usize> = BTreeMap::new();
    for (e, (merged, full)) in kept.iter().zip(&targets) {
        let is_dir = e.kind == EntryKind::Dir;
        let mut dest = full.clone();
        if dest != e.path {
            match taken.get(&dest) {
                // Two directories landing on the same path just merge.
                Some(true) if is_dir => {
                    out.map.moved.insert(e.path.clone(), dest);
                    continue;
                }
                Some(_) => {
                    // The multiarch move is optional, so a clash leaves the file where
                    // merged usr put it. The merged usr move is not optional.
                    if merged != full && !taken.contains_key(merged) {
                        out.warnings.push(format!(
                            "{}: kept in {} because {dest} is already in the package",
                            e.path,
                            triplet.unwrap_or_default()
                        ));
                        dest = merged.clone();
                        // Its directory is mapped away, so pin this path or links to it
                        // would follow the directory.
                        out.map.moved.insert(e.path.clone(), dest.clone());
                    } else {
                        out.warnings.push(format!("{}: dropped, {dest} is already in the package", e.path));
                        continue;
                    }
                }
                None => {}
            }
            if dest != e.path {
                let root = MERGED_USR
                    .iter()
                    .map(|(from, _)| *from)
                    .chain(triplet)
                    .find(|from| is_under(&e.path, from))
                    .unwrap_or("/");
                *moved_from.entry(root).or_default() += 1;
                out.map.moved.insert(e.path.clone(), dest.clone());
            }
            taken.insert(dest.clone(), is_dir);
        }
        out.nodes.push(Node {
            kind: match e.kind {
                EntryKind::Dir => NodeKind::Dir,
                EntryKind::Symlink => NodeKind::Symlink(e.link.clone().unwrap_or_default()),
                EntryKind::Hardlink => NodeKind::Hardlink(e.link.clone().unwrap_or_default()),
                _ => NodeKind::File,
            },
            path: dest,
            mode: e.mode,
            size: e.size,
            source: Some(e.path.clone()),
        });
    }
    for (root, n) in moved_from {
        let to = system_path(root, triplet);
        out.changes.push(format!("moved {n} {} from {root} into {to}", if n == 1 { "entry" } else { "entries" }));
    }

    fix_links(&mut out);
    let removed_set: HashSet<&str> = removed.iter().map(String::as_str).collect();
    prune_dirs(&mut out.nodes, &removed_set);

    if let Some(e) = entries.iter().find(|e| e.uid != 0 || e.gid != 0) {
        out.changes.push(format!("made every file owned by root (the deb had other owners, such as {})", e.path));
    }
    out
}

fn is_apt_cron(e: &DataEntry, contents: &HashMap<String, Vec<u8>>) -> bool {
    e.kind == EntryKind::File
        && CRON_DIRS.iter().any(|d| is_under(&e.path, d) && e.path != *d)
        && contents
            .get(&e.path)
            .is_some_and(|c| mentions_apt(&String::from_utf8_lossy(c)))
}

pub fn mentions_apt(text: &str) -> bool {
    ["/etc/apt", "apt-key", "sources.list", "apt-get", "apt.conf", "trusted.gpg"]
        .iter()
        .any(|p| text.contains(p))
}

/// Keeps link targets pointing at the same thing after the moves. Relative links stay
/// relative, absolute ones stay absolute.
fn fix_links(out: &mut FsResult) {
    for node in &mut out.nodes {
        let old_path = node.source.clone().unwrap_or_else(|| node.path.clone());
        match &mut node.kind {
            NodeKind::Symlink(target) => {
                let old_abs = resolve(parent(&old_path), target);
                let new_abs = out.map.apply(&old_abs);
                if new_abs == old_abs && node.path == old_path {
                    continue;
                }
                let new_target = if target.starts_with('/') {
                    new_abs
                } else {
                    relative(parent(&node.path), &new_abs)
                };
                if new_target != *target {
                    out.changes.push(format!("{}: link now points to {new_target}", node.path));
                    *target = new_target;
                }
            }
            NodeKind::Hardlink(target) => {
                *target = out.map.apply(target);
            }
            _ => {}
        }
    }
}

/// Drops directories that only held things the fixes removed.
fn prune_dirs(nodes: &mut Vec<Node>, removed: &HashSet<&str>) {
    let mut candidates: Vec<String> = removed.iter().map(|p| parent(p).to_string()).collect();
    while let Some(dir) = candidates.pop() {
        if dir == "/" || removed.contains(dir.as_str()) {
            continue;
        }
        let Some(i) = nodes.iter().position(|n| n.is_dir() && n.path == dir) else {
            continue;
        };
        if nodes.iter().any(|n| n.path != dir && is_under(&n.path, &dir)) {
            continue;
        }
        nodes.remove(i);
        candidates.push(parent(&dir).to_string());
    }
}

/// Strips group and other write permission, and gives chrome-sandbox the setuid bit
/// Electron needs. Returns change notes.
pub fn fix_modes(nodes: &mut [Node]) -> Vec<String> {
    let mut changes = Vec::new();
    let mut writable = Vec::new();
    for node in nodes.iter_mut() {
        if matches!(node.kind, NodeKind::File | NodeKind::Dir) && node.mode & 0o022 != 0 {
            node.mode &= !0o022;
            writable.push(node.path.clone());
        }
        if node.kind == NodeKind::File && basename(&node.path) == "chrome-sandbox" && node.mode != 0o4755 {
            changes.push(format!("{}: mode {:04o} -> 4755 so the Electron sandbox works", node.path, node.mode));
            node.mode = 0o4755;
        }
    }
    if !writable.is_empty() {
        let example = &writable[0];
        let more = if writable.len() > 1 { format!(" and {} more", writable.len() - 1) } else { String::new() };
        changes.push(format!("removed group/other write permission from {example}{more}"));
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, kind: EntryKind, mode: u32, link: Option<&str>) -> DataEntry {
        DataEntry { path: path.into(), kind, mode, size: 1, uid: 0, gid: 0, link: link.map(String::from) }
    }
    fn file(path: &str) -> DataEntry {
        entry(path, EntryKind::File, 0o644, None)
    }
    fn dir(path: &str) -> DataEntry {
        entry(path, EntryKind::Dir, 0o755, None)
    }
    fn link(path: &str, target: &str) -> DataEntry {
        entry(path, EntryKind::Symlink, 0o777, Some(target))
    }
    fn paths(r: &FsResult) -> Vec<String> {
        r.nodes.iter().map(|n| n.path.clone()).collect()
    }

    #[test]
    fn moves_legacy_dirs_into_usr() {
        let entries = [
            dir("/bin"),
            file("/bin/tool"),
            file("/sbin/daemon"),
            file("/usr/sbin/admin"),
            dir("/lib"),
            dir("/lib/udev"),
            file("/lib/udev/rules.d/70-x.rules"),
            file("/lib64/ld.so"),
            dir("/usr"),
            dir("/usr/bin"),
            file("/usr/bin/app"),
        ];
        let r = apply(&entries, "x86_64", &HashMap::new());
        assert_eq!(
            paths(&r),
            [
                "/usr/bin/tool",
                "/usr/bin/daemon",
                "/usr/bin/admin",
                "/usr/lib",
                "/usr/lib/udev",
                "/usr/lib/udev/rules.d/70-x.rules",
                "/usr/lib/ld.so",
                "/usr",
                "/usr/bin",
                "/usr/bin/app",
            ]
        );
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
        assert!(r.changes.contains(&"moved 1 entry from /bin into /usr/bin".to_string()), "{:?}", r.changes);
        for n in &r.nodes {
            for (legacy, _) in MERGED_USR {
                assert!(!is_under(&n.path, legacy), "{} still under {legacy}", n.path);
            }
        }
    }

    #[test]
    fn multiarch_moves_unless_it_collides() {
        let entries = [
            dir("/usr/lib"),
            file("/usr/lib/libdup.so.1"),
            dir("/usr/lib/x86_64-linux-gnu"),
            file("/usr/lib/x86_64-linux-gnu/libfoo.so.1"),
            file("/usr/lib/x86_64-linux-gnu/libdup.so.1"),
            file("/lib/x86_64-linux-gnu/libbar.so.2"),
        ];
        let r = apply(&entries, "x86_64", &HashMap::new());
        assert_eq!(
            paths(&r),
            [
                "/usr/lib",
                "/usr/lib/libdup.so.1",
                "/usr/lib/libfoo.so.1",
                "/usr/lib/x86_64-linux-gnu/libdup.so.1",
                "/usr/lib/libbar.so.2",
            ]
        );
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("kept in /usr/lib/x86_64-linux-gnu"));
        assert_eq!(r.map.apply("/usr/lib/x86_64-linux-gnu/libdup.so.1"), "/usr/lib/x86_64-linux-gnu/libdup.so.1");
        assert_eq!(r.map.apply("/usr/lib/x86_64-linux-gnu/libfoo.so.1"), "/usr/lib/libfoo.so.1");

        // No multiarch move for an arch-independent package.
        let r = apply(&[file("/usr/lib/x86_64-linux-gnu/x")], "any", &HashMap::new());
        assert_eq!(paths(&r), ["/usr/lib/x86_64-linux-gnu/x"]);
    }

    #[test]
    fn drops_merged_usr_collisions() {
        let r = apply(&[file("/usr/bin/tool"), file("/bin/tool")], "x86_64", &HashMap::new());
        assert_eq!(paths(&r), ["/usr/bin/tool"]);
        assert!(r.warnings[0].contains("/bin/tool: dropped"), "{:?}", r.warnings);
    }

    #[test]
    fn strips_apt_files_and_apt_cron_jobs() {
        let contents = HashMap::from([
            ("/etc/cron.daily/app".to_string(), b"#!/bin/sh\napt-key add ...\n".to_vec()),
            ("/etc/cron.daily/cleanup".to_string(), b"#!/bin/sh\nrm -rf /tmp/app\n".to_vec()),
        ]);
        let entries = [
            dir("/etc"),
            dir("/etc/apt"),
            dir("/etc/apt/sources.list.d"),
            file("/etc/apt/sources.list.d/app.list"),
            dir("/etc/cron.daily"),
            file("/etc/cron.daily/app"),
            dir("/etc/cron.weekly"),
            file("/etc/cron.weekly/app"),
            file("/etc/cron.daily/cleanup"),
            dir("/etc/default"),
            file("/etc/default/app"),
        ];
        let r = apply(&entries, "x86_64", &contents);
        assert_eq!(
            paths(&r),
            ["/etc", "/etc/cron.daily", "/etc/cron.weekly", "/etc/cron.weekly/app", "/etc/cron.daily/cleanup", "/etc/default", "/etc/default/app"]
        );
        assert!(r.changes.iter().any(|c| c.contains("removed apt file /etc/apt/sources.list.d/app.list")));
        assert!(r.changes.iter().any(|c| c.contains("removed cron job /etc/cron.daily/app")));
    }

    #[test]
    fn prunes_dirs_left_empty() {
        let contents = HashMap::from([("/etc/cron.daily/app".to_string(), b"apt-get update".to_vec())]);
        let entries = [dir("/etc"), dir("/etc/cron.daily"), file("/etc/cron.daily/app"), dir("/etc/apt"), file("/etc/apt/x")];
        let r = apply(&entries, "x86_64", &contents);
        assert!(r.nodes.is_empty(), "{:?}", paths(&r));
    }

    #[test]
    fn keeps_links_pointing_at_the_same_thing() {
        let entries = [
            file("/lib/x86_64-linux-gnu/libfoo.so.1.2"),
            link("/lib/x86_64-linux-gnu/libfoo.so.1", "libfoo.so.1.2"),
            link("/usr/bin/run", "../../lib/x86_64-linux-gnu/libfoo.so.1"),
            link("/usr/bin/abs", "/lib/x86_64-linux-gnu/libfoo.so.1.2"),
            link("/usr/bin/sys", "/usr/lib/x86_64-linux-gnu/libc.so.6"),
            link("/usr/bin/app", "../lib/app/app"),
            link("/sbin/tool", "../usr/lib/app/tool"),
        ];
        let r = apply(&entries, "x86_64", &HashMap::new());
        let links: Vec<_> = r
            .nodes
            .iter()
            .filter_map(|n| match &n.kind {
                NodeKind::Symlink(t) => Some((n.path.as_str(), t.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            links,
            [
                ("/usr/lib/libfoo.so.1", "libfoo.so.1.2"),
                ("/usr/bin/run", "../lib/libfoo.so.1"),
                ("/usr/bin/abs", "/usr/lib/libfoo.so.1.2"),
                ("/usr/bin/sys", "/usr/lib/libc.so.6"),
                ("/usr/bin/app", "../lib/app/app"),
                ("/usr/bin/tool", "../lib/app/tool"),
            ]
        );
    }

    #[test]
    fn fixes_modes() {
        let mut nodes: Vec<Node> = [
            ("/usr/lib/app/chrome-sandbox", NodeKind::File, 0o755),
            ("/usr/lib/app/resources/icon.png", NodeKind::File, 0o666),
            ("/usr/lib/app/open", NodeKind::Dir, 0o777),
            ("/usr/lib/app/fine", NodeKind::File, 0o644),
            ("/usr/bin/app", NodeKind::Symlink("x".into()), 0o777),
        ]
        .into_iter()
        .map(|(path, kind, mode)| Node { path: path.into(), kind, mode, size: 0, source: None })
        .collect();
        let changes = fix_modes(&mut nodes);
        let modes: Vec<u32> = nodes.iter().map(|n| n.mode).collect();
        assert_eq!(modes, [0o4755, 0o644, 0o755, 0o644, 0o777]);
        assert_eq!(changes.len(), 2, "{changes:?}");
        assert!(changes[1].contains("icon.png and 1 more"), "{changes:?}");
    }

    #[test]
    fn notes_non_root_owners() {
        let mut e = file("/usr/bin/x");
        e.uid = 1000;
        let r = apply(&[e], "x86_64", &HashMap::new());
        assert!(r.changes.iter().any(|c| c.contains("owned by root")), "{:?}", r.changes);
    }
}
