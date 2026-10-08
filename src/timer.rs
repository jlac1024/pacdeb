//! `pacdeb timer`: a systemd user timer that runs `pacdeb timer run` after login and
//! every few hours. Everything lives in the user's own systemd directory, so no
//! root is needed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Context, Result, bail};

const SERVICE: &str = "pacdeb-check.service";
const TIMER: &str = "pacdeb-check.timer";

pub fn run(action: &str) -> Result<()> {
    let dir = unit_dir()?;
    match action {
        "enable" => enable(&dir),
        "disable" => disable(&dir),
        "run" => scheduled(),
        _ => status(&dir),
    }
}

/// ~/.config/systemd/user, or a folder under PACDEB_HOME so test runs stay out of the
/// real one.
fn unit_dir() -> Result<PathBuf> {
    let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(home) = var("PACDEB_HOME") {
        return Ok(std::path::absolute(home.join("systemd-user"))?);
    }
    let config = var("XDG_CONFIG_HOME").filter(|p| p.is_absolute()).or_else(|| var("HOME").map(|h| h.join(".config")));
    match config {
        Some(c) => Ok(c.join("systemd/user")),
        None => bail!("HOME is not set, so pacdeb cannot find the systemd user directory"),
    }
}

/// Runs `systemctl --user <args>`. PACDEB_SYSTEMCTL replaces systemctl; with PACDEB_HOME
/// set and no replacement, nothing is run, so test runs never touch the real session.
fn systemctl(args: &[&str]) -> Result<()> {
    let program = match std::env::var("PACDEB_SYSTEMCTL").ok().filter(|c| !c.is_empty()) {
        Some(p) => p,
        None if std::env::var_os("PACDEB_HOME").is_some_and(|v| !v.is_empty()) => {
            println!("(PACDEB_HOME is set, so 'systemctl --user {}' was not run)", args.join(" "));
            return Ok(());
        }
        None => "systemctl".into(),
    };
    let status = Command::new(&program).arg("--user").args(args).status().context(format!("cannot run {program}"))?;
    if !status.success() {
        bail!("systemctl --user {} failed ({status})", args.join(" "));
    }
    Ok(())
}

/// What the timer runs. With a repository, new versions are built in the background
/// and published there, so the system updater installs them; without one, updates
/// are only reported.
fn scheduled() -> Result<()> {
    let paths = crate::paths::Paths::from_env()?;
    let config = crate::registry::Config::load(&paths.config)?;
    if config.settings.repo.is_some() {
        let opts = crate::update::Options { name: None, file: None, direct: false, no_install: true, notify: true };
        crate::update::update(&opts)
    } else {
        crate::apps::check(None, true)
    }
}

fn units(exe: &Path) -> (String, String) {
    let service = format!(
        "[Unit]\n\
         Description=Check pacdeb apps for updates\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart=\"{}\" timer run\n",
        exe.display()
    );
    let timer = "[Unit]\n\
                 Description=Check pacdeb apps for updates after login and every 6 hours\n\
                 \n\
                 [Timer]\n\
                 OnStartupSec=5min\n\
                 OnUnitActiveSec=6h\n\
                 RandomizedDelaySec=10min\n\
                 \n\
                 [Install]\n\
                 WantedBy=timers.target\n"
        .to_string();
    (service, timer)
}

fn enable(dir: &Path) -> Result<()> {
    let exe = std::env::current_exe().context("finding the pacdeb binary")?;
    let (service, timer) = units(&exe);
    fs::create_dir_all(dir).context(dir.display())?;
    for (name, text) in [(SERVICE, service), (TIMER, timer)] {
        let path = dir.join(name);
        fs::write(&path, text).context(path.display())?;
    }
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", TIMER])?;
    println!("Update checks are on: 5 minutes after login, then every 6 hours.");
    println!("New versions show up as a notification; its Update button runs 'pacdeb update' in a terminal.");
    println!("The timer runs {}; run 'pacdeb timer enable' again if you move it.", exe.display());
    Ok(())
}

fn disable(dir: &Path) -> Result<()> {
    if !dir.join(TIMER).exists() {
        println!("Update checks are not on.");
        return Ok(());
    }
    systemctl(&["disable", "--now", TIMER])?;
    for name in [SERVICE, TIMER] {
        let path = dir.join(name);
        fs::remove_file(&path).context(path.display())?;
    }
    systemctl(&["daemon-reload"])?;
    println!("Update checks are off.");
    Ok(())
}

fn status(dir: &Path) -> Result<()> {
    if !dir.join(TIMER).exists() {
        println!("Update checks are off. Turn them on with 'pacdeb timer enable'.");
        return Ok(());
    }
    println!("Update checks are on ({}).", dir.join(TIMER).display());
    systemctl(&["list-timers", TIMER, "--no-pager"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_units_that_run_check() {
        let (service, timer) = units(Path::new("/home/j/my apps/pacdeb"));
        assert!(service.contains("ExecStart=\"/home/j/my apps/pacdeb\" timer run\n"), "{service}");
        assert!(service.contains("Type=oneshot"));
        for want in ["OnStartupSec=5min", "OnUnitActiveSec=6h", "WantedBy=timers.target"] {
            assert!(timer.contains(want), "{want}");
        }
    }
}
