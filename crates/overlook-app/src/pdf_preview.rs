//! PDF page rendering for the quick look and attachment thumbnails, through poppler's
//! `pdftoppm`. Renders are cached under the app's cache directory, keyed by the file's
//! path, size and modification time, so a document is rasterised once.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime};

pub const MAX_PAGES: usize = 40;
pub const MAX_CACHE_BYTES: u64 = 200 * 1024 * 1024;
const PAGE_WIDTH_PX: u32 = 1400;
const THUMB_WIDTH_PX: u32 = 144;
const PDFTOPPM_TIMEOUT: Duration = Duration::from_secs(30);
const STALE_TMP_AGE: Duration = Duration::from_secs(3600);
const PRUNE_EVERY_RENDERS: u64 = 16;

/// Whether page rendering is possible on this machine. Cached once per process.
pub fn available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| which("pdftoppm"))
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH").map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file())).unwrap_or(false)
}

fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> std::io::Result<ExitStatus> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        let now = Instant::now();
        if now >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "pdftoppm timed out"));
        }
        std::thread::sleep((deadline - now).min(Duration::from_millis(10)));
    }
}

fn cache_key(pdf: &Path) -> String {
    let mut h = DefaultHasher::new();
    pdf.hash(&mut h);
    if let Ok(meta) = std::fs::metadata(pdf) {
        meta.len().hash(&mut h);
        if let Ok(modified) = meta.modified() {
            if let Ok(d) = modified.duration_since(std::time::UNIX_EPOCH) {
                d.as_secs().hash(&mut h);
            }
        }
    }
    format!("{:016x}", h.finish())
}

/// Renders up to [`MAX_PAGES`] pages as PNG files and returns their paths in page order.
pub fn render_pages(pdf: &Path) -> std::io::Result<Vec<PathBuf>> {
    let base = crate::updater::cache_dir().join("pdf");
    render_pages_in(&base, Path::new("pdftoppm"), pdf)
}

fn render_pages_in(base: &Path, program: &Path, pdf: &Path) -> std::io::Result<Vec<PathBuf>> {
    let dir = base.join(cache_key(pdf));
    let done = dir.join(".done");
    if !done.exists() {
        std::fs::create_dir_all(base)?;
        let stage = tempfile::Builder::new().prefix(".tmp-pages-").tempdir_in(base)?;
        let prefix = stage.path().join("page");
        let mut cmd = Command::new(program);
        cmd.arg("-png")
            .arg("-f").arg("1")
            .arg("-l").arg(MAX_PAGES.to_string())
            .arg("-scale-to-x").arg(PAGE_WIDTH_PX.to_string())
            .arg("-scale-to-y").arg("-1")
            .arg(pdf)
            .arg(&prefix);
        // Poppler reports a damaged file on stderr; the exit status says enough.
        let status = run_with_timeout(&mut cmd, PDFTOPPM_TIMEOUT)?;
        if !status.success() {
            return Err(std::io::Error::other(format!("pdftoppm exited with {status}")));
        }
        std::fs::create_dir_all(&dir)?;
        for png in numbered_pngs(stage.path(), "page") {
            if let Some(name) = png.file_name() {
                std::fs::rename(&png, dir.join(name))?;
            }
        }
        let stage_done = stage.path().join(".done");
        std::fs::write(&stage_done, b"")?;
        std::fs::rename(&stage_done, &done)?;
        maybe_prune_cache(base);
    }
    Ok(numbered_pngs(&dir, "page"))
}

/// A small first-page image for the attachment card.
pub fn render_thumbnail(pdf: &Path) -> std::io::Result<PathBuf> {
    let base = crate::updater::cache_dir().join("pdf");
    render_thumbnail_in(&base, Path::new("pdftoppm"), pdf)
}

