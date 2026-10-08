// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the analyzer may assume about the Arch system a converted package lands on.
//! Scripts probe for tools and the init system before using them; answering those
//! probes keeps an `exit` behind a probe from hiding the rest of a script.

use super::PathFact;

/// Commands every CachyOS or Arch system has (base, coreutils, util-linux, procps,
/// shadow, systemd, and xdg-utils, which the desktop debs depend on), plus Debian tools
/// pacdeb translates itself, so a script may use them.
const PRESENT: &[&str] = &[
    "sh", "bash", "cat", "cp", "mv", "rm", "ln", "mkdir", "rmdir", "chmod", "chown", "chgrp", "install", "touch", "sed", "grep", "egrep",
    "fgrep", "awk", "cut", "tr", "head", "tail", "sort", "uniq", "find", "xargs", "tee", "id", "whoami", "getent", "groupadd",
    "useradd", "usermod", "userdel", "groupdel", "gpasswd", "systemctl", "systemd-tmpfiles", "udevadm", "pkill", "pgrep", "ps",
    "readlink", "realpath", "dirname", "basename", "stat", "env", "test", "true", "false", "which", "sleep", "uname", "date",
    "xdg-icon-resource", "xdg-desktop-menu", "xdg-mime", "xdg-settings", "xdg-open", "update-desktop-database",
    "update-mime-database", "gtk-update-icon-cache", "glib-compile-schemas", "ldconfig", "setcap", "update-alternatives",
];

/// Debian commands an Arch system does not have.
const ABSENT: &[&str] = &["apt-config", "apt-get", "apt-key", "apt", "update-menus", "install-menu", "update-notifier", "debconf-communicate"];

/// Whether a command exists: Some(true) present, Some(false) absent, None unknown.
pub(super) fn has_tool(name: &str) -> Option<bool> {
    let name = name.rsplit('/').next().unwrap_or(name);
    if PRESENT.contains(&name) {
        Some(true)
    } else if ABSENT.contains(&name) {
        Some(false)
    } else {
        None
    }
}

/// What `command -v <name>` prints: the tool's path, or nothing when it is absent.
pub(super) fn tool_path(name: &str) -> Option<String> {
    match has_tool(name)? {
        true if name.starts_with('/') => Some(name.to_string()),
        true => Some(format!("/usr/bin/{name}")),
        false => Some(String::new()),
    }
}

/// A path the system provides, for tests like `[ -x /usr/bin/pkill ]`. Arch keeps
/// every command in /usr/bin; /bin, /sbin and /usr/sbin lead there.
pub(super) fn system_fact(path: &str) -> Option<PathFact> {
    let name = ["/usr/bin/", "/bin/", "/usr/sbin/", "/sbin/"].iter().find_map(|d| path.strip_prefix(d))?;
    (has_tool(name) == Some(true) && !name.contains('/')).then_some(PathFact::File { exec: true, empty: false })
}

/// Scripts find out which init system runs by looking at process 1. On Arch it is systemd.
pub(super) fn probes_init_system(command: &str) -> bool {
    ["/proc/1/exe", "/proc/1/comm", "ps -p 1", "ps -o comm= 1", "ps -p1", "/sbin/init --version"].iter().any(|p| command.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_the_system() {
        let cases = [
            ("xdg-icon-resource", Some("/usr/bin/xdg-icon-resource")),
            ("/usr/bin/pkill", Some("/usr/bin/pkill")),
            ("apt-config", Some("")),
            ("update-menus", Some("")),
            ("pkcheck", None),
        ];
        for (tool, want) in cases {
            assert_eq!(tool_path(tool).as_deref(), want, "{tool}");
        }
        assert_eq!(system_fact("/usr/bin/pkill"), Some(PathFact::File { exec: true, empty: false }));
        assert_eq!(system_fact("/bin/sed"), Some(PathFact::File { exec: true, empty: false }));
        assert_eq!(system_fact("/usr/bin/frobnicate"), None);
        assert_eq!(system_fact("/opt/App/sed"), None);
        assert!(probes_init_system("ls -al /proc/1/exe | awk -F' ' '{print $NF}'"));
        assert!(!probes_init_system("ls -al /proc/self/exe"));
    }
}
