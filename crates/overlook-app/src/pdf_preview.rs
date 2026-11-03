//! PDF page rendering for the quick look and attachment thumbnails, through poppler's
//! `pdftoppm`. Renders are cached under the app's cache directory, keyed by the file's
//! path, size and modification time, so a document is rasterised once.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const MAX_PAGES: usize = 40;
const PAGE_WIDTH_PX: u32 = 1400;
const THUMB_WIDTH_PX: u32 = 144;

/// Whether page rendering is possible on this machine.
pub fn available() -> bool {
    which("pdftoppm")
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH").map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file())).unwrap_or(false)
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
    let dir = crate::updater::cache_dir().join("pdf").join(cache_key(pdf));
    let done = dir.join(".done");
    if !done.exists() {
        std::fs::create_dir_all(&dir)?;
        let prefix = dir.join("page");
        let status = Command::new("pdftoppm")
            .arg("-png")
            .arg("-f").arg("1")
            .arg("-l").arg(MAX_PAGES.to_string())
            .arg("-scale-to-x").arg(PAGE_WIDTH_PX.to_string())
            .arg("-scale-to-y").arg("-1")
            .arg(pdf)
            .arg(&prefix)
            // Poppler reports a damaged file on stderr; the exit status says enough.
            .stderr(Stdio::null())
            .status()?;
        if !status.success() {
            return Err(std::io::Error::other(format!("pdftoppm exited with {status}")));
        }
        std::fs::write(&done, b"")?;
    }
    Ok(numbered_pngs(&dir, "page"))
}

/// A small first-page image for the attachment card.
pub fn render_thumbnail(pdf: &Path) -> std::io::Result<PathBuf> {
    let dir = crate::updater::cache_dir().join("pdf").join(cache_key(pdf));
    let out = dir.join("thumb.png");
    if out.exists() {
        return Ok(out);
    }
    std::fs::create_dir_all(&dir)?;
    let status = Command::new("pdftoppm")
        .arg("-png")
        .arg("-singlefile")
        .arg("-f").arg("1")
        .arg("-l").arg("1")
        .arg("-scale-to").arg(THUMB_WIDTH_PX.to_string())
        .arg(pdf)
        .arg(dir.join("thumb"))
        .stderr(Stdio::null())
        .status()?;
    if !status.success() || !out.exists() {
        return Err(std::io::Error::other("pdftoppm produced no thumbnail"));
    }
    Ok(out)
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
}