fn render_thumbnail_in(base: &Path, program: &Path, pdf: &Path) -> std::io::Result<PathBuf> {
    let dir = base.join(cache_key(pdf));
    let out = dir.join("thumb.png");
    if out.exists() {
        return Ok(out);
    }
    std::fs::create_dir_all(&dir)?;
    let stage = tempfile::Builder::new().prefix(".tmp-thumb-").tempdir_in(&dir)?;
    let mut cmd = Command::new(program);
    cmd.arg("-png")
        .arg("-singlefile")
        .arg("-f").arg("1")
        .arg("-l").arg("1")
        .arg("-scale-to").arg(THUMB_WIDTH_PX.to_string())
        .arg(pdf)
        .arg(stage.path().join("thumb"));
    let status = run_with_timeout(&mut cmd, PDFTOPPM_TIMEOUT)?;
    let staged = stage.path().join("thumb.png");
    if !status.success() || !staged.exists() {
        return Err(std::io::Error::other("pdftoppm produced no thumbnail"));
    }
    std::fs::rename(&staged, &out)?;
    maybe_prune_cache(base);
    Ok(out)
}

/// Caps the PDF render cache on disk to [`MAX_CACHE_BYTES`], evicting the oldest entries first.
pub fn prune_cache() {
    let base = crate::updater::cache_dir().join("pdf");
    prune_cache_dir(&base, MAX_CACHE_BYTES);
}

fn maybe_prune_cache(base: &Path) {
    static RENDERS: AtomicU64 = AtomicU64::new(0);
    if RENDERS.fetch_add(1, Ordering::Relaxed).is_multiple_of(PRUNE_EVERY_RENDERS) {
        prune_cache_dir(base, MAX_CACHE_BYTES);
    }
}

fn entry_stats(path: &Path) -> (u64, SystemTime) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return (0, SystemTime::UNIX_EPOCH);
    };
    let mut newest = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    if meta.is_file() {
        return (meta.len(), newest);
    }
    if !meta.is_dir() {
        return (0, newest);
    }
    let mut total = 0u64;
    if let Ok(rd) = std::fs::read_dir(path) {
        for entry in rd.flatten() {
            let (bytes, mtime) = entry_stats(&entry.path());
            total = total.saturating_add(bytes);
            if mtime > newest {
                newest = mtime;
            }
        }
    }
    (total, newest)
}

fn prune_cache_dir(root: &Path, max_bytes: u64) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    let mut entries: Vec<(PathBuf, bool, u64, SystemTime)> = Vec::new();
    let mut total = 0u64;
    for entry in rd.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if name.starts_with(".tmp-") {
            let mtime = entry.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
            if now.duration_since(mtime).unwrap_or_default() >= STALE_TMP_AGE {
                if is_dir {
                    let _ = std::fs::remove_dir_all(&path);
                } else {
                    let _ = std::fs::remove_file(&path);
                }
            }
            continue;
        }
        let (size, mtime) = entry_stats(&path);
        total = total.saturating_add(size);
        entries.push((path, is_dir, size, mtime));
    }
    if total <= max_bytes {
        return;
    }
    entries.sort_by_key(|(_, _, _, mtime)| *mtime);
    for (path, is_dir, size, _) in entries {
        if total <= max_bytes {
            break;
        }
        let removed = if is_dir {
            std::fs::remove_dir_all(&path).is_ok()
        } else {
            std::fs::remove_file(&path).is_ok()
        };
        if removed {
            total = total.saturating_sub(size);
        }
    }
}

