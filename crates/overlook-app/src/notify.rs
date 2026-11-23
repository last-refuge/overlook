//! Desktop notifications.
//!
//! On Omarchy they go through `omarchy-notification-send`, so they look like the rest of
//! the desktop (a glyph instead of an icon) and clicking one focuses Overlook. Elsewhere
//! they use the plain freedesktop notification service. Nothing here blocks the UI
//! thread, and failures are logged only: the in-app toast is always shown as well.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

/// How important a notification is; picks the glyph and urgency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Success,
    Warning,
    Error,
    Mail,
    Reminder,
}

/// Whether in-app messages are mirrored to Omarchy's notifications (the preference).
static MIRROR: AtomicBool = AtomicBool::new(false);

pub fn set_mirror(on: bool) {
    MIRROR.store(on, Ordering::Relaxed);
}

/// Omarchy's notification command, when this desktop is Omarchy.
fn omarchy_sender() -> Option<&'static PathBuf> {
    static SENDER: OnceLock<Option<PathBuf>> = OnceLock::new();
    SENDER
        .get_or_init(|| {
            let on_path = std::env::var_os("PATH")
                .map(|paths| std::env::split_paths(&paths).map(|d| d.join("omarchy-notification-send")).find(|p| p.is_file()))
                .unwrap_or_default();
            on_path.or_else(|| Some(PathBuf::from("/usr/share/omarchy/bin/omarchy-notification-send")).filter(|p| p.is_file()))
        })
        .as_ref()
}

/// True on Omarchy: the settings show the mirroring option only there.
pub fn omarchy_available() -> bool {
    omarchy_sender().is_some()
}

/// Mirrors an in-app message to the desktop when the preference is on and this is
/// Omarchy. Plain information (confirmations such as "Archived") stays in the app.
pub fn mirror(level: Level, text: &str) {
    if level != Level::Info && MIRROR.load(Ordering::Relaxed) && omarchy_available() {
        let (headline, body) = split(text);
        send(level, headline, body);
    }
}

/// Shows a desktop notification (Omarchy's when available).
pub fn desktop(level: Level, headline: String, body: String) {
    send(level, headline, body);
}

/// A long message becomes a short headline and a description.
fn split(text: &str) -> (String, String) {
    if text.chars().count() <= 60 {
        return (text.to_owned(), String::new());
    }
    match text.find(". ").or_else(|| text.find(": ")) {
        Some(i) if i < 80 => (text[..=i].trim_end_matches(['.', ':']).to_owned(), text[i + 1..].trim().to_owned()),
        _ => ("Overlook".to_owned(), text.to_owned()),
    }
}

fn glyph(level: Level) -> &'static str {
    match level {
        Level::Info => "\u{f05a}",          // info circle
        Level::Success => "\u{f00c}",       // check
        Level::Warning => "\u{f071}",       // warning triangle
        Level::Error => "\u{f057}",         // times circle
        Level::Mail => "\u{f01ee}",         // envelope
        Level::Reminder => "\u{f00f0}",     // calendar clock
    }
}

fn send(level: Level, headline: String, body: String) {
    std::thread::Builder::new()
        .name("overlook-notify".into())
        .spawn(move || {
            if let Some(sender) = omarchy_sender() {
                let urgency = match level {
                    Level::Error | Level::Reminder => "normal",
                    _ => "low",
                };
                // Each value is one argument, never parsed by a shell; --exec comes last.
                let status = Command::new(sender)
                    .args(["--app-name", "Overlook", "-g", glyph(level), "-u", urgency])
                    .args(omarchy_args(level, &headline, &body))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                match status {
                    Ok(s) if s.success() => return,
                    Ok(s) => tracing::warn!("omarchy notification exited with {s}"),
                    Err(e) => tracing::warn!("omarchy notification failed: {e}"),
                }
            }
            let result = notify_rust::Notification::new()
                .appname("Overlook")
                .summary(&headline)
                .body(&body)
                .icon(if level == Level::Reminder { "x-office-calendar" } else { "mail-unread" })
                .timeout(Duration::from_secs(20))
                .show();
            if let Err(e) = result {
                tracing::warn!("desktop notification failed: {e}");
            }
        })
        .ok();
}

/// Headline, description, and a click that focuses Overlook (or starts it).
fn omarchy_args(level: Level, headline: &str, body: &str) -> Vec<String> {
    let mut args = vec![headline.to_owned()];
    if !body.is_empty() {
        args.push(body.to_owned());
    }
    if level == Level::Reminder {
        args.extend(["--exec", "overlook", "--reminders"].map(str::to_owned));
    } else {
        // Match the app's window class so a terminal titled "…omarchy-overlook" can't match it.
        args.extend(["--exec", "omarchy-launch-or-focus", "ai.lastrefuge.Overlook", "overlook"].map(str::to_owned));
    }
    args
}

#[cfg(test)]
mod tests {
    #[test]
    fn long_messages_split_into_headline_and_description() {
        assert_eq!(super::split("Sent: Hello"), ("Sent: Hello".into(), String::new()));
        let (h, b) = super::split("Couldn’t sync: the server refused the request because the account needs to sign in again.");
        assert_eq!(h, "Couldn’t sync");
        assert!(b.starts_with("the server refused"));
    }

    #[test]
    fn the_click_command_comes_last_as_separate_words() {
        let args = super::omarchy_args(super::Level::Mail, "New mail", "From Maya");
        assert_eq!(args, vec!["New mail", "From Maya", "--exec", "omarchy-launch-or-focus", "ai.lastrefuge.Overlook", "overlook"]);
        assert_eq!(super::omarchy_args(super::Level::Info, "Hi", "")[1], "--exec");
        assert_eq!(super::omarchy_args(super::Level::Reminder, "Meeting", "Starts at 3:00 PM"),
            vec!["Meeting", "Starts at 3:00 PM", "--exec", "overlook", "--reminders"]);
    }
}
