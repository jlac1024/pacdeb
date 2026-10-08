//! The GUI's install step. The command line tool hands pacman the packages in a
//! terminal; the GUI has none, so it shows what will change itself (including
//! installed packages a conflict removes) and then runs pacman through pkexec, whose
//! polkit dialog asks for the password.

use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;

use adw::prelude::*;

use crate::Ctx;
use crate::run;

/// pacman's answer mask for "remove the conflicting package?": the person already
/// agreed to that in the confirmation.
const ANSWER_CONFLICTS: &str = "4";

/// What installing one package file changes.
#[derive(Debug, PartialEq, Eq)]
pub struct Planned {
    pub name: String,
    pub version: String,
    pub installed: Option<String>,
    pub removes: Vec<String>,
}

/// Reads a field from `pacman -Qi`/`-Qip` output.
fn field<'a>(info: &'a str, name: &str) -> Option<&'a str> {
    info.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == name).then(|| v.trim())
    })
}

/// The conflicts in `pacman -Qip` output, without version constraints.
fn conflicts(info: &str) -> Vec<String> {
    match field(info, "Conflicts With") {
        None | Some("None") => Vec::new(),
        Some(v) => v.split_whitespace().map(|c| c.split(['<', '>', '=']).next().unwrap_or(c).to_string()).collect(),
    }
}

/// The installed version of exactly `pkg`. `pacman -Q` also answers for a package that
/// only provides the name (proton-mail-deb for proton-mail), which is not one a
/// conflict would remove.
fn installed_version(pkg: &str) -> Option<String> {
    let out = Command::new("pacman").args(["-Q", "--", pkg]).env("LC_ALL", "C").output().ok()?;
    exact_version(&String::from_utf8_lossy(&out.stdout), pkg)
}

fn exact_version(q_output: &str, pkg: &str) -> Option<String> {
    let mut parts = q_output.split_whitespace();
    (parts.next()? == pkg).then(|| parts.next().map(String::from)).flatten()
}

fn plan(pkgs: &[PathBuf]) -> Result<Vec<Planned>, String> {
    pkgs.iter()
        .map(|p| {
            let out = Command::new("pacman").arg("-Qip").arg(p).env("LC_ALL", "C").output().map_err(|e| format!("cannot run pacman: {e}"))?;
            if !out.status.success() {
                return Err(format!("pacman cannot read {}: {}", p.display(), String::from_utf8_lossy(&out.stderr).trim()));
            }
            let info = String::from_utf8_lossy(&out.stdout);
            let name = field(&info, "Name").unwrap_or_default().to_string();
            let version = field(&info, "Version").unwrap_or_default().to_string();
            let removes = conflicts(&info).into_iter().filter(|c| *c != name && installed_version(c).is_some()).collect();
            Ok(Planned { installed: installed_version(&name), name, version, removes })
        })
        .collect()
}

fn describe(plan: &[Planned]) -> String {
    let mut lines = Vec::new();
    for p in plan {
        match &p.installed {
            Some(old) if *old == p.version => lines.push(format!("{} {} (reinstall)", p.name, p.version)),
            Some(old) => lines.push(format!("{} {old} \u{2192} {}", p.name, p.version)),
            None => lines.push(format!("{} {} (new)", p.name, p.version)),
        }
        for r in &p.removes {
            lines.push(format!("  removes {r}, which it replaces"));
        }
    }
    lines.join("\n")
}

/// The install command: pkexec pacman, or PACDEB_GUI_INSTALL_CMD for testing.
fn command() -> (PathBuf, Vec<String>) {
    let custom = std::env::var("PACDEB_GUI_INSTALL_CMD").ok().filter(|c| !c.trim().is_empty());
    let line = custom.unwrap_or_else(|| format!("pkexec pacman -U --noconfirm --ask {ANSWER_CONFLICTS}"));
    let mut parts = line.split_whitespace().map(String::from);
    (PathBuf::from(parts.next().unwrap_or_default()), parts.collect())
}

/// Asks before installing `pkgs`, then installs them in one pacman call.
pub fn confirm(ctx: &Rc<Ctx>, pkgs: Vec<PathBuf>) {
    let planned = match plan(&pkgs) {
        Ok(p) => p,
        Err(e) => {
            ctx.toast(&e);
            return;
        }
    };
    let heading = match planned.len() {
        1 => "Install 1 package?".to_string(),
        n => format!("Install {n} packages?"),
    };
    let alert = adw::AlertDialog::new(Some(&heading), Some(&describe(&planned)));
    alert.add_response("cancel", "Cancel");
    alert.add_response("install", "Install");
    alert.set_response_appearance("install", adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some("install"));
    alert.set_close_response("cancel");
    let ctx2 = ctx.clone();
    alert.connect_response(None, move |_, response| {
        if response != "install" {
            ctx2.toast("Not installed; the built packages stay in the cache");
            return;
        }
        let (program, mut args) = command();
        args.extend(pkgs.iter().map(|p| p.display().to_string()));
        let ctx3 = ctx2.clone();
        run::logged(&ctx2, "Installing", &program, &args, false, move |dialog, done| {
            ctx3.refresh();
            if done.ok {
                dialog.close();
                ctx3.toast("Installed");
            }
        });
    });
    alert.present(Some(&ctx.window));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pacman_info() {
        let info = "Name            : proton-mail-deb\nVersion         : 1.15.1-1\nConflicts With  : proton-mail  proton-mail-bin>=1.0\nReplaces        : None\n";
        assert_eq!(field(info, "Name"), Some("proton-mail-deb"));
        assert_eq!(field(info, "Version"), Some("1.15.1-1"));
        assert_eq!(conflicts(info), ["proton-mail", "proton-mail-bin"]);
        assert_eq!(conflicts("Conflicts With  : None\n"), Vec::<String>::new());
    }

    #[test]
    fn only_counts_the_package_itself_as_installed() {
        assert_eq!(exact_version("proton-mail-deb 1.15.0-1\n", "proton-mail-deb").as_deref(), Some("1.15.0-1"));
        assert_eq!(exact_version("proton-mail-deb 1.15.0-1\n", "proton-mail"), None);
        assert_eq!(exact_version("", "x"), None);
    }

    #[test]
    fn describes_the_plan() {
        let plan = [
            Planned { name: "a-deb".into(), version: "2.0-1".into(), installed: Some("1.0-1".into()), removes: vec![] },
            Planned { name: "b-deb".into(), version: "1.0-1".into(), installed: None, removes: vec!["b".into()] },
        ];
        assert_eq!(describe(&plan), "a-deb 1.0-1 \u{2192} 2.0-1\nb-deb 1.0-1 (new)\n  removes b, which it replaces");
    }
}
