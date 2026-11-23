//! Overlook's life apart from its windows (issue #15).
//!
//! The app quits only when asked (`QuitMode::Explicit`); this module decides when. Closing the
//! last window quits, unless "Keep running when the window is closed" is on: then GPUI switches
//! to headless mode (no display connection at all) and the main window opens again there. It
//! draws nothing, but sync, IDLE, reminders, new-mail notifications and update checks carry
//! on. A later launch, or a click on one of its
//! notifications, switches back to the launch's desktop and opens the window again. Quit (Ctrl+Q,
//! the command palette, `overlook --quit`) ends the process.
//!
//! Every request from a later launch arrives here too (`serve_launches`), so one is served even
//! while the window is hidden, or when only a composer window is open. So do the tray icon's,
//! which this module shows for the life of the app (`tray.rs`).

use crate::reminders::{self, Inbox, Reminder};
use crate::single_instance::{LaunchEnv, Request};
use crate::ui::{MailWindow, ReminderWindow, WindowSetup};
use gpui_kit::component::TitleBar;
use gpui_kit::*;
use overlook_core::{Account, AccountId, AppearancePreference, EventId, Prefs};
use overlook_provider::sample::SampleProvider;
use overlook_store::Store;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

static KEEP_RUNNING: AtomicBool = AtomicBool::new(false);

/// Follows the "Keep running when the window is closed" preference.
pub fn set_keep_running(on: bool) {
    KEEP_RUNNING.store(on, Ordering::Relaxed);
}

fn keep_running() -> bool {
    KEEP_RUNNING.load(Ordering::Relaxed)
}

struct Main {
    window: AnyWindowHandle,
    view: WeakEntity<MailWindow>,
}

struct ReminderPanel {
    window: AnyWindowHandle,
    view: WeakEntity<ReminderWindow>,
}

struct Lifecycle {
    store: Arc<Store>,
    sample: SampleProvider,
    /// The main window, shown or (while `hidden`) headless.
    main: Option<Main>,
    /// Keep the latest sync state while a detached editor keeps the app open.
    sync_view: Option<Entity<MailWindow>>,
    hidden: bool,
    /// A switch between headless and windowed is under way; windows closing meanwhile are ours.
    switching: bool,
    /// A show request that arrived during a switch.
    show_after: Option<LaunchEnv>,
    reminders_after: Option<LaunchEnv>,
    // A reminder can be the only visible window. Keep the last usable display to open it.
    desktop: Option<GraphicalEnvironment>,
    reminder_panel: Option<ReminderPanel>,
    reminders: Inbox,
    reminder_prefs: reminders::Preferences,
    reminder_wake: Arc<tokio::sync::Notify>,
    /// Requests waiting for the window to be shown: a mailto link while hidden.
    after_show: Vec<Request>,
    /// Every mail window can own a draft that must be saved before Quit.
    editors: Vec<(AnyWindowHandle, WeakEntity<MailWindow>)>,
    replaced_noticed: bool,
    tray: crate::tray::TrayWorker,
}

impl Global for Lifecycle {}

/// Opens the main window (or, when `hidden`, starts headless with it) and starts serving
/// later launches. GPUI must already be in the matching mode.
pub fn start(store: Arc<Store>, sample: SampleProvider, hidden: bool, cx: &mut App) {
    let saved = store.setting::<Prefs>("prefs").ok().flatten().unwrap_or_default();
    let reminders = Inbox::load(&store);
    cx.set_global(Lifecycle {
        store,
        sample,
        main: None,
        sync_view: None,
        hidden,
        switching: false,
        show_after: None,
        reminders_after: None,
        desktop: cx.graphical_environment(),
        reminder_panel: None,
        reminders,
        reminder_prefs: reminder_preferences(&saved),
        reminder_wake: Arc::new(tokio::sync::Notify::new()),
        after_show: Vec::new(),
        editors: Vec::new(),
        replaced_noticed: false,
        tray: crate::tray::TrayWorker::new(),
    });
    cx.on_window_closed(window_closed).detach();
    cx.on_app_quit(|cx| {
        crate::single_instance::desktop_stopping();
        crate::mcp::stop_desktop();
        if cx.has_global::<Lifecycle>() {
            cx.global_mut::<Lifecycle>().tray.shutdown();
        }
        crate::browser_engine::set_prewarm(false);
        cx.background_executor().spawn(async {
            crate::browser_engine::drain_workers();
            crate::persist::flush();
        })
    }).detach();
    cx.background_executor().spawn(async {
        crate::pdf_preview::prune_cache();
    }).detach();
    if hidden {
        crate::browser_engine::set_prewarm(false);
        tracing::info!("starting in the background");
    }
    open_main(hidden, cx).expect("failed to open the Overlook window");
    if !hidden {
        cx.activate(true);
    }
    serve_launches(cx);
    watch_for_upgrades(cx);
    keep_tray(cx);
    watch_reminders(cx);
}

