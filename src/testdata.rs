// SPDX-License-Identifier: AGPL-3.0-or-later
//! Sample sources for tests, made up or made at test time so the tests need no saved
//! responses from real vendors: a version feed with channels, a GitHub releases
//! listing, a signed apt repository with a throwaway key, and an install stub.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};

/// A folder for generated test files, under the ignored build folder.
pub fn dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("build/sandbox/test-data").join(name)
}

/// A version feed with three channels, newest first, shaped like the JSON feeds some
/// vendors publish for their .deb downloads.
pub fn feed() -> String {
    let release = |channel: &str, version: &str, sum: &str| {
        format!(
            r#"{{"CategoryName": "{channel}", "Version": "{version}", "ReleaseDate": "2026-10-01T00:00:00Z",
                "File": [
                    {{"Identifier": ".rpm (Fedora/RHEL)", "Url": "https://example.com/app/{version}/app.rpm", "Sha512CheckSum": "{sum}"}},
                    {{"Identifier": ".deb (Ubuntu/Debian)", "Url": "https://example.com/app/{version}/app.deb", "Sha512CheckSum": "{sum}"}}
                ]}}"#
        )
    };
    format!(
        r#"{{"Releases": [{}, {}, {}]}}"#,
        release("Alpha", "1.15.1", &"c0ffee00".repeat(16)),
        release("EarlyAccess", "1.15.0", &"9a0f3f4a1190b010".repeat(8)),
        release("Stable", "1.14.0", &"5eed5eed".repeat(16)),
    )
}

/// A GitHub releases listing, newest first, with a draft and a release without a deb.
pub const GITHUB_RELEASES: &str = r#"[
    {"tag_name": "v1.15.0", "draft": true, "prerelease": false,
     "assets": [{"name": "app_1.15.0_amd64.deb", "browser_download_url": "https://github.com/example/app/releases/download/v1.15.0/app_1.15.0_amd64.deb"}]},
    {"tag_name": "v1.14.4", "draft": false, "prerelease": false,
     "assets": [
        {"name": "app-1.14.4.AppImage", "browser_download_url": "https://github.com/example/app/releases/download/v1.14.4/app-1.14.4.AppImage"},
        {"name": "app_1.14.4_amd64.deb", "browser_download_url": "https://github.com/example/app/releases/download/v1.14.4/app_1.14.4_amd64.deb",
         "digest": "sha256:85b10dcba6edfc1c0460a6d18260cf31c30447a444bd858a6440b9c9c8806d25"}
     ]},
    {"tag_name": "v1.14.3", "draft": false, "prerelease": false,
     "assets": [{"name": "app_1.14.3_amd64.deb", "browser_download_url": "https://github.com/example/app/releases/download/v1.14.3/app_1.14.3_amd64.deb"}]}
]"#;

/// A stub install command: logs its arguments to $FAKE_INSTALL_LOG and installs
/// nothing. Run it through sh, since a script written moments ago can still be open
/// for writing in another test thread.
pub fn install_stub() -> PathBuf {
    static STUB: OnceLock<PathBuf> = OnceLock::new();
    STUB.get_or_init(|| {
        let dir = dir("stub");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fake-install.sh");
        fs::write(&path, "printf '%s\\n' \"$*\" >> \"$FAKE_INSTALL_LOG\"\n").unwrap();
        path
    })
    .clone()
}

/// A signed apt repository: one component, main, for amd64.
pub struct AptRepo {
    /// The armored public key.
    pub key: PathBuf,
    pub fingerprint: String,
    pub inrelease: Vec<u8>,
    pub packages_gz: Vec<u8>,
}

/// The package index of the sample repository.
pub const PACKAGES: &str = "\
Package: example-app
Version: 1.2-1
Architecture: amd64
Maintainer: Example <apps@example.com>
Installed-Size: 1024
Depends: libgtk-3-0, libnss3
Filename: pool/main/e/example-app/example-app_1.2-1_amd64.deb
Size: 52428
SHA256: 1111111111111111111111111111111111111111111111111111111111111111
Description: An example app

Package: example-app
Version: 1.10-1
Architecture: amd64
Maintainer: Example <apps@example.com>
Installed-Size: 1100
Depends: libgtk-3-0, libnss3
Filename: pool/main/e/example-app/example-app_1.10-1_amd64.deb
Size: 53000
SHA256: 2222222222222222222222222222222222222222222222222222222222222222
Description: An example app

Package: example-app
Version: 1.9-1
Architecture: amd64
Maintainer: Example <apps@example.com>
Installed-Size: 1090
Filename: pool/main/e/example-app/example-app_1.9-1_amd64.deb
Size: 52900
SHA256: 3333333333333333333333333333333333333333333333333333333333333333
Description: An example app

Package: example-data
Version: 2.0
Architecture: all
Maintainer: Example <apps@example.com>
Filename: pool/main/e/example-data/example-data_2.0_all.deb
Size: 1000
SHA256: 4444444444444444444444444444444444444444444444444444444444444444
Description: Data for the example app
";

/// The sample repository, made once per test run with its own throwaway key.
pub fn apt_repo() -> &'static AptRepo {
    static REPO: OnceLock<AptRepo> = OnceLock::new();
    REPO.get_or_init(make_apt_repo)
}

fn gpg(home: &Path) -> Command {
    let mut c = Command::new("gpg");
    c.arg("--homedir").arg(home).args(["--batch", "--pinentry-mode", "loopback", "--passphrase", ""]);
    c
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().expect("gpg is needed for these tests");
    assert!(out.status.success(), "{cmd:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

fn make_apt_repo() -> AptRepo {
    let dir = dir("apt");
    let _ = fs::remove_dir_all(&dir);
    let home = dir.join("gnupg");
    fs::create_dir_all(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();

    run(gpg(&home).args(["--quick-gen-key", "Example Repository <repo@example.com>", "ed25519", "sign", "never"]));
    let colons = run(gpg(&home).args(["--with-colons", "--list-secret-keys"]));
    let fingerprint = colons.lines().find_map(|l| l.strip_prefix("fpr:")).unwrap().trim_matches(':').to_string();
    let key = dir.join("key.asc");
    run(gpg(&home).args(["--armor", "--output"]).arg(&key).args(["--export", &fingerprint]));

    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    std::io::Write::write_all(&mut gz, PACKAGES.as_bytes()).unwrap();
    let packages_gz = gz.finish().unwrap();
    let sha: String = Sha256::digest(&packages_gz).iter().map(|b| format!("{b:02x}")).collect();
    let release = format!(
        "Origin: Example\nLabel: Example\nSuite: stable\nCodename: stable\nDate: Sat, 03 Oct 2026 12:00:00 UTC\n\
         Architectures: amd64\nComponents: main\nSHA256:\n {sha} {} main/binary-amd64/Packages.gz\n",
        packages_gz.len()
    );
    let release_path = dir.join("Release");
    fs::write(&release_path, release).unwrap();
    let inrelease = dir.join("InRelease");
    run(gpg(&home).args(["--local-user", &fingerprint, "--clearsign", "--output"]).arg(&inrelease).arg(&release_path));
    let _ = Command::new("gpgconf").arg("--homedir").arg(&home).args(["--kill", "gpg-agent"]).status();

    AptRepo { key, fingerprint, inrelease: fs::read(&inrelease).unwrap(), packages_gz }
}
