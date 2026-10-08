//! Desktop notifications for the timer. Without a repository it reports updates found,
//! with an Update button that opens a terminal running `pacdeb update`, since
//! installing needs the sudo prompt. With one it reports builds ready to install.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Context, Result, bail};
use crate::paths::Paths;

/// Terminals to try, in order, with the arguments that come before the command.
const TERMINALS: &[(&str, &[&str])] = &[
    ("xdg-terminal-exec", &[]),
    ("konsole", &["-e"]),
    ("gnome-terminal", &["--"]),
    ("ptyxis", &["--"]),
    ("kgx", &["--"]),
    ("alacritty", &["-e"]),
    ("kitty", &[]),
    ("foot", &[]),
    ("wezterm", &["start", "--"]),
    ("xfce4-terminal", &["-x"]),
    ("xterm", &["-e"]),
];

/// Notifies about `pending` updates unless they are the ones notified about last time.
/// No updates clears the memory, so the next one is announced again.
pub fn updates(pending: &[String], paths: &Paths) -> Result<()> {
    notify_with(&program(), pending, paths)
}

fn program() -> String {
    std::env::var("PACDEB_NOTIFY_CMD").ok().filter(|c| !c.is_empty()).unwrap_or_else(|| "notify-send".into())
}

/// Says that new versions were built and are ready to install. Each build happens
/// once, so there is nothing to remember between runs.
pub fn built(lines: &[String], ready: &str) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let title = match lines.len() {
        1 => "pacdeb: 1 update ready".to_string(),
        n => format!("pacdeb: {n} updates ready"),
    };
    let body = format!("{}\n{ready}", lines.join("\n"));
    let program = program();
    Command::new(&program)
        .args(["--app-name=pacdeb", "--icon=system-software-update", &title, &body])
        .status()
        .context(format!("cannot run {program} (install libnotify for notifications)"))?;
    Ok(())
}

fn notify_with(program: &str, pending: &[String], paths: &Paths) -> Result<()> {
    let seen = paths.state.join("notified.txt");
    let text = pending.join("\n");
    if pending.is_empty() {
        let _ = fs::remove_file(&seen);
        return Ok(());
    }
    if fs::read_to_string(&seen).is_ok_and(|old| old == text) {
        return Ok(());
    }
    fs::create_dir_all(&paths.state).context(paths.state.display())?;
    fs::write(&seen, &text).context(seen.display())?;

    let title = match pending.len() {
        1 => "pacdeb: 1 update available".to_string(),
        n => format!("pacdeb: {n} updates available"),
    };
    // --action waits until the notification is clicked or closed and prints the action.
    let out = Command::new(program)
        .args(["--app-name=pacdeb", "--icon=system-software-update", "--action=update=Update", &title, &text])
        .output()
        .context(format!("cannot run {program} (install libnotify for notifications)"))?;
    if String::from_utf8_lossy(&out.stdout).trim() == "update" {
        open_update_terminal()?;
    }
    Ok(())
}

/// Opens a terminal running `pacdeb update`, which stays open afterwards so the result
/// can be read. Waits for it, since the timer's service would otherwise end and take
/// the terminal with it.
fn open_update_terminal() -> Result<()> {
    let exe = std::env::current_exe().context("finding the pacdeb binary")?;
    let script = r#""$0" update; status=$?; echo; printf 'Press Enter to close. '; read -r _; exit $status"#;
    let inner: Vec<String> = ["sh", "-c", script].iter().map(|s| s.to_string()).chain([exe.display().to_string()]).collect();
    let preferred = std::env::var("TERMINAL").ok().filter(|t| !t.is_empty());
    let Some((term, args)) = pick_terminal(preferred.as_deref(), |t| on_path(t).is_some()) else {
        bail!("no terminal found to run 'pacdeb update' in; set TERMINAL to your terminal's command");
    };
    Command::new(&term).args(&args).args(&inner).status().context(format!("cannot start {term}"))?;
    Ok(())
}

/// The terminal to use and the arguments before the command. $TERMINAL wins when set,
/// taking `-e` like most terminals.
fn pick_terminal(preferred: Option<&str>, exists: impl Fn(&str) -> bool) -> Option<(String, Vec<String>)> {
    let owned = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    if let Some(t) = preferred {
        let known = TERMINALS.iter().find(|(name, _)| Path::new(t).file_name().is_some_and(|f| f == *name));
        return Some((t.to_string(), owned(known.map_or(&["-e"][..], |(_, a)| a))));
    }
    TERMINALS.iter().find(|(name, _)| exists(name)).map(|(name, args)| (name.to_string(), owned(args)))
}

fn on_path(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?.to_str()?.split(':').map(|d| Path::new(d).join(program)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_a_terminal() {
        let cases: &[(Option<&str>, &[&str], Option<(&str, &[&str])>)] = &[
            (None, &["konsole", "xterm"], Some(("konsole", &["-e"]))),
            (None, &["gnome-terminal"], Some(("gnome-terminal", &["--"]))),
            (None, &["foot"], Some(("foot", &[]))),
            (None, &[], None),
            (Some("/usr/bin/kitty"), &["konsole"], Some(("/usr/bin/kitty", &[]))),
            (Some("myterm"), &[], Some(("myterm", &["-e"]))),
        ];
        for (preferred, installed, want) in cases {
            let got = pick_terminal(*preferred, |t| installed.contains(&t));
            let want = want.map(|(t, a)| (t.to_string(), a.iter().map(|s| s.to_string()).collect::<Vec<_>>()));
            assert_eq!(got, want, "{preferred:?} {installed:?}");
        }
    }

    #[test]
    fn notifies_once_per_set_of_updates() {
        let home = Path::new(env!("CARGO_MANIFEST_DIR")).join("build/sandbox/test-notify");
        let _ = fs::remove_dir_all(&home);
        let paths = Paths { config: home.join("config"), state: home.join("state"), cache: home.join("cache") };
        let log = home.join("calls.log");
        fs::create_dir_all(&home).unwrap();
        let stub = home.join("notify-stub.sh");
        fs::write(&stub, format!("#!/bin/sh\necho \"call $*\" >> '{}'\n", log.display())).unwrap();
        fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let updates = |pending: &[String], paths: &Paths| notify_with(stub.to_str().unwrap(), pending, paths);
        let calls = || fs::read_to_string(&log).unwrap_or_default().lines().filter(|l| l.starts_with("call ")).count();
        let one = vec!["app 1.0 -> 1.1".to_string()];

        updates(&one, &paths).unwrap();
        assert_eq!(calls(), 1);
        assert!(fs::read_to_string(&log).unwrap().contains("pacdeb: 1 update available app 1.0 -> 1.1"));
        updates(&one, &paths).unwrap();
        assert_eq!(calls(), 1, "same updates, no second notification");
        updates(&[one[0].clone(), "other 2.0".into()], &paths).unwrap();
        assert_eq!(calls(), 2);
        updates(&[], &paths).unwrap();
        updates(&one, &paths).unwrap();
        assert_eq!(calls(), 3, "announced again after it was cleared");
    }
}
