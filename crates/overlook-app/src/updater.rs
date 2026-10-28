//! Update check and in-place update.
//!
//! Releases live on GitHub. The app asks the releases API for the latest tag, compares it
//! with its own version, and offers to update. How the update is applied depends on how
//! Overlook was installed:
//!
//! - **pacman package** (Arch/Omarchy): download the release package, verify its checksum,
//!   then install it with `pkexec pacman -U` (a polkit password dialog), falling back to a
//!   terminal running `sudo pacman -U` when no polkit agent is available.
//! - **user prefix** (`~/.local` or `--prefix`): download the tarball, verify, and replace
//!   the binary atomically; no password needed.
//! - **development build** (`target/`): no in-place update; pull and rebuild.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const REPO: &str = "last-refuge/overlook";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: String,
    pub tag: String,
    pub html_url: String,
    pub notes: String,
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

impl ReleaseInfo {
    /// The first asset with this suffix, skipping makepkg's companion `-debug-` package,
    /// which shares the `.pkg.tar.zst` suffix.
    pub fn asset_ending_with(&self, suffix: &str) -> Option<&ReleaseAsset> {
        self.assets.iter().find(|a| a.name.ends_with(suffix) && !a.name.contains("-debug-"))
    }

    pub fn package(&self) -> Option<&ReleaseAsset> {
        self.asset_ending_with("x86_64.pkg.tar.zst")
    }

    pub fn tarball(&self) -> Option<&ReleaseAsset> {
        self.asset_ending_with("x86_64-linux.tar.gz")
    }

    pub fn checksums(&self) -> Option<&ReleaseAsset> {
        self.assets.iter().find(|a| a.name == "SHA256SUMS")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// Installed by pacman; the binary is `/usr/bin/overlook`.
    Pacman,
    /// Installed into a prefix the user can write to.
    Prefix { bin: PathBuf, prefix: PathBuf },
    /// Running from a cargo target directory.
    Development,
    /// Somewhere we cannot write and not a package.
    Unknown(PathBuf),
}

impl InstallKind {
    pub fn detect() -> Self {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("overlook"));
        let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
        let text = exe.to_string_lossy();
        if text.contains("/target/debug/") || text.contains("/target/release/") {
            return InstallKind::Development;
        }
        if exe == Path::new("/usr/bin/overlook") {
            let packaged = Command::new("pacman").args(["-Qi", "overlook"]).output().map(|o| o.status.success()).unwrap_or(false);
            if packaged {
                return InstallKind::Pacman;
            }
        }
        let Some(dir) = exe.parent() else { return InstallKind::Unknown(exe) };
        let writable = std::fs::metadata(dir).map(|m| !m.permissions().readonly()).unwrap_or(false) && test_writable(dir);
        if writable {
            let prefix = dir.parent().map(Path::to_path_buf).unwrap_or_else(|| dir.to_path_buf());
            InstallKind::Prefix { bin: exe, prefix }
        } else {
            InstallKind::Unknown(exe)
        }
    }

    pub fn describe(&self) -> String {
        match self {
            InstallKind::Pacman => "Installed with pacman".into(),
            InstallKind::Prefix { prefix, .. } => format!("Installed in {}", prefix.display()),
            InstallKind::Development => "Development build".into(),
            InstallKind::Unknown(p) => format!("Running from {}", p.display()),
        }
    }
}

fn test_writable(dir: &Path) -> bool {
    let probe = dir.join(".overlook-write-test");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Compares dotted versions numerically; pre-release suffixes sort before the release.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    fn parts(v: &str) -> (Vec<u64>, bool) {
        let v = v.trim().trim_start_matches('v');
        let (core, pre) = match v.split_once('-') {
            Some((c, _)) => (c, true),
            None => (v, false),
        };
        (core.split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect(), pre)
    }
    let (a, a_pre) = parts(candidate);
    let (b, b_pre) = parts(current);
    let len = a.len().max(b.len());
    for i in 0..len {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    // Same numbers: a release is newer than a pre-release of the same version.
    !a_pre && b_pre
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .https_only(true)
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent(format!("Overlook/{CURRENT_VERSION} (+https://github.com/{REPO})"))
        .build()
        .into()
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    body: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

/// Asks GitHub for the latest release. Blocking; run on a background thread.
pub fn check_latest() -> Result<ReleaseInfo, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let mut response = agent().get(&url).header("Accept", "application/vnd.github+json").call().map_err(|e| format!("could not reach GitHub: {}", overlook_provider::secure_error::ureq("update check", e)))?;
    let status = response.status().as_u16();
    if status == 404 {
        return Err("No release has been published yet.".into());
    }
    if status != 200 {
        return Err(format!("GitHub answered {status}."));
    }
    let release: ApiRelease = response.body_mut().read_json().map_err(|e| format!("unexpected answer from GitHub: {}", overlook_provider::secure_error::ureq("update check response", e)))?;
    if release.draft || release.prerelease {
        return Err("The latest release is not ready yet.".into());
    }
    Ok(ReleaseInfo {
        version: release.tag_name.trim_start_matches('v').to_owned(),
        tag: release.tag_name,
        html_url: release.html_url,
        notes: release.body.unwrap_or_default(),
        assets: release.assets.into_iter().map(|a| ReleaseAsset { name: a.name, url: a.browser_download_url, size: a.size }).collect(),
    })
}

pub struct Progress {
    pub downloaded: AtomicU64,
    pub total: AtomicU64,
}

impl Progress {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { downloaded: AtomicU64::new(0), total: AtomicU64::new(0) })
    }

    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.load(Ordering::Relaxed);
        (total > 0).then(|| self.downloaded.load(Ordering::Relaxed) as f32 / total as f32)
    }
}

