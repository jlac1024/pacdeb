//! Turns the translated model into a .pkg.tar.zst.

mod direct;
mod pkgbuild;
mod tree;

use std::fs;
use std::io::{Read, Seek};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::deb::Deb;
use crate::error::{Context, Result, bail};
use crate::model::Package;

/// Builds with makepkg. The work dir `<work_root>/<pkgname>` is recreated each time and
/// keeps the PKGBUILD afterwards for reference; the finished package goes to `out_dir`.
pub fn with_makepkg<R: Read + Seek>(
    deb: &mut Deb<R>,
    pkg: &Package,
    origin: &str,
    work_root: &Path,
    out_dir: &Path,
) -> Result<PathBuf> {
    let work = work_root.join(&pkg.name);
    remove_tree(&work)?;
    fs::create_dir_all(&work).context(work.display())?;
    fs::create_dir_all(out_dir).context(out_dir.display())?;

    let tree = work.join(pkgbuild::TREE_DIR);
    tree::write(deb, pkg, &tree).context("writing the package files")?;
    fs::write(work.join("PKGBUILD"), pkgbuild::render(pkg, origin)).context("writing the PKGBUILD")?;
    if let Some(script) = pkgbuild::install_script(pkg) {
        fs::write(work.join(pkgbuild::install_file_name(pkg)), script).context("writing the .install file")?;
    }

    let status = Command::new("makepkg")
        .args(["--force", "--nodeps", "--clean"])
        .current_dir(&work)
        .env("PKGDEST", out_dir)
        .status()
        .context("cannot run makepkg")?;
    if !status.success() {
        bail!("makepkg failed ({status}); the PKGBUILD and files are in {}", work.display());
    }

    // makepkg knows the final name, including the PKGEXT the user configured.
    let list = Command::new("makepkg")
        .arg("--packagelist")
        .current_dir(&work)
        .env("PKGDEST", out_dir)
        .output()
        .context("cannot run makepkg --packagelist")?;
    let listed = String::from_utf8_lossy(&list.stdout);
    let Some(built) = listed.lines().map(PathBuf::from).find(|p| p.exists()) else {
        bail!("makepkg finished but the package is not in {}", out_dir.display());
    };
    remove_tree(&tree)?;
    Ok(built)
}

/// Builds without makepkg: Ferry writes .PKGINFO, .MTREE and the archive itself.
pub fn direct<R: Read + Seek>(deb: &mut Deb<R>, pkg: &Package, work_root: &Path, out_dir: &Path) -> Result<PathBuf> {
    let work = work_root.join(&pkg.name);
    remove_tree(&work)?;
    fs::create_dir_all(out_dir).context(out_dir.display())?;
    let tree = work.join(pkgbuild::TREE_DIR);
    tree::write(deb, pkg, &tree).context("writing the package files")?;
    let install = pkgbuild::install_script(pkg);
    let built = direct::pack(pkg, &tree, install.as_deref(), out_dir, build_time()).context("writing the package")?;
    remove_tree(&work)?;
    Ok(built)
}

/// Seconds since the epoch, or SOURCE_DATE_EPOCH when set, as makepkg does.
fn build_time() -> u64 {
    std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs())
        })
}

/// Whether makepkg is on PATH.
pub fn makepkg_available() -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|d| d.join("makepkg").is_file()))
}

/// Deletes a tree even when it contains read only directories.
fn remove_tree(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    make_writable(path)?;
    fs::remove_dir_all(path).context(path.display())
}

fn make_writable(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path).context(path.display())?;
    if meta.is_dir() {
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() | 0o700);
        fs::set_permissions(path, perms).context(path.display())?;
        for entry in fs::read_dir(path).context(path.display())? {
            make_writable(&entry.context(path.display())?.path())?;
        }
    }
    Ok(())
}
