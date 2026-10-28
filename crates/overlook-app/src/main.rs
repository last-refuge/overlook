//! Overlook — a clear view of what matters.

mod assets;
mod backup;
mod browser_engine;
mod browser_body;
mod credentials;
mod caldav_sync;
mod google_signin;
mod notify;
mod persist;
mod watchdog;
mod malware_scan;
mod pdf_preview;
mod single_instance;
mod updater;
mod providers;
mod sync;
mod theme;
mod ui;

use gpui_kit::component::TitleBar;
use gpui_kit::*;
use overlook_core::time::now;
use overlook_core::*;
use overlook_provider::sample::{sample_mailbox, SampleProvider};
use overlook_store::Store;
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

pub const APP_ID: &str = "ai.lastrefuge.Overlook";

static INBOX_LIMIT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(100);

/// Inbox download size from preferences; set at start-up and whenever it changes.
pub fn store_prefs_inbox_limit() -> usize {
    INBOX_LIMIT.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn set_inbox_limit(limit: usize) {
    INBOX_LIMIT.store(limit, std::sync::atomic::Ordering::Relaxed);
}
pub const SETTING_APPEARANCE: &str = "appearance";

/// When the process started, for the startup timings logged at info level.
pub static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

pub fn since_start_ms() -> u128 {
    STARTED.get().map(|t| t.elapsed().as_millis()).unwrap_or(0)
}

fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("OVERLOOK_DATA_DIR") {
        return PathBuf::from(dir);
    }
    directories::ProjectDirs::from("ai", "Last Refuge", "Overlook")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".overlook"))
}

/// Creates the sample account with the fictional mailbox already downloaded.
pub fn seed_sample_account(store: &Store) -> anyhow::Result<Account> {
    let account = Account {
        id: AccountId::from("sample"),
        provider: ProviderKind::Sample,
        display_name: "Jason Alexander".into(),
        email: "jason@example.com".into(),
        created_at: now(),
    };
    store.upsert_account(&account)?;
    let snapshot = sample_mailbox(&account, now());
    for label in &snapshot.labels {
        store.upsert_label(label)?;
    }
    store.upsert_messages(&snapshot.messages)?;
    store.set_checkpoint(&account.id, snapshot.checkpoint.as_deref())?;
    store.set_sync_state(&account.id, &SyncState::Offline, None)?;
    info!("seeded sample account");
    Ok(account)
}

/// Opens the profile database and makes sure at least one account exists so the shell
/// always has local mail to work with offline.
fn open_store() -> anyhow::Result<(Arc<Store>, Vec<Account>)> {
    let dir = data_dir();
    let store = Store::open(&dir.join("overlook.sqlite"))?;
    let mut accounts = store.accounts()?;
    if accounts.is_empty() {
        accounts.push(seed_sample_account(&store)?);
    }
    Ok((Arc::new(store), accounts))
}

/// Logs to stderr and to `overlook.log` in the data folder (the previous log is kept as
/// `overlook.log.old` once it passes 4 MiB), so problems can be traced after the fact.
fn init_logging() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::Layer;
    let filter = log_filter;
    let path = data_dir().join("overlook.log");
    restrict_to_owner(&data_dir());
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 4 * 1024 * 1024) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    let file = open_private_log(&path).ok();
    let registry = tracing_subscriber::registry().with(tracing_subscriber::fmt::layer().with_filter(tracing_subscriber::filter::filter_fn(safe_log_metadata)).with_filter(filter()));
    match file {
        Some(file) => registry
            .with(tracing_subscriber::fmt::layer().with_ansi(false).with_writer(std::sync::Mutex::new(file)).with_filter(tracing_subscriber::filter::filter_fn(safe_log_metadata)).with_filter(filter()))
            .init(),
        None => registry.init(),
    }
}

/// The profile (mail cache, logs, attachments) is readable by its owner only: 0700. An
/// existing folder created with looser permissions by an older version is tightened too.
fn restrict_to_owner(dir: &std::path::Path) {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir);
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
}