/// Downloads `asset` into `dir`, reporting progress. Blocking.
pub fn download(asset: &ReleaseAsset, dir: &Path, progress: &Progress) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let dest = dir.join(&asset.name);
    let part = dir.join(format!("{}.part", asset.name));
    let mut response = agent().get(&asset.url).call().map_err(|e| format!("download failed: {}", overlook_provider::secure_error::ureq("update download", e)))?;
    if response.status().as_u16() != 200 {
        return Err(format!("download failed: HTTP {}", response.status()));
    }
    progress.total.store(asset.size, Ordering::Relaxed);
    let mut file = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut reader = response.body_mut().as_reader();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("download interrupted: {}", overlook_provider::secure_error::io("update download", &e)))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        progress.downloaded.fetch_add(n as u64, Ordering::Relaxed);
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Fetches the release's SHA256SUMS and returns the expected digest for `name`.
pub fn expected_sha256(release: &ReleaseInfo, name: &str) -> Result<String, String> {
    let sums = release.checksums().ok_or("the release has no SHA256SUMS file")?;
    let mut response = agent().get(&sums.url).call().map_err(|e| format!("could not fetch checksums: {}", overlook_provider::secure_error::ureq("update checksums", e)))?;
    let text = response.body_mut().read_to_string().map_err(|e| overlook_provider::secure_error::ureq("update checksums", e).to_owned())?;
    parse_sha256sums(&text, name).ok_or_else(|| format!("no checksum listed for {name}"))
}

