//! "Start Overlook when you log in": an XDG autostart entry in `~/.config/autostart`, which
//! Omarchy (through uwsm and systemd's autostart generator) and other desktops start at
//! login. The entry itself is the setting: there is no preference to fall out of step with it.
//! It runs `overlook --background`, which starts without a window when "Keep running when
//! the window is closed" is on and opens the window otherwise.

use std::path::{Path, PathBuf};

const FILE_NAME: &str = "ai.lastrefuge.Overlook.desktop";

fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

fn entry_path() -> Option<PathBuf> {
    Some(config_home()?.join("autostart").join(FILE_NAME))
}

/// Whether Overlook starts at login.
pub fn enabled() -> bool {
    entry_path().is_some_and(|p| is_enabled_entry(&p))
}

/// Adds or removes the entry.
pub fn set_enabled(on: bool) -> std::io::Result<()> {
    let path = entry_path().ok_or_else(|| std::io::Error::other("no home folder"))?;
    if on {
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
        let tmp = path.with_extension("desktop.tmp");
        std::fs::write(&tmp, entry(&crate::updater::installed_exe()))?;
        std::fs::rename(&tmp, &path)
    } else {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

/// An entry another tool switched off (`Hidden=true` or `X-GNOME-Autostart-enabled=false`)
/// counts as off.
fn is_enabled_entry(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else { return false };
    !text.lines().map(str::trim).any(|l| {
        l.eq_ignore_ascii_case("Hidden=true") || l.eq_ignore_ascii_case("X-GNOME-Autostart-enabled=false")
    })
}

fn entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Overlook\n\
         Comment=Start Overlook at login\n\
         Exec={} --background\n\
         Icon=ai.lastrefuge.Overlook\n\
         Terminal=false\n\
         NoDisplay=true\n\
         X-GNOME-Autostart-enabled=true\n",
        exec_quote(&exe.to_string_lossy())
    )
}

/// Quotes a path for a desktop entry's `Exec` key when it needs it.
fn exec_quote(path: &str) -> String {
    if path.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c)) {
        return path.to_owned();
    }
    let mut out = String::from("\"");
    for c in path.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    // The desktop entry format unescapes `\\` once more before `Exec` is parsed.
    out.replace('\\', "\\\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_starts_overlook_in_the_background() {
        let text = entry(Path::new("/usr/bin/overlook"));
        assert!(text.contains("\nExec=/usr/bin/overlook --background\n"), "{text}");
        assert!(text.starts_with("[Desktop Entry]\n"));
    }

    #[test]
    fn unusual_paths_are_quoted_for_exec() {
        assert_eq!(exec_quote("/home/a/.local/bin/overlook"), "/home/a/.local/bin/overlook");
        assert_eq!(exec_quote("/home/a b/overlook"), "\"/home/a b/overlook\"");
        assert_eq!(exec_quote("/x/$y"), "\"/x/\\\\$y\"");
    }

    #[test]
    fn an_entry_switched_off_elsewhere_reads_as_off() {
        let dir = std::env::temp_dir().join(format!("overlook-autostart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let on = dir.join("on.desktop");
        std::fs::write(&on, entry(Path::new("/usr/bin/overlook"))).unwrap();
        assert!(is_enabled_entry(&on));
        let off = dir.join("off.desktop");
        std::fs::write(&off, entry(Path::new("/usr/bin/overlook")).replace("enabled=true", "enabled=false")).unwrap();
        assert!(!is_enabled_entry(&off));
        assert!(!is_enabled_entry(&dir.join("missing.desktop")));
        let _ = std::fs::remove_dir_all(dir);
    }
}