/// Registers a mail window so its draft gets a chance to save before Quit.
pub fn add_editor(window: AnyWindowHandle, editor: WeakEntity<MailWindow>, cx: &mut App) {
    if cx.has_global::<Lifecycle>() {
        cx.global_mut::<Lifecycle>().editors.push((window, editor));
    }
}

struct MailStartup {
    store: Arc<Store>,
    sample: SampleProvider,
    accounts: Vec<Account>,
    active: Option<AccountId>,
    preference: AppearancePreference,
}

fn mail_startup(cx: &App) -> anyhow::Result<MailStartup> {
    let (store, sample) = {
        let l = cx.global::<Lifecycle>();
        (l.store.clone(), l.sample.clone())
    };
    let accounts = store.accounts()?;
    let active: Option<AccountId> = store
        .setting::<AccountId>("active_account")
        .ok()
        .flatten()
        .filter(|id| accounts.iter().any(|a| &a.id == id))
        .or_else(|| accounts.first().map(|a| a.id.clone()));
    let preference = store.setting(crate::SETTING_APPEARANCE).ok().flatten().unwrap_or_default();
    Ok(MailStartup { store, sample, accounts, active, preference })
}

impl MailStartup {
    fn build(self, setup: WindowSetup, window: &mut Window, cx: &mut App) -> Entity<MailWindow> {
        cx.new(|cx| MailWindow::new(self.store, self.sample, self.accounts, self.active, setup, window, cx))
    }
}