/// Opens the log for appending with owner-only permissions (0600), tightening an older,
/// world-readable file as well.
fn open_private_log(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let file = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

/// Release builds log Overlook's own targets at info and ignore `RUST_LOG`, so verbose
/// diagnostics (`overlook::diag` and other debug/trace events) cannot be switched on in
/// production. Development builds honour `RUST_LOG` for troubleshooting.
fn log_filter() -> tracing_subscriber::EnvFilter {
    if cfg!(debug_assertions) {
        tracing_subscriber::EnvFilter::from_default_env().add_directive("overlook=info".parse().unwrap())
    } else {
        tracing_subscriber::EnvFilter::new(RELEASE_LOG_FILTER)
    }
}

const RELEASE_LOG_FILTER: &str = "overlook=info";

/// Release builds replace the default panic output (message, thread name, optional
/// backtrace) with one fixed line on stderr. The location and message go only to the log.
fn install_panic_hook() {
    if cfg!(debug_assertions) {
        return;
    }
    std::panic::set_hook(Box::new(|info| {
        let location = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let message = info.payload().downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| info.payload().downcast_ref::<String>().cloned()).unwrap_or_default();
        tracing::error!(target: "overlook::panic", location, "internal error: {message}");
        eprintln!("{PANIC_NOTICE}");
    }));
}

const PANIC_NOTICE: &str = "Overlook hit an internal error. Details were written to overlook.log in the data folder.";

/// Protocol and vault diagnostics can contain wire bytes or secrets. An RUST_LOG
/// override must never enable them in either the file log or stderr.
fn safe_log_metadata(metadata: &tracing::Metadata<'_>) -> bool {
    !["ureq", "rustls", "lettre", "imap", "oo7", "zbus", "zvariant"].iter().any(|prefix| metadata.target().starts_with(prefix))
}

#[cfg(test)]
mod security_tests {
    use super::safe_log_metadata;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::{layer::SubscriberExt, Layer};

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { self.0.lock().unwrap().extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }

    #[test]
    fn log_and_profile_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("overlook-log-perms-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let log = dir.join("overlook.log");
        std::fs::write(&log, b"older entry\n").unwrap();
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).unwrap();
        super::restrict_to_owner(&dir);
        super::open_private_log(&log).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(&log).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn release_filter_ignores_rust_log_and_stops_at_info() {
        let filter = tracing_subscriber::EnvFilter::new(super::RELEASE_LOG_FILTER);
        let captured = Capture(Arc::new(Mutex::new(Vec::new())));
        let writer = captured.clone();
        let subscriber = tracing_subscriber::registry().with(tracing_subscriber::fmt::layer().without_time().with_ansi(false).with_writer(move || writer.clone()).with_filter(tracing_subscriber::filter::filter_fn(safe_log_metadata)).with_filter(filter));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "overlook::sync", "visible-info");
            tracing::debug!(target: "overlook::diag", "hidden-debug-detail");
            tracing::trace!(target: "overlook::signin", "hidden-trace");
            tracing::info!(target: "overlook_provider::imap", "visible-provider-info");
            tracing::debug!(target: "overlook_provider::smtp", "hidden-provider-debug");
            tracing::info!(target: "gpui::window", "hidden-foreign-info");
        });
        let bytes = captured.0.lock().unwrap().clone();
        let output = String::from_utf8(bytes).unwrap();
        assert!(output.contains("visible-info") && output.contains("visible-provider-info"), "{output}");
        assert!(!output.contains("hidden"), "{output}");
        assert!(!super::PANIC_NOTICE.contains('{'));
    }

    #[test]
    fn synthetic_login_logs_redact_credentials_even_with_trace_enabled() {
        let captured = Capture(Arc::new(Mutex::new(Vec::new())));
        let writer = captured.clone();
        let subscriber = tracing_subscriber::registry().with(tracing_subscriber::fmt::layer().without_time().with_ansi(false).with_writer(move || writer.clone()).with_filter(tracing_subscriber::filter::filter_fn(safe_log_metadata)).with_filter(tracing_subscriber::EnvFilter::new("trace")));
        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!(target: "ureq_proto::client", "synthetic-wire-secret");
            tracing::trace!(target: "lettre::smtp", "synthetic-smtp-secret");
            tracing::trace!(target: "oo7::dbus", "synthetic-vault-secret");
            let tokens = overlook_provider::gmail::OAuthTokens { access_token: "synthetic-access-secret".into(), refresh_token: Some("synthetic-refresh-secret".into()), expires_at: 3600000, scope: None, signed_in_at: 0 };
            tracing::info!(target: "overlook::synthetic_login", ?tokens, "Synthetic login credential diagnostic (not a live Google login)");
        });
        let output = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        assert!(output.contains("[REDACTED]"));
        assert!(!output.contains("-secret"));
        println!("{output}");
    }
}

