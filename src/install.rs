//! Hands a built package to pacman. Ferry never asks for or stores the sudo password:
//! sudo and pacman talk to the terminal directly.

use std::path::Path;
use std::process::Command;

use crate::error::{Result, bail};

/// Runs `sudo pacman -U <pkg>`, or `FERRY_INSTALL_CMD <pkg>` when that is set.
pub fn install(pkg: &Path) -> Result<()> {
    let custom = std::env::var("FERRY_INSTALL_CMD").ok().filter(|c| !c.trim().is_empty());
    run(custom.as_deref(), pkg)
}

fn run(custom: Option<&str>, pkg: &Path) -> Result<()> {
    let (program, args): (&str, Vec<&str>) = match custom {
        Some(cmd) => {
            let mut parts = cmd.split_whitespace();
            (parts.next().unwrap_or_default(), parts.collect())
        }
        None => ("sudo", vec!["pacman", "-U"]),
    };
    let kept = format!("the built package is kept at {}", pkg.display());
    let shown = std::iter::once(program).chain(args.iter().copied()).collect::<Vec<_>>().join(" ");
    match Command::new(program).args(&args).arg(pkg).status() {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => bail!("{shown} failed ({s}); {kept}"),
        Err(e) => bail!("cannot run {program}: {e}; {kept}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn stub() -> String {
        format!("{}/tests/fixtures/fake-install.sh", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn passes_the_package_to_the_command() {
        let log = Path::new(env!("CARGO_MANIFEST_DIR")).join("build/sandbox/test-install/log");
        let _ = std::fs::remove_file(&log);
        let pkg = PathBuf::from("/tmp/demo-1.0-1-x86_64.pkg.tar.zst");
        // The stub reads where to log from its environment; a wrapper sets it without
        // touching this process's environment.
        let cmd = format!("env FAKE_INSTALL_LOG={} {}", log.display(), stub());
        run(Some(&cmd), &pkg).unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), format!("{}\n", pkg.display()));
    }

    #[test]
    fn reports_failures_and_keeps_the_package() {
        let pkg = Path::new("/x/demo.pkg.tar.zst");
        let err = run(Some("false"), pkg).unwrap_err().to_string();
        assert!(err.starts_with("false failed"), "{err}");
        assert!(err.contains("kept at /x/demo.pkg.tar.zst"), "{err}");
        let err = run(Some("/no/such/program"), pkg).unwrap_err().to_string();
        assert!(err.contains("cannot run /no/such/program"), "{err}");
    }
}
