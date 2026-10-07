//! Hands built packages to pacman. Ferry never asks for or stores the sudo password:
//! sudo and pacman talk to the terminal directly.

use std::path::PathBuf;
use std::process::Command;

use crate::error::{Result, bail};

/// Runs `sudo pacman -U <pkgs>`, or `FERRY_INSTALL_CMD <pkgs>` when that is set. Several
/// packages go in one call, so there is one password prompt and one confirmation.
pub fn install(pkgs: &[PathBuf]) -> Result<()> {
    let custom = std::env::var("FERRY_INSTALL_CMD").ok().filter(|c| !c.trim().is_empty());
    run(custom.as_deref(), pkgs)
}

fn run(custom: Option<&str>, pkgs: &[PathBuf]) -> Result<()> {
    let (program, args): (&str, Vec<&str>) = match custom {
        Some(cmd) => {
            let mut parts = cmd.split_whitespace();
            (parts.next().unwrap_or_default(), parts.collect())
        }
        None => ("sudo", vec!["pacman", "-U"]),
    };
    let listed = pkgs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ");
    let kept = if pkgs.len() == 1 {
        format!("the built package is kept at {listed}")
    } else {
        format!("the built packages are kept at {listed}")
    };
    let shown = std::iter::once(program).chain(args.iter().copied()).collect::<Vec<_>>().join(" ");
    match Command::new(program).args(&args).args(pkgs).status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => bail!("{shown} failed ({s}); {kept}"),
        Err(e) => bail!("cannot run {program}: {e}; {kept}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn stub() -> String {
        format!("{}/tests/fixtures/fake-install.sh", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn passes_all_packages_in_one_call() {
        let log = Path::new(env!("CARGO_MANIFEST_DIR")).join("build/sandbox/test-install/log");
        let _ = std::fs::remove_file(&log);
        let pkgs = [PathBuf::from("/tmp/a-1.0-1-x86_64.pkg.tar.zst"), PathBuf::from("/tmp/b-2.0-1-x86_64.pkg.tar.zst")];
        // The stub reads where to log from its environment; a wrapper sets it without
        // touching this process's environment.
        let cmd = format!("env FAKE_INSTALL_LOG={} {}", log.display(), stub());
        run(Some(&cmd), &pkgs).unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), format!("{} {}\n", pkgs[0].display(), pkgs[1].display()));
    }

    #[test]
    fn reports_failures_and_keeps_the_packages() {
        let pkg = [PathBuf::from("/x/demo.pkg.tar.zst")];
        let err = run(Some("false"), &pkg).unwrap_err().to_string();
        assert!(err.starts_with("false failed"), "{err}");
        assert!(err.contains("kept at /x/demo.pkg.tar.zst"), "{err}");
        let err = run(Some("/no/such/program"), &pkg).unwrap_err().to_string();
        assert!(err.contains("cannot run /no/such/program"), "{err}");
    }
}
