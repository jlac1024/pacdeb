//! Turns the services a deb's scripts enable or start into a note pacman shows after
//! install. Arch packages never enable or start services on their own; the person
//! decides, so Ferry tells them what Debian would have done.

use super::scripts::{Command, Outcome, ServiceStep};
use crate::model::Node;

pub struct Note {
    pub lines: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn note(cmds: &[Command], nodes: &[Node]) -> Note {
    // unit, user, enable, start, start-if-enabled: in first-seen order
    let mut units: Vec<(String, bool, bool, bool, bool)> = Vec::new();
    for c in cmds {
        let Outcome::Service(steps) = &c.outcome else {
            continue;
        };
        for ServiceStep { verb, unit, user } in steps {
            let i = match units.iter().position(|(u, usr, ..)| u == unit && usr == user) {
                Some(i) => i,
                None => {
                    units.push((unit.clone(), *user, false, false, false));
                    units.len() - 1
                }
            };
            match verb.as_str() {
                "enable" => units[i].2 = true,
                "start-if-enabled" => units[i].4 = true,
                _ => units[i].3 = true,
            }
        }
    }

    let mut warnings = Vec::new();
    let mut system = Vec::new();
    let mut user = Vec::new();
    for (unit, is_user, enable, start, start_if_enabled) in units {
        // Debian only starts what it enabled through deb-systemd-invoke.
        let start = start || (start_if_enabled && enable);
        if !enable && !start {
            continue;
        }
        let dir = if is_user { "/usr/lib/systemd/user/" } else { "/usr/lib/systemd/system/" };
        if !nodes.iter().any(|n| n.path == format!("{dir}{unit}")) {
            warnings.push(format!("the scripts enable or start {unit}, which the package does not ship in {dir}"));
            continue;
        }
        let action = match (enable, start) {
            (true, true) => "enable --now",
            (true, false) => "enable",
            _ => "start",
        };
        if is_user {
            user.push(format!("  systemctl --user {action} {unit}"));
        } else {
            system.push(format!("  sudo systemctl {action} {unit}"));
        }
    }

    let mut lines = Vec::new();
    if !system.is_empty() || !user.is_empty() {
        lines.push("On Debian this package turns on its services by itself. Arch leaves that to you.".into());
        lines.push("To do what Debian would have done:".into());
        lines.extend(system);
        if !user.is_empty() {
            lines.push("and, as each user who needs it:".into());
            lines.extend(user);
        }
    }
    Note { lines, warnings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NodeKind, Source};

    fn cmd(steps: &[(&str, &str, bool)]) -> Command {
        Command {
            script: "postinst".into(),
            line: 1,
            text: String::new(),
            outcome: Outcome::Service(
                steps.iter().map(|(v, u, user)| ServiceStep { verb: v.to_string(), unit: u.to_string(), user: *user }).collect(),
            ),
        }
    }

    fn unit(path: &str) -> Node {
        Node { path: path.into(), kind: NodeKind::File, mode: 0o644, size: 1, source: Source::None }
    }

    #[test]
    fn merges_enable_and_start_per_unit() {
        let cmds = [
            cmd(&[("enable", "demo.service", false)]),
            cmd(&[("start", "demo.service", false)]),
            cmd(&[("enable", "demo-env.service", true)]),
            cmd(&[("start", "other.service", false)]),
        ];
        let nodes = [
            unit("/usr/lib/systemd/system/demo.service"),
            unit("/usr/lib/systemd/user/demo-env.service"),
        ];
        let n = note(&cmds, &nodes);
        assert_eq!(
            n.lines,
            [
                "On Debian this package turns on its services by itself. Arch leaves that to you.",
                "To do what Debian would have done:",
                "  sudo systemctl enable --now demo.service",
                "and, as each user who needs it:",
                "  systemctl --user enable demo-env.service",
            ]
        );
        assert_eq!(n.warnings.len(), 1);
        assert!(n.warnings[0].contains("other.service"));
    }

    #[test]
    fn debhelper_start_needs_an_enable() {
        let nodes = [unit("/usr/lib/systemd/system/a.service"), unit("/usr/lib/systemd/system/b.service")];
        let cmds = [
            cmd(&[("start-if-enabled", "a.service", false)]),
            cmd(&[("enable", "b.service", false)]),
            cmd(&[("start-if-enabled", "b.service", false)]),
        ];
        let n = note(&cmds, &nodes);
        assert!(!n.lines.iter().any(|l| l.contains("a.service")), "{:?}", n.lines);
        assert!(n.lines.iter().any(|l| l == "  sudo systemctl enable --now b.service"), "{:?}", n.lines);
    }

    #[test]
    fn no_services_no_note() {
        let n = note(&[], &[]);
        assert!(n.lines.is_empty() && n.warnings.is_empty());
    }
}
