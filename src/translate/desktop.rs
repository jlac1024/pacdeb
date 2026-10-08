// SPDX-License-Identifier: AGPL-3.0-or-later
//! Checks that the app will show up in the launcher with its icon.

use std::collections::{HashMap, HashSet};

use super::pathutil::{basename, is_under};
use crate::model::Node;

const APPLICATIONS: &str = "/usr/share/applications";
const ICON_EXTS: [&str; 4] = ["png", "svg", "svgz", "xpm"];

#[derive(Debug, PartialEq, Eq)]
pub struct DesktopCheck {
    pub path: String,
    pub exec: Option<String>,
    pub icon: Option<String>,
    pub problems: Vec<String>,
}

/// `contents` holds desktop file bytes keyed by their path in the deb (the node's
/// source). `on_system` says whether a program exists outside the package.
pub fn check(
    nodes: &[Node],
    contents: &HashMap<String, Vec<u8>>,
    on_system: impl Fn(&str) -> bool,
) -> (Vec<DesktopCheck>, Vec<String>) {
    let paths: HashSet<&str> = nodes.iter().map(|n| n.path.as_str()).collect();
    let mut checks = Vec::new();
    let mut warnings = Vec::new();
    let mut elsewhere = Vec::new();

    for node in nodes.iter().filter(|n| !n.is_dir() && n.path.ends_with(".desktop")) {
        if !is_under(&node.path, APPLICATIONS) {
            elsewhere.push(node.path.clone());
            continue;
        }
        let Some(bytes) = node.deb_path().and_then(|s| contents.get(s)) else {
            continue;
        };
        let text = String::from_utf8_lossy(bytes);
        let entry = parse(&text);
        let mut problems = Vec::new();

        let program = entry.get("Exec").and_then(|e| exec_program(e));
        match &program {
            None => problems.push("no Exec line".to_string()),
            Some(p) if p.starts_with('/') => {
                if !paths.contains(p.as_str()) && !on_system(p) {
                    problems.push(format!("Exec program {p} is not in the package or on this system"));
                }
            }
            Some(p) => {
                if !paths.contains(format!("/usr/bin/{p}").as_str()) && !on_system(p) {
                    problems.push(format!("Exec program '{p}' is not in /usr/bin in the package or on this system"));
                }
            }
        }

        // Hidden entries (URL and file handlers, autostart helpers) never show in the
        // launcher on purpose, so only their Exec matters.
        let hidden = entry.get("NoDisplay").copied() == Some("true") || entry.get("Hidden").copied() == Some("true");
        let icon = entry.get("Icon").map(|s| s.to_string());
        match &icon {
            _ if hidden => {}
            None => problems.push("no Icon line".to_string()),
            Some(i) if i.starts_with('/') => {
                if !paths.contains(i.as_str()) {
                    problems.push(format!("icon {i} is not in the package"));
                }
            }
            Some(i) => {
                if !has_icon(&paths, i) {
                    problems.push(format!(
                        "icon '{i}' is not in the package under /usr/share/icons or /usr/share/pixmaps; it only shows if the icon theme has it"
                    ));
                }
            }
        }
        for p in &problems {
            warnings.push(format!("{}: {p}", node.path));
        }
        checks.push(DesktopCheck { path: node.path.clone(), exec: program, icon, problems });
    }

    // Command line tools, libraries and services have no launcher entry and need none.
    // Only an app that ships icons or a misplaced .desktop file looks like it wanted one.
    let ships_icons = paths.iter().any(|p| p.starts_with("/usr/share/icons/") || p.starts_with("/usr/share/pixmaps/"));
    if checks.is_empty() {
        if elsewhere.is_empty() && ships_icons {
            warnings.push(format!("no .desktop file in {APPLICATIONS}, so the app will not show up in the launcher"));
        } else if !elsewhere.is_empty() {
            warnings.push(format!(
                "no .desktop file in {APPLICATIONS}, only {}; the app will not show up in the launcher",
                elsewhere.join(", ")
            ));
        }
    }
    (checks, warnings)
}

/// Keys of the [Desktop Entry] group. Localized keys like Name[de] are skipped.
fn parse(text: &str) -> HashMap<&str, &str> {
    let mut in_entry = false;
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            if !k.contains('[') {
                out.entry(k).or_insert(v.trim());
            }
        }
    }
    out
}

/// The program an Exec line starts, skipping `env VAR=value` prefixes.
fn exec_program(exec: &str) -> Option<String> {
    let mut words = Vec::new();
    let mut chars = exec.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut w = String::new();
        if c == '"' {
            chars.next();
            while let Some(c) = chars.next() {
                match c {
                    '"' => break,
                    '\\' => w.extend(chars.next()),
                    c => w.push(c),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                w.push(c);
                chars.next();
            }
        }
        words.push(w);
    }
    let mut iter = words.into_iter().peekable();
    if iter.peek().is_some_and(|w| w == "env") {
        iter.next();
        while iter.peek().is_some_and(|w| w.contains('=') || w.starts_with('-')) {
            iter.next();
        }
    }
    iter.next()
}

