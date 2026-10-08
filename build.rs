// Records which commit the binaries were built from, for 'pacdeb --version'.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

fn main() {
    let build = match (git(&["rev-parse", "--short", "HEAD"]), git(&["log", "-1", "--format=%cd", "--date=short"])) {
        (Some(hash), Some(date)) => {
            // Uncommitted changes in the source make it a different build than the commit.
            let dirty = Command::new("git").args(["diff", "--quiet", "HEAD", "--", "src", "data", "Cargo.toml"]).status().is_ok_and(|s| !s.success());
            format!("{hash}{}, {date}", if dirty { " modified" } else { "" })
        }
        _ => "no git information".to_string(),
    };
    println!("cargo:rustc-env=PACDEB_BUILD={build}");
    println!("cargo:rerun-if-changed=.git/HEAD");
    // A new commit moves the branch, not HEAD, so the branch's ref is watched too.
    if let Some(branch) = std::fs::read_to_string(".git/HEAD").ok().and_then(|h| h.strip_prefix("ref: ").map(|r| r.trim().to_string())) {
        println!("cargo:rerun-if-changed=.git/{branch}");
    }
    println!("cargo:rerun-if-changed=.git/packed-refs");
    println!("cargo:rerun-if-changed=.git/index");
    println!("cargo:rerun-if-changed=src");
}