fn main() {
    STARTED.get_or_init(std::time::Instant::now);
    // Plain command-line conveniences; anything else starts the app.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mailto = args.iter().find(|a| a.to_ascii_lowercase().starts_with("mailto:")).cloned();
    if let Some(link) = &mailto {
        // A running Overlook takes the link; this launch is done.
        if single_instance::send(link) {
            return;
        }
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("overlook {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Overlook {} — a clear view of what matters.\n\nUsage: overlook [mailto:…] [--version] [--help]\n\nEnvironment:\n  OVERLOOK_DATA_DIR      profile directory (default ~/.local/share/overlook)\n  OVERLOOK_CONFIG_DIR    directory holding google-oauth-client.json (default ~/.config/overlook)\n  OVERLOOK_SIGN_IN=gmail start the Google sign-in at launch (imap / pop3: open that account setup)\n  OVERLOOK_BROWSER_BIN  compatible Chromium executable (default PATH discovery){}", env!("CARGO_PKG_VERSION"), if cfg!(debug_assertions) { "\n  RUST_LOG=overlook=debug tracing output (development builds only)" } else { "" });
        return;
    }
    init_logging();
    install_panic_hook();

    let (store, accounts) = match open_store() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Overlook could not open its local database: {e}");
            std::process::exit(1);
        }
    };
    tracing::debug!("startup: store open after {} ms", since_start_ms());
    single_instance::listen();
    if let Some(link) = mailto {
        single_instance::push(link);
    }
    let provider = SampleProvider::new();
    // The sample account's connection is simulated. Start it connected when asked, so a
    // restart-while-offline test can be followed by a reconnect at launch.
    if std::env::var("OVERLOOK_SAMPLE_ONLINE").is_ok_and(|v| v == "1") {
        provider.set_online(true);
    }

    gpui_kit::application().with_assets(assets::Assets).run(move |cx| {
        tracing::debug!("startup: app running after {} ms", since_start_ms());
        gpui_kit::init(cx);
        let mut fonts: Vec<Cow<'static, [u8]>> = assets::INTER_FACES.iter().map(|f| Cow::Borrowed(*f)).collect();
        fonts.push(Cow::Borrowed(assets::SOURCE_SERIF_DISPLAY));
        if let Err(e) = cx.text_system().add_fonts(fonts) {
            tracing::warn!("bundled fonts could not be loaded: {e}");
        }
        tracing::debug!("startup: fonts loaded after {} ms", since_start_ms());

        if let Ok(Some(prefs)) = store.setting::<overlook_core::Prefs>("prefs") {
            set_inbox_limit(prefs.inbox_messages as usize);
            overlook_core::tz::set_clock_24h(prefs.clock_24h);
        }
        let preference: AppearancePreference = store
            .setting(SETTING_APPEARANCE)
            .ok()
            .flatten()
            .unwrap_or_default();
        theme::apply_preference(preference, theme::system_appearance(cx.window_appearance()), cx);
        ui::init(cx);
        tracing::debug!("startup: theme and keys ready after {} ms", since_start_ms());

        let bounds = Bounds::centered(None, size(px(1320.), px(920.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            app_id: Some(APP_ID.into()),
            window_min_size: Some(size(px(880.), px(560.))),
            ..TitleBar::window_options()
        };
        let active: AccountId = store
            .setting::<AccountId>("active_account")
            .ok()
            .flatten()
            .filter(|id| accounts.iter().any(|a| &a.id == id))
            .unwrap_or_else(|| accounts[0].id.clone());
        let store = store.clone();
        let accounts = accounts.clone();
        let provider = provider.clone();
        gpui_kit::open_window(options, cx, move |window, cx| {
            cx.new(|cx| ui::MailWindow::new(store, provider, accounts, active, preference, window, cx))
        })
        .expect("failed to open the Overlook window");
        cx.activate(true);
    });
}