fn has_icon(paths: &HashSet<&str>, name: &str) -> bool {
    let stem = ICON_EXTS
        .iter()
        .find_map(|ext| name.strip_suffix(&format!(".{ext}")))
        .unwrap_or(name);
    paths.iter().any(|p| {
        let in_icon_dir = p.starts_with("/usr/share/icons/") || p.starts_with("/usr/share/pixmaps/");
        in_icon_dir
            && basename(p)
                .rsplit_once('.')
                .is_some_and(|(s, ext)| s == stem && ICON_EXTS.contains(&ext))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NodeKind, Source};

    fn node(path: &str) -> Node {
        Node { path: path.into(), kind: NodeKind::File, mode: 0o644, size: 0, source: Source::Deb(path.into()) }
    }

    fn run(files: &[(&str, &str)], extra: &[&str]) -> (Vec<DesktopCheck>, Vec<String>) {
        let mut nodes: Vec<Node> = files.iter().map(|(p, _)| node(p)).collect();
        nodes.extend(extra.iter().map(|p| node(p)));
        let contents = files.iter().map(|(p, c)| (p.to_string(), c.as_bytes().to_vec())).collect();
        check(&nodes, &contents, |p| p == "xdg-open" || p == "/usr/bin/env")
    }

    #[test]
    fn accepts_a_good_entry() {
        let (checks, warnings) = run(
            &[("/usr/share/applications/app.desktop", "[Desktop Entry]\nName=App\nName[de]=X\nExec=app %U\nIcon=app\nType=Application\n")],
            &["/usr/bin/app", "/usr/share/pixmaps/app.png"],
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(checks[0].exec.as_deref(), Some("app"));
        assert_eq!(checks[0].icon.as_deref(), Some("app"));
    }

    #[test]
    fn finds_problems() {
        let cases: &[(&str, &[&str], &str)] = &[
            ("Exec=missing\nIcon=app", &["/usr/share/pixmaps/app.png"], "Exec program 'missing'"),
            ("Exec=\"/opt/My App/run\" %U\nIcon=app", &["/usr/share/icons/hicolor/256x256/apps/app.png"], "Exec program /opt/My App/run"),
            ("Exec=app\nIcon=nothere", &["/usr/bin/app"], "icon 'nothere' is not in the package"),
            ("Exec=app\nIcon=/opt/app.png", &["/usr/bin/app"], "icon /opt/app.png is not in the package"),
            ("Exec=missing\nNoDisplay=true", &[], "Exec program 'missing'"),
            ("Icon=app", &["/usr/share/pixmaps/app.png"], "no Exec line"),
        ];
        for (body, extra, want) in cases {
            let text = format!("[Desktop Entry]\n{body}\n");
            let (_, warnings) = run(&[("/usr/share/applications/a.desktop", &text)], extra);
            assert!(warnings.iter().any(|w| w.contains(want)), "{body:?}: {warnings:?}");
        }
    }

    #[test]
    fn accepts_system_programs_and_icon_names_with_extensions() {
        let text = "[Desktop Entry]\nExec=env FOO=1 xdg-open http://x\nIcon=app.png\n[Desktop Action x]\nExec=nothing\n";
        let (checks, warnings) = run(&[("/usr/share/applications/a.desktop", text)], &["/usr/share/icons/hicolor/48x48/apps/app.png"]);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(checks[0].exec.as_deref(), Some("xdg-open"));
    }

    #[test]
    fn hidden_entries_only_need_a_working_exec() {
        let text = "[Desktop Entry]\nExec=/opt/App/open-url %u\nNoDisplay=true\nMimeType=x-scheme-handler/app;\n";
        let (_, warnings) = run(&[("/usr/share/applications/handler.desktop", text)], &["/opt/App/open-url"]);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn warns_without_a_launcher_entry_only_for_apps() {
        let (_, warnings) = run(&[("/opt/App/app.desktop", "[Desktop Entry]\nExec=x\n")], &[]);
        assert!(warnings[0].contains("only /opt/App/app.desktop"), "{warnings:?}");
        let (_, warnings) = run(&[], &["/usr/bin/app", "/usr/share/icons/hicolor/48x48/apps/app.png"]);
        assert!(warnings[0].contains("no .desktop file"), "{warnings:?}");
        // A command line tool or service has nothing to put in the launcher.
        let (_, warnings) = run(&[], &["/usr/bin/tool", "/usr/lib/systemd/system/tool.service"]);
        assert!(warnings.is_empty(), "{warnings:?}");
    }
}