/// `prefix-1.png`, `prefix-01.png` … sorted by page number.
fn numbered_pngs(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut pages: Vec<(usize, PathBuf)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter_map(|p| {
                    let name = p.file_name()?.to_str()?.to_owned();
                    let num = name.strip_prefix(prefix)?.strip_prefix('-')?.strip_suffix(".png")?.parse::<usize>().ok()?;
                    Some((num, p))
                })
                .collect()
        })
        .unwrap_or_default();
    pages.sort_by_key(|(n, _)| *n);
    pages.into_iter().map(|(_, p)| p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn orders_pages_numerically() {
        let dir = std::env::temp_dir().join(format!("overlook-pdf-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for n in ["page-10.png", "page-2.png", "page-1.png", "thumb.png"] {
            std::fs::write(dir.join(n), b"").unwrap();
        }
        let pages = numbered_pngs(&dir, "page");
        let names: Vec<String> = pages.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["page-1.png", "page-2.png", "page-10.png"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_with_timeout_kills_hung_subprocess() {
        let dir = tempfile::tempdir().unwrap();
        let hung = dir.path().join("fake-pdftoppm");
        std::fs::write(&hung, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&hung, std::fs::Permissions::from_mode(0o700)).unwrap();
        let started = Instant::now();
        let err = run_with_timeout(&mut Command::new(&hung), Duration::from_millis(100)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn prune_cache_dir_evicts_oldest_entries_above_budget() {
        let root = tempfile::tempdir().unwrap();
        let old = root.path().join("old-doc");
        let mid = root.path().join("mid-doc");
        let new = root.path().join("new-doc");
        let active_tmp = root.path().join(".tmp-pages-active");
        for d in [&old, &mid, &new, &active_tmp] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(old.join("page-1.png"), vec![1u8; 400]).unwrap();
        std::thread::sleep(Duration::from_millis(25));
        std::fs::write(mid.join("page-1.png"), vec![2u8; 400]).unwrap();
        std::thread::sleep(Duration::from_millis(25));
        std::fs::write(new.join("page-1.png"), vec![3u8; 400]).unwrap();
        std::fs::write(active_tmp.join("page-1.png"), vec![4u8; 400]).unwrap();

        prune_cache_dir(root.path(), 500);
        assert!(!old.exists(), "oldest entry should be evicted first");
        assert!(!mid.exists(), "second oldest entry should be evicted to meet budget");
        assert!(new.exists(), "newest entry fits within budget and should remain");
        assert!(active_tmp.exists(), "active temp staging dirs must not be removed during pruning");
    }

    #[test]
    fn atomic_render_leaves_no_partial_files_on_failure_and_publishes_cleanly_on_success() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        let pdf = root.path().join("doc.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n").unwrap();

        // Failing pdftoppm writes partial output then exits non-zero; cache dir must stay clean.
        let failing = root.path().join("fail-pdftoppm");
        std::fs::write(&failing, "#!/bin/sh\neval \"prefix=\\${$#}\"\nprintf 'partial' > \"${prefix}.png\"\nprintf 'partial' > \"${prefix}-1.png\"\nexit 1\n").unwrap();
        std::fs::set_permissions(&failing, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert!(render_thumbnail_in(&cache, &failing, &pdf).is_err());
        assert!(!cache.join(cache_key(&pdf)).join("thumb.png").exists(), "failed thumbnail must not publish partial file");
        assert!(render_pages_in(&cache, &failing, &pdf).is_err());
        assert!(!cache.join(cache_key(&pdf)).join(".done").exists(), "failed pages render must not publish .done");
        assert!(numbered_pngs(&cache.join(cache_key(&pdf)), "page").is_empty());

        // Succeeding pdftoppm publishes complete outputs atomically and cleans up temp staging dirs.
        let ok = root.path().join("ok-pdftoppm");
        std::fs::write(&ok, "#!/bin/sh\neval \"prefix=\\${$#}\"\nprintf 'PNG-THUMB' > \"${prefix}.png\"\nprintf 'PNG-P1' > \"${prefix}-1.png\"\nprintf 'PNG-P2' > \"${prefix}-2.png\"\nexit 0\n").unwrap();
        std::fs::set_permissions(&ok, std::fs::Permissions::from_mode(0o700)).unwrap();

        let thumb = render_thumbnail_in(&cache, &ok, &pdf).unwrap();
        assert_eq!(std::fs::read(&thumb).unwrap(), b"PNG-THUMB");
        let pages = render_pages_in(&cache, &ok, &pdf).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(std::fs::read(&pages[0]).unwrap(), b"PNG-P1");
        assert_eq!(std::fs::read(&pages[1]).unwrap(), b"PNG-P2");
        assert!(cache.join(cache_key(&pdf)).join(".done").exists());
    }
}
