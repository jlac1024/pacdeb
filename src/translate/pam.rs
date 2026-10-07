//! Rewrites Debian PAM service files for Arch. Debian splits its stack into
//! common-auth, common-account and friends; Arch keeps it in system-auth (from
//! pambase). A Debian `@include common-auth` on Arch fails to load, which breaks
//! logins for that service.

/// Debian include, and the Arch line that does the same job. Debian's common-session
/// carries pam_systemd, which gives the session its runtime dir and user bus; on Arch
/// that lives in system-login. The noninteractive variant leaves pam_systemd out on
/// purpose, so it stays on system-auth.
const INCLUDES: [(&str, &str); 5] = [
    ("common-auth", "auth      include   system-auth"),
    ("common-account", "account   include   system-auth"),
    ("common-password", "password  include   system-auth"),
    ("common-session", "session   include   system-login"),
    ("common-session-noninteractive", "session   include   system-auth"),
];

pub struct Rewrite {
    pub text: String,
    pub changes: Vec<String>,
    /// Includes pacdeb has no Arch line for.
    pub unknown: Vec<String>,
}

/// None when the file needs no change.
pub fn rewrite(text: &str) -> Option<Rewrite> {
    let mut out = String::new();
    let mut changes = Vec::new();
    let mut unknown = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(target) = trimmed.strip_prefix("@include") {
            let target = target.trim();
            match INCLUDES.iter().find(|(deb, _)| *deb == target) {
                Some((deb, arch)) => {
                    changes.push(format!("@include {deb} -> {}", arch.split_whitespace().collect::<Vec<_>>().join(" ")));
                    out.push_str(arch);
                }
                None => {
                    unknown.push(target.to_string());
                    out.push_str(line);
                }
            }
        } else if !trimmed.starts_with('#') && trimmed.contains("pam_selinux.so") {
            // Arch has no SELinux module; PAM logs a "faulty module" error for every
            // session that loads one, even when the line tells it to ignore that.
            changes.push("removed a pam_selinux line (Arch has no SELinux)".to_string());
            continue;
        } else if trimmed.contains("envfile=/etc/default/locale") {
            // Debian keeps the locale in /etc/default/locale, Arch in /etc/locale.conf;
            // both are KEY=value lines that pam_env reads the same way.
            changes.push("pam_env reads /etc/locale.conf instead of /etc/default/locale".to_string());
            out.push_str(&line.replace("envfile=/etc/default/locale", "envfile=/etc/locale.conf"));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if changes.is_empty() && unknown.is_empty() {
        return None;
    }
    Some(Rewrite { text: out, changes, unknown })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_the_chrome_remote_desktop_file() {
        let deb = "\
# comment
@include common-auth
@include common-account
@include common-password
session [success=ok ignore=ignore module_unknown=ignore default=bad] pam_selinux.so close
session required pam_limits.so
@include common-session
session required pam_env.so readenv=1 user_readenv=1 envfile=/etc/default/locale
";
        let r = rewrite(deb).unwrap();
        assert_eq!(
            r.text,
            "\
# comment
auth      include   system-auth
account   include   system-auth
password  include   system-auth
session required pam_limits.so
session   include   system-login
session required pam_env.so readenv=1 user_readenv=1 envfile=/etc/locale.conf
"
        );
        assert_eq!(r.changes.len(), 6);
        assert!(r.unknown.is_empty());
    }

    #[test]
    fn reports_unknown_includes_and_leaves_arch_files_alone() {
        let r = rewrite("@include common-foo\nauth include system-auth\n").unwrap();
        assert_eq!(r.unknown, ["common-foo"]);
        assert!(rewrite("auth include system-auth\naccount include system-auth\n").is_none());
    }
}