pub fn parse_sha256sums(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let digest = parts.next()?;
        let file = parts.next()?.trim_start_matches('*');
        (file == name || file.ends_with(&format!("/{name}"))).then(|| digest.to_ascii_lowercase())
    })
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Replaces the binary in a user prefix with the one in the tarball (atomic rename), and
/// refreshes the desktop entry and icons next to it.
pub fn apply_prefix_update(tarball: &Path, bin: &Path, prefix: &Path) -> Result<(), String> {
    let work = tarball.parent().unwrap_or(Path::new("/tmp")).join("extract");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let status = Command::new("tar").args(["-xzf"]).arg(tarball).arg("-C").arg(&work).status().map_err(|e| format!("tar failed: {e}"))?;
    if !status.success() {
        return Err("could not extract the update".into());
    }
    // The tarball has one top-level directory.
    let root = std::fs::read_dir(&work).map_err(|e| e.to_string())?.filter_map(|e| e.ok()).map(|e| e.path()).find(|p| p.is_dir()).ok_or("unexpected tarball layout")?;
    let new_bin = root.join("overlook");
    if !new_bin.is_file() {
        return Err("the update has no overlook binary".into());
    }
    let staged = bin.with_extension("new");
    std::fs::copy(&new_bin, &staged).map_err(|e| format!("could not stage the binary: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&staged, bin).map_err(|e| format!("could not replace the binary: {e}"))?;
    // Desktop entry and icons: best effort, only where they already exist.
    let apps = prefix.join("share/applications/ai.lastrefuge.Overlook.desktop");
    if apps.exists() {
        let _ = std::fs::copy(root.join("packaging/ai.lastrefuge.Overlook.desktop"), &apps);
    }
    let icons = prefix.join("share/icons/hicolor");
    if icons.is_dir() {
        let _ = std::fs::copy(root.join("icons/overlook-app-icon.svg"), icons.join("scalable/apps/ai.lastrefuge.Overlook.svg"));
        for s in [16, 24, 32, 48, 64, 128, 256, 512] {
            let _ = std::fs::copy(root.join(format!("icons/overlook-icon-{s}.png")), icons.join(format!("{s}x{s}/apps/ai.lastrefuge.Overlook.png")));
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    Ok(())
}

/// Installs a pacman package: polkit dialog first, then a terminal with sudo.
pub fn apply_pacman_update(package: &Path) -> Result<PacmanOutcome, String> {
    if Command::new("pkexec").arg("--version").output().map(|o| o.status.success()).unwrap_or(false) {
        let output = Command::new("pkexec").args(["pacman", "-U", "--noconfirm"]).arg(package).output().map_err(|e| e.to_string())?;
        if output.status.success() {
            return Ok(PacmanOutcome::Installed);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        // 126 = dismissed/denied by the user; 127 = no agent or not authorized.
        if output.status.code() == Some(126) {
            return Err("The password dialog was cancelled.".into());
        }
        tracing::warn!("pkexec failed ({:?}): {}", output.status.code(), stderr.trim());
    }
    // No polkit agent: hand the step to a terminal so the user can type their password.
    // Pass the path as an argument, never interpolate it into shell program text.
    let script = "printf 'Overlook update: installing %s\\n' \"$1\"; sudo pacman -U --noconfirm -- \"$1\" && echo && echo 'Done. Restart Overlook to finish.' || echo 'The update did not install.'; echo; read -r -p 'Press Enter to close this window.' _";
    let terminals: Vec<Vec<String>> = {
        let mut t: Vec<Vec<String>> = vec![vec!["xdg-terminal-exec".into(), "--".into()]];
        if let Ok(term) = std::env::var("TERMINAL") {
            t.push(vec![term, "-e".into()]);
        }
        for name in ["alacritty", "foot", "kitty", "ghostty", "gnome-terminal", "konsole"] {
            t.push(vec![name.into(), if name == "gnome-terminal" { "--".into() } else { "-e".into() }]);
        }
        t
    };
    for cmd in terminals {
        let Some((program, args)) = cmd.split_first() else { continue };
        let ok = Command::new(program).args(args).arg("bash").arg("-c").arg(script).arg("overlook-update").arg(package).spawn().is_ok();
        if ok {
            return Ok(PacmanOutcome::TerminalOpened);
        }
    }
    Err(format!("Could not open a terminal. Run: sudo pacman -U {}", package.display()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacmanOutcome {
    Installed,
    /// A terminal window is asking for the password; the result is not known here.
    TerminalOpened,
}

/// Starts a fresh copy of the current executable and exits this one.
pub fn restart() -> ! {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("overlook"));
    let _ = Command::new(exe).spawn();
    std::process::exit(0)
}

pub fn cache_dir() -> PathBuf {
    directories::ProjectDirs::from("ai", "Last Refuge", "Overlook")
        .map(|d| d.cache_dir().join("updates"))
        .unwrap_or_else(|| std::env::temp_dir().join("overlook-updates"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v0.1.1", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.1.0", "0.1.0-beta.1"));
        assert!(!is_newer("0.1.0-beta.1", "0.1.0"));
    }

    #[test]
    fn checksum_parsing() {
        let sums = "abc123  overlook-0.2.0-1-x86_64.pkg.tar.zst\nDEF456 *overlook-0.2.0-x86_64-linux.tar.gz\n";
        assert_eq!(parse_sha256sums(sums, "overlook-0.2.0-x86_64-linux.tar.gz").as_deref(), Some("def456"));
        assert_eq!(parse_sha256sums(sums, "overlook-0.2.0-1-x86_64.pkg.tar.zst").as_deref(), Some("abc123"));
        assert!(parse_sha256sums(sums, "missing").is_none());
    }

    #[test]
    fn skips_debug_package_asset() {
        let asset = |name: &str| ReleaseAsset { name: name.into(), url: String::new(), size: 0 };
        let info = ReleaseInfo {
            version: "0.1.0".into(),
            tag: "v0.1.0".into(),
            html_url: String::new(),
            notes: String::new(),
            assets: vec![asset("overlook-debug-0.1.0-1-x86_64.pkg.tar.zst"), asset("overlook-0.1.0-1-x86_64.pkg.tar.zst"), asset("SHA256SUMS")],
        };
        assert_eq!(info.package().unwrap().name, "overlook-0.1.0-1-x86_64.pkg.tar.zst");
    }

    #[test]
    fn detects_development_build() {
        assert_eq!(InstallKind::detect(), InstallKind::Development);
    }
}
