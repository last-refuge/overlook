//! Overlook's life apart from its windows (issue #15).
//!
//! The app quits only when asked (`QuitMode::Explicit`); this module decides when. Closing the
//! last window quits, unless "Keep running when the window is closed" is on: then GPUI switches
//! to headless mode (no display connection at all) and the main window opens again there. It
//! draws nothing, but its sync, IDLE, reminders, new-mail notifications and update checks carry
//! on, since they all live in the main window. A later launch, or a click on one of its
//! notifications, switches back to the launch's desktop and opens the window again. Quit (Ctrl+Q,
//! the command palette, `overlook --quit`) ends the process.
//!
//! Every request from a later launch arrives here too (`serve_launches`), so one is served even
//! while the window is hidden, or when only a composer window is open.

use crate::single_instance::{LaunchEnv, Request};
use crate::ui::{MailWindow, WindowSetup};
use gpui_kit::component::TitleBar;
use gpui_kit::*;
use overlook_core::{AccountId, AppearancePreference};
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

struct Lifecycle {
    store: Arc<Store>,
    sample: SampleProvider,
    /// The main window, shown or (while `hidden`) headless.
    main: Option<Main>,
    hidden: bool,
    /// A switch between headless and windowed is under way; windows closing meanwhile are ours.
    switching: bool,
    /// A show request that arrived during a switch.
    show_after: Option<LaunchEnv>,
    /// Requests waiting for the window to be shown: a mailto link while hidden.
    after_show: Vec<Request>,
    /// Composer windows, which save their draft before Quit ends the process.
    composers: Vec<(AnyWindowHandle, WeakEntity<MailWindow>)>,
    replaced_noticed: bool,
}

impl Global for Lifecycle {}

/// Opens the main window (or, when `hidden`, starts headless with it) and starts serving
/// later launches. GPUI must already be in the matching mode.
pub fn start(store: Arc<Store>, sample: SampleProvider, hidden: bool, cx: &mut App) {
    cx.set_global(Lifecycle {
        store,
        sample,
        main: None,
        hidden,
        switching: false,
        show_after: None,
        after_show: Vec::new(),
        composers: Vec::new(),
        replaced_noticed: false,
    });
    cx.on_window_closed(window_closed).detach();
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
}

/// A composer window to give the chance to save before Quit.
pub fn add_composer(window: AnyWindowHandle, editor: WeakEntity<MailWindow>, cx: &mut App) {
    if cx.has_global::<Lifecycle>() {
        cx.global_mut::<Lifecycle>().composers.push((window, editor));
    }
}

fn open_main(hidden: bool, cx: &mut App) -> anyhow::Result<()> {
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
    let preference: AppearancePreference = store.setting(crate::SETTING_APPEARANCE).ok().flatten().unwrap_or_default();
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
        cx.new(|cx| MailWindow::new(store, sample, accounts, active, WindowSetup::Mail(preference), window, cx))
    })?;
    cx.global_mut::<Lifecycle>().main = Some(Main { window, view: view.downgrade() });
    Ok(())
}

fn window_closed(cx: &mut App, closed: WindowId) {
    let none_left = cx.windows().is_empty();
    if !cx.has_global::<Lifecycle>() {
        return;
    }
    let l = cx.global_mut::<Lifecycle>();
    l.composers.retain(|(w, _)| w.window_id() != closed);
    if l.main.as_ref().is_some_and(|m| m.window.window_id() == closed) {
        l.main = None;
    }
    if l.switching || !none_left {
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
            if let Some(env) = l.show_after.take() {
                show(env, cx);
            }
        });
    })
    .detach();
}

/// Brings Overlook forward on the launch's desktop: the main window, opened again if it was
/// hidden or closed.
pub fn show(env: LaunchEnv, cx: &mut App) {
    let l = cx.global_mut::<Lifecycle>();
    if l.switching {
        l.show_after = Some(env);
        return;
    }
    if !l.hidden {
        let main = l.main.as_ref().map(|m| m.window);
        // Only a composer is open: the main window comes back beside it.
        let window = match main {
            Some(window) => Some(window),
            None => open_main(false, cx).map_err(|e| tracing::error!("could not open the window: {e:#}")).ok().and_then(|()| cx.global::<Lifecycle>().main.as_ref().map(|m| m.window)),
        };
        if let Some(window) = window {
            let _ = window.update(cx, |_, window, _| window.activate_window());
        }
        serve_after_show(cx);
        return;
    }
    l.switching = true;
    // The headless window goes; its replacement on the desktop starts afresh from the store.
    if let Some(main) = l.main.take() {
        let _ = main.window.update(cx, |_, window, _| window.remove_window());
    }
    let mut desktop = GraphicalEnvironment::detect();
    for (name, slot) in [("WAYLAND_DISPLAY", &mut desktop.wayland_display), ("DISPLAY", &mut desktop.x11_display), ("XDG_RUNTIME_DIR", &mut desktop.xdg_runtime_dir)] {
        if let Some(value) = env.get(name) {
            *slot = Some(value.into());
        }
    }
    desktop.activation_token = env.get("XDG_ACTIVATION_TOKEN").map(str::to_owned);
    cx.spawn(async move |cx| {
        let switched = cx.update(|cx| cx.request_windowing(WindowingRequest::Windowed(desktop))).await;
        cx.update(|cx| {
            let opened = switched.and_then(|()| {
                crate::browser_engine::set_prewarm(true);
                open_main(false, cx)
            });
            let l = cx.global_mut::<Lifecycle>();
            l.switching = false;
            match opened {
                Ok(()) => {
                    l.hidden = false;
                    tracing::info!("window shown");
                    cx.activate(true);
                    serve_after_show(cx);
                }
                Err(e) => {
                    // Still headless: carry on there.
                    tracing::error!("could not show the window: {e:#}");
                    crate::browser_engine::set_prewarm(false);
                    if let Err(e) = open_main(true, cx) {
                        tracing::error!("could not keep running without a window, so quitting: {e:#}");
                        quit_now(cx);
                    }
                }
            }
        });
    })
    .detach();
}

/// Quit: each open composer saves its draft as closing its window would, and one that can't
/// close yet (an attachment still loading, a save that failed) keeps Overlook open.
pub fn quit(cx: &mut App) {
    let composers = cx.try_global::<Lifecycle>().map(|l| l.composers.clone()).unwrap_or_default();
    for (window, editor) in composers {
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
    // A launch from now on starts a new Overlook rather than handing itself to this one.
    crate::single_instance::release();
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
        if wakeups.recv().await.is_none() {
            break;
        }
    })
    .detach();
}

fn serve(request: Request, cx: &mut App) {
    let l = cx.global_mut::<Lifecycle>();
    match request {
        Request::Show(env) => show(env, cx),
        Request::Quit => quit(cx),
        // While hidden there's nothing to repaint; the window reads the theme when it opens.
        Request::ReloadTheme if l.hidden || l.switching => {}
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