fn open_main(hidden: bool, cx: &mut App) -> anyhow::Result<()> {
    let startup = mail_startup(cx)?;
    let preference = startup.preference;
    if !hidden {
        // The desktop's light or dark setting may have changed while the window was away.
        crate::theme::apply_preference(preference, crate::theme::system_appearance(cx.window_appearance()), cx);
    }
    let bounds = Bounds::centered(None, size(px(1320.), px(920.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        app_id: Some(crate::APP_ID.into()),
        window_min_size: Some(size(px(880.), px(560.))),
        ..TitleBar::window_options()
    };
    let (window, view) = gpui_kit::open_window(options, cx, move |window, cx| {
        startup.build(WindowSetup::Mail(preference), window, cx)
    })?;
    let lifecycle = cx.global_mut::<Lifecycle>();
    lifecycle.sync_view = Some(view.clone());
    lifecycle.main = Some(Main { window, view: view.downgrade() });
    crate::single_instance::wake();
    Ok(())
}

fn window_closed(cx: &mut App, closed: WindowId) {
    let none_left = cx.windows().is_empty();
    if !cx.has_global::<Lifecycle>() {
        return;
    }
    let l = cx.global_mut::<Lifecycle>();
    l.editors.retain(|(w, _)| w.window_id() != closed);
    let closed_main = l.main.as_ref().is_some_and(|m| m.window.window_id() == closed);
    if closed_main {
        l.main = None;
    }
    if l.reminder_panel.as_ref().is_some_and(|panel| panel.window.window_id() == closed) {
        l.reminder_panel = None;
    }
    if none_left {
        l.sync_view = None;
    }
    if l.switching || !none_left {
        if !l.switching && closed_main && l.reminder_panel.is_some() {
            if let Err(error) = start_reminder_sync_owner(cx) {
                tracing::warn!(%error, "could not continue background sync in the reminders window");
            }
        }
        crate::single_instance::wake();
        return;
    }
    if l.hidden || !keep_running() {
        quit_now(cx);
    } else {
        go_hidden(cx);
    }
}

/// The last window closed: carry on without one.
fn go_hidden(cx: &mut App) {
    cx.global_mut::<Lifecycle>().switching = true;
    crate::browser_engine::set_prewarm(false);
    cx.spawn(async move |cx| {
        let switch = cx.update(|cx| cx.graphical_environment().is_some().then(|| cx.request_windowing(WindowingRequest::Headless)));
        let switched = match switch {
            Some(task) => task.await,
            None => Ok(()),
        };
        cx.update(|cx| {
            if let Err(e) = switched.and_then(|()| open_main(true, cx)) {
                tracing::error!("could not keep running without a window, so quitting: {e:#}");
                quit_now(cx);
                return;
            }
            tracing::info!("running in the background");
            let l = cx.global_mut::<Lifecycle>();
            l.hidden = true;
            l.switching = false;
            serve_window_requests(cx);
        });
    })
    .detach();
}

#[cfg(target_os = "macos")]
pub(crate) fn show_settings(page: crate::ui::SettingsPage, cx: &mut App) {
    show(LaunchEnv::default(), cx);
    let lifecycle = cx.global::<Lifecycle>();
    if lifecycle.hidden || lifecycle.switching {
        return;
    }
    if let Some((window, view)) = lifecycle.main.as_ref().and_then(|main| {
        main.view.upgrade().map(|view| (main.window, view))
    }) {
        let _ = window.update(cx, |_, window, cx| {
            view.update(cx, |this, cx| this.open_settings(page, window, cx));
        });
    }
}

/// Brings Overlook forward on the launch's desktop: the main window, opened again if it was
/// hidden or closed.
pub fn show(env: LaunchEnv, cx: &mut App) {
    show_window(WindowTarget::Main, env, cx);
}

pub(crate) fn show_reminders(env: LaunchEnv, cx: &mut App) {
    show_window(WindowTarget::Reminders, env, cx);
}

#[derive(Clone, Copy)]
enum WindowTarget {
    Main,
    Reminders,
}

fn show_window(target: WindowTarget, env: LaunchEnv, cx: &mut App) {
    if !cx.has_global::<Lifecycle>() {
        return;
    }
    let l = cx.global_mut::<Lifecycle>();
    if l.switching {
        match target {
            WindowTarget::Main => l.show_after = Some(env),
            WindowTarget::Reminders => l.reminders_after = Some(env),
        }
        return;
    }
    if !l.hidden {
        if let Err(error) = present(target, cx) {
            tracing::warn!(%error, "could not open the Overlook window");
        }
        return;
    }
    l.switching = true;
    let desktop = l.desktop.clone().unwrap_or_else(GraphicalEnvironment::detect);
    // The headless window goes; its replacement on the desktop starts afresh from the store.
    if let Some(main) = l.main.take() {
        let _ = main.window.update(cx, |_, window, _| window.remove_window());
    }
    #[cfg(target_os = "linux")]
    let desktop = {
        let mut desktop = desktop;
        for (name, slot) in [("WAYLAND_DISPLAY", &mut desktop.wayland_display), ("DISPLAY", &mut desktop.x11_display), ("XDG_RUNTIME_DIR", &mut desktop.xdg_runtime_dir)] {
            if let Some(value) = env.get(name) {
                *slot = Some(value.into());
            }
        }
        desktop.activation_token = env.get("XDG_ACTIVATION_TOKEN").map(str::to_owned);
        desktop
    };
    cx.spawn(async move |cx| {
        let switched = cx.update(|cx| cx.request_windowing(WindowingRequest::Windowed(desktop))).await;
        cx.update(|cx| {
            let opened = switched.and_then(|()| {
                let desktop = cx.graphical_environment();
                let l = cx.global_mut::<Lifecycle>();
                l.desktop = desktop;
                l.hidden = false;
                if matches!(target, WindowTarget::Reminders) {
                    let preference = l.store.setting(crate::SETTING_APPEARANCE).ok().flatten().unwrap_or_default();
                    crate::theme::apply_preference(preference, crate::theme::system_appearance(cx.window_appearance()), cx);
                }
                present(target, cx)
            });
            cx.global_mut::<Lifecycle>().switching = false;
            if let Err(error) = opened {
                tracing::warn!(%error, "could not show the Overlook window");
                go_hidden(cx);
            } else {
                serve_window_requests(cx);
            }
        });
    })
    .detach();
}

fn serve_window_requests(cx: &mut App) {
    let l = cx.global_mut::<Lifecycle>();
    let (main, reminders) = (l.show_after.take(), l.reminders_after.take());
    if let Some(env) = main {
        show(env, cx);
    }
    if let Some(env) = reminders {
        show_reminders(env, cx);
    }
}

fn present(target: WindowTarget, cx: &mut App) -> anyhow::Result<()> {
    let window = match target {
        WindowTarget::Main => {
            let lifecycle = cx.global::<Lifecycle>();
            if lifecycle.main.is_none() {
                crate::browser_engine::set_prewarm(true);
                open_main(false, cx)?;
            }
            cx.global::<Lifecycle>().main.as_ref().map(|m| m.window)
        }
        WindowTarget::Reminders => Some(open_reminder_panel(cx)?),
    };
    if let Some(window) = window {
        let _ = window.update(cx, |_, window, _| window.activate_window());
        cx.activate(true);
    }
    if matches!(target, WindowTarget::Main) {
        serve_after_show(cx);
    }
    Ok(())
}

fn open_reminder_panel(cx: &mut App) -> anyhow::Result<AnyWindowHandle> {
    let lifecycle = cx.global::<Lifecycle>();
    if let Some(panel) = &lifecycle.reminder_panel {
        return Ok(panel.window);
    }
    let due = lifecycle.reminders.due().to_vec();
    let (window, view) = ReminderWindow::open(due, cx)?;
    let lifecycle = cx.global_mut::<Lifecycle>();
    lifecycle.reminder_panel = Some(ReminderPanel { window, view: view.downgrade() });
    if lifecycle.main.is_none() {
        if let Err(error) = start_reminder_sync_owner(cx) {
            tracing::warn!(%error, "could not continue background sync in the reminders window");
        }
    }
    Ok(window)
}

/// While only reminders are visible, use this window for the existing mail sync tasks.
fn start_reminder_sync_owner(cx: &mut App) -> anyhow::Result<()> {
    let panel = cx.global::<Lifecycle>().reminder_panel.as_ref().expect("reminders window exists");
    let (window, view) = (panel.window, panel.view.clone());
    let startup = mail_startup(cx)?;
    let preference = startup.preference;
    let mail = window.update(cx, |_, window, cx| {
        let mail = startup.build(WindowSetup::ReminderService(preference), window, cx);
        let _ = view.update(cx, |view, cx| view.focus(window, cx));
        mail
    })?;
    cx.global_mut::<Lifecycle>().sync_view = Some(mail);
    Ok(())
}

/// Quit: each open composer saves its draft as closing its window would, and one that can't
/// close yet (an attachment still loading, a save that failed) keeps Overlook open.
pub fn quit(cx: &mut App) {
    let editors = cx.try_global::<Lifecycle>().map(|l| l.editors.clone()).unwrap_or_default();
    for (window, editor) in editors {
        let ready = window
            .update(cx, |_, window, cx| editor.update(cx, |e, cx| e.prepare_composer_window_close(window, cx)).unwrap_or(true))
            .unwrap_or(true);
        if !ready {
            let _ = window.update(cx, |_, window, _| window.activate_window());
            return;
        }
    }
    quit_now(cx);
}

fn quit_now(cx: &mut App) {
    tracing::info!("quitting");
    crate::single_instance::desktop_stopping();
    crate::mcp::stop_desktop();
    cx.quit();
}

/// Serves requests from later launches for the life of the app. The listener wakes this task,
/// so nothing polls.
fn serve_launches(cx: &mut App) {
    let mut wakeups = crate::single_instance::wakeups();
    cx.spawn(async move |cx| loop {
        for line in crate::single_instance::take() {
            match Request::parse(&line) {
                Some(request) => {
                    tracing::info!("later launch: {}", request.kind());
                    cx.update(|cx| serve(request, cx));
                }
                None => tracing::warn!("ignored a request from another launch"),
            }
        }
        cx.update(|cx| {
            with_sync_owner(cx, |this, window, cx| {
                let changes = crate::mcp::take_changes();
                if !changes.is_empty() {
                    this.mcp_data_changed(changes, window, cx);
                }
            });
        });
        if wakeups.recv().await.is_none() {
            break;
        }
    })
    .detach();
}

/// Use a live window for the main view's sync work, including while only a
/// detached editor remains open.
pub(crate) fn with_sync_owner(
    cx: &mut App,
    update: impl FnOnce(&mut MailWindow, &mut Window, &mut Context<MailWindow>),
) {
    let Some(lifecycle) = cx.try_global::<Lifecycle>() else { return };
    let Some(view) = lifecycle.sync_view.clone() else { return };
    let windows: Vec<_> = lifecycle.main.as_ref().map(|main| main.window).into_iter()
        .chain(lifecycle.reminder_panel.as_ref().map(|panel| panel.window))
        .chain(lifecycle.editors.iter().map(|(window, _)| *window))
        .collect();
    let mut update = Some(update);
    for window in windows {
        if window.update(cx, |_, window, cx| {
            view.update(cx, |this, cx| update.take().unwrap()(this, window, cx));
        }).is_ok() {
            break;
        }
    }
}

fn serve(request: Request, cx: &mut App) {
    let l = cx.global_mut::<Lifecycle>();
    match request {
        Request::Show(env) => show(env, cx),
        Request::Reminders(env) => show_reminders(env, cx),
        Request::Quit => quit(cx),
        // While hidden there's nothing to repaint; the window reads the theme when it opens.
        Request::ReloadTheme if l.hidden || l.switching => {}
        Request::SyncNow if l.switching => {}
        request @ (Request::SyncNow | Request::ReloadTheme) if l.main.is_none() => {
            with_sync_owner(cx, |this, window, cx| this.serve_launch(request, window, cx));
        }
        Request::Mailto(_) if l.hidden || l.switching || l.main.is_none() => {
            l.after_show.push(request);
            show(LaunchEnv::default(), cx);
        }
        request => serve_in_main(request, cx),
    }
}

fn serve_after_show(cx: &mut App) {
    for request in std::mem::take(&mut cx.global_mut::<Lifecycle>().after_show) {
        serve_in_main(request, cx);
    }
}

fn serve_in_main(request: Request, cx: &mut App) {
    let Some((window, view)) = cx.global::<Lifecycle>().main.as_ref().map(|m| (m.window, m.view.clone())) else { return };
    let _ = window.update(cx, |_, window, cx| view.update(cx, |this, cx| this.serve_launch(request, window, cx)));
}

/// A package upgrade can replace Overlook on disk while it keeps running for days. Hidden, it
/// restarts into the new version at once; with a window, the update banner offers the restart.
fn watch_for_upgrades(cx: &mut App) {
    cx.spawn(async move |cx| loop {
        cx.background_executor().timer(Duration::from_secs(60)).await;
        let noticed = cx.update(|cx| cx.global::<Lifecycle>().replaced_noticed);
        if noticed {
            continue;
        }
        let Some(version) = cx.background_executor().spawn(async { crate::updater::replaced_on_disk() }).await else { continue };
        cx.update(|cx| {
            let l = cx.global_mut::<Lifecycle>();
            if l.hidden && !l.switching {
                tracing::info!("Overlook {version} was installed; restarting into it");
                crate::updater::restart_in_background();
            }
            l.replaced_noticed = true;
            serve_in_main_with(cx, move |this, cx| this.offer_restart_into(version, cx));
        });
    })
    .detach();
}

fn serve_in_main_with(cx: &mut App, f: impl FnOnce(&mut MailWindow, &mut Context<MailWindow>)) {
    if let Some(view) = cx.global::<Lifecycle>().main.as_ref().and_then(|m| m.view.upgrade()) {
        view.update(cx, f);
    }
}

/// Shows the tray icon for the life of the process. D-Bus work runs off the UI thread.
fn keep_tray(cx: &mut App) {
    cx.spawn(async move |cx| loop {
        cx.update(|cx| {
            let unread = cx
                .global::<Lifecycle>()
                .sync_view
                .as_ref()
                .map(|view| view.read(cx).unread_inbox_total());
            cx.global_mut::<Lifecycle>().tray.sync(true, unread);
        });
        cx.background_executor().timer(Duration::from_secs(2)).await;
    })
    .detach();
}

fn reminder_preferences(prefs: &Prefs) -> reminders::Preferences {
    reminders::Preferences { enabled: prefs.event_reminders, default_minutes: prefs.default_reminder_minutes }
}

pub(crate) fn set_reminder_preferences(prefs: &Prefs, cx: &mut App) {
    if cx.has_global::<Lifecycle>() {
        cx.global_mut::<Lifecycle>().reminder_prefs = reminder_preferences(prefs);
    }
}

fn watch_reminders(cx: &mut App) {
    let wake = cx.global::<Lifecycle>().reminder_wake.clone();
    cx.spawn(async move |cx| loop {
        let (store, tracked, prefs) = cx.update(|cx| {
            let l = cx.global::<Lifecycle>();
            (l.store.clone(), l.reminders.tracked(), l.reminder_prefs)
        });
        let now = overlook_core::time::now();
        let result = cx.background_executor().spawn(async move {
            reminders::candidates(&store, tracked, now, prefs)
        }).await;
        let checked_at = overlook_core::time::now();
        cx.update(|cx| match result {
            Ok(candidates) => {
                let l = cx.global_mut::<Lifecycle>();
                if l.reminder_prefs != prefs {
                    return;
                }
                let (changed, due) = l.reminders.refresh(candidates, checked_at, prefs.enabled);
                if changed {
                    l.reminders.save(l.store.clone());
                }
                refresh_reminder_panel(cx);
                if !due.is_empty() {
                    notify_reminders(&due);
                    show_reminders(LaunchEnv::default(), cx);
                }
            }
            Err(error) => tracing::warn!(%error, "could not check calendar reminders"),
        });
        let next_snooze = cx.update(|cx| {
            let l = cx.global::<Lifecycle>();
            if l.reminder_prefs.enabled {
                l.reminders.next_snooze_after(checked_at)
            } else {
                None
            }
        });
        let interval = Duration::from_secs(15);
        let delay = next_snooze.map_or(interval, |deadline| {
            let remaining = deadline.saturating_sub(overlook_core::time::now()).max(0) as u64;
            Duration::from_millis(remaining).min(interval)
        });
        tokio::select! {
            _ = cx.background_executor().timer(delay) => {},
            _ = wake.notified() => {},
        }
    }).detach();
}

fn notify_reminders(due: &[Reminder]) {
    let (headline, body) = if let [reminder] = due {
        let event = &reminder.event;
        let schedule = format!("{} · {}", overlook_core::tz::format_day(event.start), overlook_core::tz::format_clock(event.start));
        let body = event.location.as_deref().filter(|text| !text.trim().is_empty())
            .map_or_else(|| schedule.clone(), |location| format!("{schedule}\n{location}"));
        (event.display_title(), body)
    } else {
        (format!("{} calendar reminders", due.len()), "Open Calendar reminders in Overlook to review them.".into())
    };
    crate::notify::desktop(crate::notify::Level::Reminder, headline, body);
}

fn refresh_reminder_panel(cx: &mut App) {
    let l = cx.global::<Lifecycle>();
    if let Some(panel) = &l.reminder_panel {
        let view = panel.view.clone();
        let due = l.reminders.due().to_vec();
        let _ = view.update(cx, |view, cx| view.update_due(due, cx));
    }
}

pub(crate) fn snooze_reminder(id: EventId, minutes: i64, cx: &mut App) {
    if change_reminders(cx, |inbox| inbox.snooze(&id, overlook_core::time::now(), minutes)) {
        refresh_reminder_panel(cx);
    }
}

pub(crate) fn dismiss_reminders(id: Option<EventId>, cx: &mut App) {
    if !change_reminders(cx, |inbox| inbox.dismiss(id.as_ref())) {
        return;
    }
    let lifecycle = cx.global::<Lifecycle>();
    if lifecycle.reminders.due().is_empty() {
        if let Some(panel) = &lifecycle.reminder_panel {
            let _ = panel.window.update(cx, |_, window, _| window.remove_window());
        }
    } else {
        refresh_reminder_panel(cx);
    }
}

fn change_reminders(cx: &mut App, change: impl FnOnce(&mut Inbox) -> bool) -> bool {
    let lifecycle = cx.global_mut::<Lifecycle>();
    if !change(&mut lifecycle.reminders) {
        return false;
    }
    lifecycle.reminders.save(lifecycle.store.clone());
    lifecycle.reminder_wake.notify_one();
    true
}

pub(crate) fn open_reminder_event(id: EventId, cx: &mut App) {
    show(LaunchEnv::default(), cx);
    let Some((window, view)) = cx.global::<Lifecycle>().main.as_ref().map(|m| (m.window, m.view.clone())) else { return };
    let _ = window.update(cx, |_, window, cx| view.update(cx, |this, cx| this.show_event_in_calendar(id, window, cx)));
}
