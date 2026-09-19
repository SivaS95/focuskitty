//! FocusKitty.
//!
//! Three surfaces over one tracker:
//!   cat      - a transparent, always-on-top overlay; the animal itself
//!   popover  - compact controls, from the tray icon or by clicking the cat
//!   main     - the full window: rules, timers, the diary
//!
//! The macOS probe drives browsers through OSAScript, which is main-thread
//! only, so every tick is marshalled onto the main thread rather than run on a
//! worker. At ~400ns when no browser is frontmost, that costs nothing.

mod overlay;
mod tracking;

use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

use fk_core::activity::ActivityProbe;
use fk_core::rules::{BackgroundMode, SiteRule};
use fk_core::store;
#[cfg(desktop)]
use tauri::menu::{Menu, MenuItem};
#[cfg(desktop)]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, State, WebviewWindow};
use tracking::{AppState, Snapshot};

/// Set when the user genuinely asks to quit.
///
/// The app normally survives every window closing -- the cat *is* the app -- so
/// `ExitRequested` is vetoed. Without this flag that veto also swallowed the
/// Quit menu item, leaving no way to actually stop it.
static QUITTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static CAT_VISIBLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Debounce for opening the controls. `Instant` has no const constructor, so
/// this starts empty rather than being conjured out of a transmute.
static LAST_TOGGLE: std::sync::Mutex<Option<std::time::Instant>> =
    std::sync::Mutex::new(None);

/// The cat's opaque region inside its window, in logical pixels.
///
/// A click-through window never delivers `mousemove` to the page, so the page
/// cannot hit-test itself. Instead it reports where the drawn cat is, once, and
/// Rust polls the global cursor against that box.
static HIT_BOX: std::sync::Mutex<(f64, f64, f64, f64)> =
    std::sync::Mutex::new((90.0, 40.0, 130.0, 190.0));

thread_local! {
    /// The probe is neither Send nor Sync, so it lives on the main thread and
    /// nowhere else. A thread_local makes that structural rather than a comment.
    static PROBE: RefCell<Option<Box<dyn ActivityProbe>>> = const { RefCell::new(None) };
}

fn with_probe<R>(f: impl FnOnce(&dyn ActivityProbe) -> R) -> Option<R> {
    PROBE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            #[cfg(target_os = "macos")]
            {
                *slot = Some(Box::new(fk_probe_macos::MacProbe::new()));
            }
            #[cfg(target_os = "android")]
            {
                *slot = Some(Box::new(fk_probe_android::AndroidProbe::new()));
            }
            #[cfg(target_os = "windows")]
            {
                *slot = Some(Box::new(fk_probe_windows::WinProbe::new()));
            }
        }
        slot.as_deref().map(f)
    })
}

// --- commands ---------------------------------------------------------------

#[tauri::command]
fn snapshot(state: State<'_, Arc<AppState>>) -> Snapshot {
    state.snapshot()
}

/// Toggle click-through. The overlay is a rectangle; only the drawn cat should
/// catch the pointer, so the page hit-tests and calls this as you move.
#[cfg(desktop)]
#[tauri::command]
fn set_click_through(window: WebviewWindow, ignore: bool) {
    let _ = window.set_ignore_cursor_events(ignore);
}

/// The page tells us where the cat actually is inside the window.
#[cfg(desktop)]
#[tauri::command]
fn set_hit_box(x: f64, y: f64, w: f64, h: f64) {
    *HIT_BOX.lock().unwrap() = (x, y, w, h);
}

#[cfg(desktop)]
#[tauri::command]
fn begin_drag(window: WebviewWindow) {
    let _ = window.start_dragging();
}

/// Open the controls BESIDE the cat, never over it.
#[cfg(desktop)]
#[tauri::command]
fn cat_clicked(app: AppHandle) {
    let Some(cat) = app.get_webview_window("cat") else { return };
    let (Ok(pos), Ok(size)) = (cat.outer_position(), cat.outer_size()) else { return };
    let scale = cat.scale_factor().unwrap_or(1.0);

    // Popover is 300 logical wide; sit it to whichever side has room, with a
    // gap, so it never covers the animal you just clicked on.
    let pop_w = 300.0 * scale;
    let gap = 12.0 * scale;
    let screen_w = cat
        .current_monitor()
        .ok()
        .flatten()
        .map(|m| m.size().width as f64)
        .unwrap_or(1920.0);

    let left_of = pos.x as f64 - pop_w - gap;
    let right_of = pos.x as f64 + size.width as f64 + gap;
    let x = if left_of > 0.0 { left_of } else { right_of.min(screen_w - pop_w) };

    // place_popover centres on the x it is given, so aim at the chosen edge.
    let near = PhysicalPosition::new(x + pop_w / 2.0, pos.y as f64);
    toggle_popover(&app, near);
}

#[tauri::command]
fn open_main(app: AppHandle) {
    #[cfg(desktop)]
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.set_focus();
    }
    #[cfg(desktop)]
    if let Some(p) = app.get_webview_window("popover") {
        let _ = p.hide();
    }
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
}

#[cfg(desktop)]
#[tauri::command]
fn close_popover(app: AppHandle) {
    if let Some(p) = app.get_webview_window("popover") {
        let _ = p.hide();
    }
}

#[tauri::command]
fn pause(state: State<'_, Arc<AppState>>, minutes: i64) {
    let mut inner = state.inner.lock().unwrap();
    inner.paused_until = if minutes <= 0 {
        None
    } else {
        Some(chrono::Local::now() + chrono::Duration::minutes(minutes))
    };
}

#[tauri::command]
fn set_sleeping(state: State<'_, Arc<AppState>>, sleeping: bool) {
    state.inner.lock().unwrap().sleeping = sleeping;
}

#[tauri::command]
fn snooze(state: State<'_, Arc<AppState>>, label: String, minutes: u64) {
    let key = fk_core::rules::TargetKey::site(&label);
    let mut tracker = state.tracker.lock().unwrap();
    tracker.snooze(&key, minutes * 60);
}

/// Add whatever is in the front tab right now, in one tap.
///
/// Reads the cached activity rather than probing: commands run on worker
/// threads and the macOS probe is main-thread only, so probing here silently
/// returned nothing every time.
#[tauri::command]
fn watch_current(state: State<'_, Arc<AppState>>, minutes: u64) -> Option<String> {
    // Use the last activity that HAD a tab: by the time this command runs the
    // popover has focus, so "what is frontmost" is FocusKitty itself.
    let activity = {
        let inner = state.inner.lock().unwrap();
        inner
            .last_tab
            .clone()
            .or_else(|| inner.current_activity.clone())?
    };
    let tab = activity.tab?;
    let domain = fk_core::domain::normalize(&tab.url);
    if domain.is_empty() {
        return None;
    }

    let mut tracker = state.tracker.lock().unwrap();
    tracker
        .config
        .sites
        .retain(|r| fk_core::domain::canonical(&r.domain) != domain);
    tracker.config.sites.push(SiteRule {
        domain: domain.clone(),
        daily_limit_secs: minutes * 60,
        session_limit_secs: None,
        background_mode: BackgroundMode::ForegroundNotice,
        warn_lead_secs: 300,
        notice_after_secs: 1800,
        enabled: true,
    });
    let _ = store::save_config(&tracker.config);
    Some(domain)
}

/// Watch whatever is in front, whether that is a tab or a native app.
///
/// "Watch this site" could only ever add a website, so an app like a terminal
/// or a chat client simply could not be limited from the quick controls.
#[tauri::command]
fn watch_current_app(state: State<'_, Arc<AppState>>, minutes: u64) -> Option<String> {
    let activity = state.inner.lock().unwrap().last_counted.clone()?;
    if activity.tab.is_some() {
        return None; // that is a site; watch_current handles it
    }
    let mut tracker = state.tracker.lock().unwrap();
    tracker
        .config
        .apps
        .retain(|r| !r.app_id.eq_ignore_ascii_case(&activity.app_id));
    tracker.config.apps.push(fk_core::rules::AppRule {
        app_id: activity.app_id.clone(),
        app_name: activity.app_name.clone(),
        daily_limit_secs: minutes.clamp(1, 24 * 60) * 60,
        session_limit_secs: None,
        warn_lead_secs: 300,
        enabled: true,
    });
    let _ = store::save_config(&tracker.config);
    Some(activity.app_name)
}

#[tauri::command]
fn remove_rule(state: State<'_, Arc<AppState>>, label: String) {
    let mut tracker = state.tracker.lock().unwrap();
    tracker.config.sites.retain(|r| !r.domain.eq_ignore_ascii_case(&label));
    tracker.config.apps.retain(|r| !r.app_name.eq_ignore_ascii_case(&label));
    let _ = store::save_config(&tracker.config);
}

#[tauri::command]
fn add_site(state: State<'_, Arc<AppState>>, domain: String, minutes: u64, mode: String) {
    let background_mode = match mode.as_str() {
        "foreground-only" => BackgroundMode::ForegroundOnly,
        "open-anywhere" => BackgroundMode::OpenAnywhere,
        _ => BackgroundMode::ForegroundNotice,
    };
    // Whatever was typed -- a URL, a host, extra whitespace -- becomes a host.
    let domain = fk_core::domain::normalize(&domain);
    if domain.is_empty() {
        return;
    }
    let mut tracker = state.tracker.lock().unwrap();
    tracker
        .config
        .sites
        .retain(|r| fk_core::domain::canonical(&r.domain) != domain);
    tracker.config.sites.push(SiteRule {
        domain,
        daily_limit_secs: minutes * 60,
        session_limit_secs: None,
        background_mode,
        warn_lead_secs: 300,
        notice_after_secs: 1800,
        enabled: true,
    });
    let _ = store::save_config(&tracker.config);
}

/// Change an existing limit without having to remove and re-add the rule.
#[tauri::command]
fn set_limit(state: State<'_, Arc<AppState>>, label: String, minutes: u64) {
    let secs = minutes.clamp(1, 24 * 60) * 60;
    let mut tracker = state.tracker.lock().unwrap();
    for r in tracker.config.sites.iter_mut() {
        if r.domain.eq_ignore_ascii_case(&label) {
            r.daily_limit_secs = secs;
        }
    }
    for r in tracker.config.apps.iter_mut() {
        if r.app_name.eq_ignore_ascii_case(&label) || r.app_id.eq_ignore_ascii_case(&label) {
            r.daily_limit_secs = secs;
        }
    }
    let _ = store::save_config(&tracker.config);
}

/// Put a daily limit on a native app.
#[tauri::command]
fn add_app(state: State<'_, Arc<AppState>>, app_id: String, app_name: String, minutes: u64) {
    if app_id.trim().is_empty() {
        return;
    }
    let mut tracker = state.tracker.lock().unwrap();
    tracker.config.apps.retain(|r| !r.app_id.eq_ignore_ascii_case(&app_id));
    tracker.config.apps.push(fk_core::rules::AppRule {
        app_id,
        app_name,
        daily_limit_secs: minutes.clamp(1, 24 * 60) * 60,
        session_limit_secs: None,
        warn_lead_secs: 300,
        enabled: true,
    });
    let _ = store::save_config(&tracker.config);
}

#[tauri::command]
fn list_apps(state: State<'_, Arc<AppState>>) -> Vec<fk_core::activity::AppInfo> {
    state.inner.lock().unwrap().apps.clone()
}

/// Make the cat do something on demand, from the popover.
/// The overlay reporting what it actually receives.
///
/// Guessing at IPC plumbing from the outside wasted several rounds; this makes
/// the webview say out loud which events reach it.
#[tauri::command]
fn debug_ping(msg: String) {
    tracing::info!("[cat] {msg}");
}

#[tauri::command]
fn cat_do(app: AppHandle, action: String) {
    // Global emit: the same path the snapshot uses. A window-targeted emit_to
    // did not reach the overlay, which is why every Play button did nothing.
    if let Err(e) = app.emit("fk://do", action) {
        tracing::warn!("cat_do emit: {e}");
    }
}

/// Hide or show the overlay without stopping the tracker.
#[cfg(desktop)]
#[tauri::command]
fn set_cat_visible(app: AppHandle, visible: bool) {
    if let Some(cat) = app.get_webview_window("cat") {
        let _ = if visible { cat.show() } else { cat.hide() };
    }
    CAT_VISIBLE.store(visible, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(desktop)]
#[tauri::command]
fn cat_visible() -> bool {
    CAT_VISIBLE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Has Android granted Usage Access? Without it nothing can be tracked, and an
/// ungranted phone looks exactly like an idle one.
#[cfg(target_os = "android")]
#[tauri::command]
fn usage_access() -> bool {
    fk_probe_android::has_usage_access()
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
fn usage_access() -> bool {
    true
}

/// Open the Settings page where the user can grant it.
#[cfg(target_os = "android")]
#[tauri::command]
fn request_usage_access() {
    if let Err(e) = fk_probe_android::open_usage_settings() {
        tracing::warn!("opening usage settings: {e}");
    }
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
fn request_usage_access() {}

/// Can the cat float over other apps yet?
#[cfg(target_os = "android")]
#[tauri::command]
fn overlay_allowed() -> bool {
    fk_probe_android::can_draw_overlay()
}
#[cfg(not(target_os = "android"))]
#[tauri::command]
fn overlay_allowed() -> bool { true }

#[cfg(target_os = "android")]
#[tauri::command]
fn request_overlay() {
    if let Err(e) = fk_probe_android::request_overlay() {
        tracing::warn!("overlay permission: {e}");
    }
}
#[cfg(not(target_os = "android"))]
#[tauri::command]
fn request_overlay() {}

/// Is the cat actually on screen right now?
#[cfg(target_os = "android")]
#[tauri::command]
fn overlay_running() -> bool {
    fk_probe_android::overlay_running()
}
#[cfg(not(target_os = "android"))]
#[tauri::command]
fn overlay_running() -> bool { false }

/// Put the cat on screen, or take it away.
#[cfg(target_os = "android")]
#[tauri::command]
fn set_overlay(on: bool) {
    if let Err(e) = fk_probe_android::set_overlay_running(on) {
        tracing::warn!("overlay service: {e}");
    }
}
#[cfg(not(target_os = "android"))]
#[tauri::command]
fn set_overlay(_on: bool) {}

#[tauri::command]
fn quit(app: AppHandle, state: State<'_, Arc<AppState>>) {
    // Save the day before going, so quitting never costs the last minute.
    {
        let tracker = state.tracker.lock().unwrap();
        let inner = state.inner.lock().unwrap();
        let _ = store::save_day(&tracker, &inner.events);
    }
    QUITTING.store(true, std::sync::atomic::Ordering::Relaxed);
    // Windows are closed explicitly: on macOS an overlay can otherwise linger
    // for a beat after exit, which looks like the cat refusing to leave.
    #[cfg(desktop)]
    for label in ["cat", "popover", "main"] {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.hide();
        }
    }
    app.exit(0);
}

// --- popover ----------------------------------------------------------------

#[cfg(desktop)]
fn toggle_popover(app: &AppHandle, near: PhysicalPosition<f64>) {
    // Both the tray and the cat can ask for this, and a context menu can fire
    // more than once. Without a debounce the panel strobes open and shut.
    {
        let mut last = LAST_TOGGLE.lock().unwrap();
        if last.is_some_and(|t| t.elapsed() < Duration::from_millis(350)) {
            return;
        }
        *last = Some(std::time::Instant::now());
    }

    let Some(p) = app.get_webview_window("popover") else { return };
    if p.is_visible().unwrap_or(false) {
        let _ = p.hide();
        return;
    }
    let _ = overlay::place_popover(&p, near);
    let _ = p.show();
    let _ = p.set_focus();
}

// --- setup ------------------------------------------------------------------

/// Android loads this library through JNI, and the entry point has to be
/// declared for it. Without the macro the .so builds fine and then fails
/// validation for missing runtime symbols.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // To a FILE, not to stdout. A desktop app has no console attached -- on
    // Windows there is not even one to attach to -- so everything written to
    // stdout has been going nowhere. Three crashes were reported with no
    // record of any of them, which is why they were diagnosed by guessing.
    let log_path = fk_core::store::data_dir().join("focuskitty.log");
    let _ = std::fs::create_dir_all(fk_core::store::data_dir());
    {
        let path = log_path.clone();
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "info".into()),
            )
            .with_target(false)
            .with_ansi(false)
            .with_writer(move || {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .unwrap_or_else(|_| std::fs::File::create("focuskitty.log").unwrap())
            })
            .init();
    }

    // A panic in a background thread kills only that thread, silently: the
    // clock would simply stop with nothing to show for it. Write it down.
    {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let where_ = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_else(|| "unknown".into());
            let what = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".into());
            tracing::error!(
                "PANIC in {} at {where_}: {what}",
                std::thread::current().name().unwrap_or("unnamed")
            );
            previous(info);
        }));
    }
    tracing::info!("FocusKitty starting; log at {}", log_path.display());

    // Android dictates where an app may write, so the store must be told
    // before anything tries to load a config from the wrong place.
    #[cfg(target_os = "android")]
    if let Some(dir) = fk_probe_android::files_dir() {
        fk_core::store::set_data_dir(std::path::PathBuf::from(dir).join("FocusKitty"));
    }

    let state = Arc::new(AppState::new().expect("loading config"));

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(state.clone());

    // The desktop build exposes the overlay commands; the mobile build has no
    // cursor, no tray and no second window, so those simply do not exist there.
    #[cfg(desktop)]
    let builder = builder.invoke_handler(tauri::generate_handler![
        snapshot, set_click_through, begin_drag, cat_clicked, open_main,
        close_popover, pause, set_sleeping, snooze, watch_current,
        remove_rule, add_site, list_apps, quit, set_hit_box, cat_do,
        set_cat_visible, cat_visible, debug_ping, set_limit, add_app, watch_current_app,
        usage_access, request_usage_access, overlay_allowed, request_overlay, set_overlay, overlay_running
    ]);
    #[cfg(mobile)]
    let builder = builder.invoke_handler(tauri::generate_handler![
        snapshot, pause, set_sleeping, snooze, watch_current,
        remove_rule, add_site, list_apps, quit, cat_do,
        debug_ping, set_limit, add_app, watch_current_app, usage_access, request_usage_access, overlay_allowed, request_overlay, set_overlay, overlay_running
    ]);

    builder
        .setup(move |app| {
            let handle = app.handle().clone();

            // No Dock icon, no app switcher entry: a focus companion that
            // clutters the switcher is working against itself.
            #[cfg(target_os = "macos")]
            let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            #[cfg(desktop)]
            if let Some(cat) = app.get_webview_window("cat") {
                if let Err(e) = overlay::make_overlay(&cat) {
                    tracing::warn!("overlay setup: {e}");
                }
                let _ = cat.set_ignore_cursor_events(true);
                // Park bottom-right by default, out of the way of real work.
                if let Ok(Some(mon)) = cat.current_monitor() {
                    let m = mon.size();
                    let s = cat.outer_size().unwrap_or(tauri::PhysicalSize::new(300, 250));
                    let _ = cat.set_position(PhysicalPosition::new(
                        (m.width - s.width) as f64 - 40.0,
                        (m.height - s.height) as f64 - 90.0,
                    ));
                }
                let _ = cat.show();
            }

            #[cfg(desktop)]
            // Hide the popover as soon as it loses focus, the way a popover should.
            if let Some(p) = app.get_webview_window("popover") {
                // One level above the cat, or the overlay draws on top of the
                // controls -- which is exactly what it did at first.
                #[cfg(target_os = "macos")]
                let _ = overlay::set_level(&p, 1001);
                let pc = p.clone();
                p.on_window_event(move |e| {
                    if let tauri::WindowEvent::Focused(false) = e {
                        let _ = pc.hide();
                    }
                });
            }

            #[cfg(desktop)]
            // Closing the full window should hide it, not quit the app.
            if let Some(m) = app.get_webview_window("main") {
                let mc = m.clone();
                let h = handle.clone();
                m.on_window_event(move |e| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                        api.prevent_close();
                        let _ = mc.hide();
                        #[cfg(target_os = "macos")]
                        let _ = h.set_activation_policy(tauri::ActivationPolicy::Accessory);
                    }
                });
            }

            #[cfg(desktop)]
            {
                build_tray(app.handle())?;
                spawn_cursor_watch(handle.clone());
            }
            spawn_tick(handle.clone(), state.clone());

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("building FocusKitty")
        .run(|_app, event| {
            // Keep running with every window closed -- the cat is the app -- but
            // never veto a quit the user actually asked for.
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                if !QUITTING.load(std::sync::atomic::Ordering::Relaxed) {
                    api.prevent_exit();
                }
            }
        });
}

#[cfg(desktop)]
fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open FocusKitty…", true, None::<&str>)?;
    let hide = MenuItem::with_id(app, "hide_cat", "Hide / show the cat", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit FocusKitty", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &hide, &quit_item])?;

    TrayIconBuilder::with_id("tray")
        .icon(app.default_window_icon().unwrap().clone())
        .icon_as_template(true)
        .tooltip("FocusKitty")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => open_main(app.clone()),
            "hide_cat" => {
                let on = CAT_VISIBLE.load(std::sync::atomic::Ordering::Relaxed);
                set_cat_visible(app.clone(), !on);
            }
            "quit" => {
                QUITTING.store(true, std::sync::atomic::Ordering::Relaxed);
                if let Some(c) = app.get_webview_window("cat") { let _ = c.hide(); }
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            // Left click opens the compact controls; the menu is on right click.
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } = event
            {
                let pos = rect.position;
                let near = match pos {
                    tauri::Position::Physical(p) => {
                        PhysicalPosition::new(p.x as f64, p.y as f64 + 24.0)
                    }
                    tauri::Position::Logical(p) => PhysicalPosition::new(p.x, p.y + 24.0),
                };
                toggle_popover(tray.app_handle(), near);
            }
        })
        .build(app)?;
    Ok(())
}

/// Walk the cat over to the tab, swipe it shut, and walk back.
///
/// The cat sitting in the corner while a tab silently vanished never read as
/// cause and effect. It now travels to where the tab actually is -- Chrome
/// gives us the window bounds and which tab is active, which is enough to
/// reconstruct roughly where the tab sits -- strikes it, and returns.
#[cfg(desktop)]
/// A few lengths of the screen, then settle.
///
/// A cat does not cross a room once and stop. It goes back and forth at a
/// brisk trot a handful of times and then decides it is done, sits down, and
/// gets on with something else -- so this walks two to four legs, alternating
/// direction, and leaves the settling to whatever the tracker picks next.
///
/// The rig walks ON THE SPOT throughout (`driven`), because the overlay window
/// is only ~340 wide: left to its own devices the rig travels about 30 pixels,
/// hits its own edge and turns round, which halfway across the desk reads as
/// the cat walking backwards.
#[cfg(desktop)]
fn stroll(app: AppHandle, frac: f64) {
    use tauri::LogicalPosition;

    let Some(cat) = app.get_webview_window("cat") else { return };
    let Ok(pos) = cat.outer_position() else { return };
    let scale = cat.scale_factor().unwrap_or(2.0);
    let start = LogicalPosition::new(pos.x as f64 / scale, pos.y as f64 / scale);

    let Ok(Some(mon)) = cat.current_monitor() else { return };
    let width = mon.size().width as f64 / scale;
    let cat_w = cat.outer_size().map(|s| s.width as f64 / scale).unwrap_or(170.0);
    let span = (width - cat_w).max(1.0);

    // Where the legs turn: near each edge, but not pinned to it.
    let left = span * 0.04;
    let right = span * 0.94;
    let legs = 2 + (frac * 3.0) as u32; // 2..4, from the same roll that chose the walk

    std::thread::spawn(move || {
        let _ = app.emit("fk://busy", true);
        let mut from = start;

        for leg in 0..legs {
            // Head for whichever end is further away, then alternate.
            let to_x = if (leg == 0 && from.x > span / 2.0) || leg % 2 == 1 { left } else { right };
            let dist = (to_x - from.x).abs();
            if dist < span * 0.15 {
                continue;
            }

            let _ = app.emit("fk://face", if to_x >= from.x { 1 } else { -1 });
            std::thread::sleep(Duration::from_millis(280)); // let it turn first
            let _ = app.emit("fk://do", "walk");

            // A trot, not a stroll: about 420 points a second.
            let steps = ((dist / 6.7) as u64).clamp(30, 300);
            for i in 1..=steps {
                let t = i as f64 / steps as f64;
                // Eased only at the very ends of a leg, so the middle of the
                // walk holds a steady pace instead of drifting.
                let e = if t < 0.15 {
                    let u = t / 0.15;
                    0.15 * u * u
                } else if t > 0.85 {
                    let u = (1.0 - t) / 0.15;
                    1.0 - 0.15 * u * u
                } else {
                    t
                };
                let _ = cat.set_position(LogicalPosition::new(
                    from.x + (to_x - from.x) * e,
                    from.y,
                ));
                std::thread::sleep(Duration::from_millis(16));
            }
            from = LogicalPosition::new(to_x, from.y);
            // A breath at the turn, the way an animal checks before doubling back.
            let _ = app.emit("fk://do", "sit");
            std::thread::sleep(Duration::from_millis(340));
        }

        let _ = app.emit("fk://do", "sit");
        let _ = app.emit("fk://busy", false);
    });
}

fn go_and_swipe(
    app: AppHandle,
    state: Arc<AppState>,
    job: tracking::PendingClose,
    rect: Option<fk_core::activity::TabRect>,
) {
    use tauri::LogicalPosition;

    let Some(cat) = app.get_webview_window("cat") else { return };
    let Ok(home) = cat.outer_position() else { return };
    let scale = cat.scale_factor().unwrap_or(2.0);
    let home = LogicalPosition::new(home.x as f64 / scale, home.y as f64 / scale);

    // Where to stand. The strip hugs the top of the screen, so the cat parks
    // just under it rather than trying to stand somewhere off-screen.
    let target = rect.map(|r| {
        let (tx, ty) = r.active_tab_center();
        LogicalPosition::new((tx - 170.0).max(0.0), (ty - 26.0).max(0.0))
    });

    std::thread::spawn(move || {
        let step = Duration::from_millis(16);

        // Walk: horizontal only, at whatever height the cat is already at.
        // Sliding diagonally across the screen is what made it look like it
        // was flying. Cats walk along a surface, then jump between surfaces.
        let walk = |from: LogicalPosition<f64>, to_x: f64| {
            let dist = (to_x - from.x).abs();
            if dist < 8.0 {
                return;
            }
            let _ = app.emit("fk://face", if to_x >= from.x { 1 } else { -1 });
            let _ = app.emit("fk://do", "walk");
            let steps = ((dist * 1.9) as u64 / 16).clamp(18, 110);
            for i in 1..=steps {
                let t = i as f64 / steps as f64;
                let e = t * t * (3.0 - 2.0 * t);
                let _ = cat.set_position(LogicalPosition::new(
                    from.x + (to_x - from.x) * e,
                    from.y,
                ));
                std::thread::sleep(step);
            }
        };

        // Pounce: a crouch, then an arc that decelerates at the top the way a
        // jump does, with a small overshoot so it settles onto the target.
        let pounce = |from: LogicalPosition<f64>, to: LogicalPosition<f64>| {
            let _ = app.emit("fk://do", "land"); // crouch to load the jump
            std::thread::sleep(Duration::from_millis(220));
            let _ = app.emit("fk://do", "leap");

            let steps: u64 = 34;
            let over = if to.y < from.y { 34.0 } else { 14.0 };
            for i in 1..=steps {
                let t = i as f64 / steps as f64;
                // Fast off the ground, slowing as it rises.
                let e = 1.0 - (1.0 - t).powi(3);
                // Overshoot past the target, then drop back onto it.
                let arc = -over * (std::f64::consts::PI * t).sin();
                let _ = cat.set_position(LogicalPosition::new(
                    from.x + (to.x - from.x) * e,
                    from.y + (to.y - from.y) * e + arc,
                ));
                std::thread::sleep(step);
            }
            let _ = app.emit("fk://do", "land");
            std::thread::sleep(Duration::from_millis(180));
        };

        if let Some(to) = target {
            walk(home, to.x);
            pounce(LogicalPosition::new(to.x, home.y), to);
        }

        // Strike, and shut the tab on the impact frame.
        let _ = app.emit("fk://do", "swipe");
        std::thread::sleep(Duration::from_millis(420));

        let app_c = app.clone();
        let state_c = state.clone();

        // Same story as the tick, and for two reasons rather than one.
        //
        // The main thread is the window event loop, which on Windows sleeps
        // until a message arrives -- so the close was posted to something that
        // was not listening. The cat walked and swiped, because that runs
        // here, and then nothing was closed.
        //
        // And even if it had run there it could not have worked: UI Automation
        // is reached through COM, and the crate asks for a multi-threaded
        // apartment. A GUI main thread is already single-threaded, so that
        // request is REFUSED and every UIA call fails. Reading the address --
        // which is how the close re-verifies it is shutting the right tab --
        // can only work off the main thread.
        let finish = move || {
            use fk_core::activity::CloseOutcome;
            let (label, outcome, verb) = match job {
                tracking::PendingClose::Tab(label, tab) => {
                    (label, with_probe(|p| p.close_tab(&tab)), "closed")
                }
                tracking::PendingClose::App(label, name) => {
                    (label, with_probe(|p| p.hide_app(&name)), "hid")
                }
            };
            let mut inner = state_c.inner.lock().unwrap();
            match outcome {
                Some(Ok(CloseOutcome::Closed)) => {
                    inner.closed_at.insert(label.clone(), chrono::Local::now());
                    inner.say = Some(format!("{verb} {label}"));
                    inner.events.push(format!("{verb} {label}"));
                }
                Some(Ok(CloseOutcome::NoLongerMatching)) => {
                    inner.events.push(format!("{label} moved on; nothing done"));
                }
                Some(Ok(CloseOutcome::Unsupported)) => {
                    inner.say = Some(format!("{label} is over its limit"));
                }
                Some(Err(e)) => tracing::warn!("acting on {label}: {e}"),
                None => {}
            }
            drop(inner);
            let _ = app_c.emit("fk://snapshot", &state_c.snapshot());
        };

        #[cfg(target_os = "macos")]
        let _ = app.run_on_main_thread(finish);
        #[cfg(not(target_os = "macos"))]
        finish();

        // Look pleased with itself, then drop back down and walk home.
        std::thread::sleep(Duration::from_millis(650));
        if let Some(to) = target {
            pounce(to, LogicalPosition::new(to.x, home.y));
            walk(LogicalPosition::new(to.x, home.y), home.x);
        }
        let _ = app.emit("fk://do", "sit");
        let _ = app.emit("fk://busy", false);
    });
}

/// Watch the global cursor.
///
/// Two jobs at once: decide whether the overlay should swallow the pointer, and
/// feed the cat's gaze. Both need the cursor even when it is nowhere near the
/// window, which the page itself can never see.
#[cfg(desktop)]
fn spawn_cursor_watch(app: AppHandle) {
    std::thread::spawn(move || {
        let mut was_over = None::<bool>;
        let mut last_at = None::<f64>;
        let mut logged = false;
        let mut logged_mon = false;
        let mut beat: u32 = 0;
        loop {
            std::thread::sleep(Duration::from_millis(40)); // 25 Hz is plenty
            let Some(cat) = app.get_webview_window("cat") else { continue };

            let cursor = app.cursor_position();
            if !logged {
                logged = true;
                match &cursor {
                    Ok(p) => tracing::info!("cursor tracking live at {:.0},{:.0}", p.x, p.y),
                    Err(e) => tracing::error!("cursor_position unavailable: {e}"),
                }
            }
            let (Ok(pos), Ok(origin), Ok(scale)) =
                (cursor, cat.outer_position(), cat.scale_factor())
            else { continue };

            // Cursor relative to the window, in logical pixels.
            let rx = (pos.x - origin.x as f64) / scale;
            let ry = (pos.y - origin.y as f64) / scale;

            // Leave the flag alone while the controls are open: every flip makes
            // the window server re-evaluate pointer ownership, which blurs the
            // popover, and the popover hides on blur. That was the flicker.
            let popover_up = app
                .get_webview_window("popover")
                .and_then(|p| p.is_visible().ok())
                .unwrap_or(false);

            let (hx, hy, hw, hh) = *HIT_BOX.lock().unwrap();
            let over = rx >= hx && rx <= hx + hw && ry >= hy && ry <= hy + hh;

            // Only touch the flag on a change: setting it every frame fights
            // the window server and makes clicks land unpredictably.
            if !popover_up && was_over != Some(over) {
                was_over = Some(over);
                let _ = cat.set_ignore_cursor_events(!over);
            }

            // Gaze, normalised against the cat's head inside the window.
            // Global emit, the same path the snapshot uses and is known to work.
            let _ = app.emit("fk://cursor", (rx, ry));

            // Where the cat is sitting, 0..1 across the display. Parked at the
            // right edge it should face left, back across the desktop, rather
            // than staring off the side of the screen.
            let mon_res = cat.current_monitor();
            if mon_res.is_err() && !logged_mon {
                logged_mon = true;
                tracing::error!("current_monitor unavailable: placement disabled");
            }
            if let Ok(Some(mon)) = mon_res {
                let mw = mon.size().width as f64;
                let cw = cat.outer_size().map(|s| s.width as f64).unwrap_or(0.0);
                if mw > 1.0 {
                    let at = ((origin.x as f64 + cw / 2.0) / mw).clamp(0.0, 1.0);
                    // On change, but also on a slow heartbeat: the first emit
                    // lands before the webview has registered its listener, and
                    // a cat that has not moved would otherwise never be told
                    // which way to face.
                    beat = beat.wrapping_add(1);
                    let moved = last_at.map_or(true, |p: f64| (p - at).abs() > 0.01);
                    if moved || beat % 50 == 0 {
                        if last_at.is_none() {
                            tracing::info!("cat sits at {:.2} across the display", at);
                        }
                        last_at = Some(at);
                        let _ = app.emit("fk://place", at);
                    }
                }
            }
        }
    });
}

/// One tick a second, marshalled onto the main thread for OSAScript's sake.
fn spawn_tick(app: AppHandle, state: Arc<AppState>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        let state = state.clone();
        let app2 = app.clone();

        // Where this runs is not a detail -- it is the difference between a
        // clock and a stopwatch somebody has to keep tapping.
        //
        // `ActivityProbe` is not Send because of macOS: OSAScript is
        // main-thread-only, so there the work must be marshalled onto the main
        // thread. Windows has no such requirement, and marshalling there was
        // actively harmful: the main thread is the window event loop, which
        // sleeps until a message arrives. With nothing to deliver -- nobody
        // clicking, no windows moving -- the posted work simply waited. The
        // timer stopped, limits never expired, and nothing was ever closed,
        // until the user touched something and woke the loop, which looked
        // exactly like the timer only counting while being watched.
        let work = move || {
            with_probe(|probe| tracking::tick(&state, probe));
            // A close is the app's whole point. Play the swipe now and shut
            // the tab on its impact frame, so the gesture and the consequence
            // are the same event rather than two unrelated ones.
            // A walk the cat decided to take on its own. Taken before the
            // close, and skipped if a close is waiting: the two would fight
            // over the same window position.
            #[cfg(desktop)]
            {
                // Not while the controls are open -- the cat would walk out
                // from under the panel you are using -- and not while a close
                // is queued, since the two would fight over the position.
                let busy = app2
                    .get_webview_window("popover")
                    .and_then(|p| p.is_visible().ok())
                    .unwrap_or(false);
                let wander = {
                    let mut inner = state.inner.lock().unwrap();
                    if inner.pending_close.is_some() || busy {
                        inner.pending_wander = None;
                        None
                    } else {
                        inner.pending_wander.take()
                    }
                };
                if let Some(frac) = wander {
                    stroll(app2.clone(), frac);
                }
            }

            let pending = state.inner.lock().unwrap().pending_close.take();
            #[cfg(desktop)]
            if let Some(job) = pending {
                // Where to go? Asked here, on the main thread, while we still
                // know which window is focused.
                let rect = match &job {
                    tracking::PendingClose::Tab(..) => {
                        with_probe(|p| p.active_tab_rect()).flatten()
                    }
                    tracking::PendingClose::App(_, name) => {
                        let n = name.clone();
                        with_probe(|p| p.app_window_rect(&n)).flatten()
                    }
                };
                let _ = app2.emit("fk://busy", true);
                go_and_swipe(app2.clone(), state.clone(), job, rect);
            }

            let snap = state.snapshot();
            if let Err(e) = app2.emit("fk://snapshot", &snap) {
                tracing::warn!("emitting snapshot: {e}");
            }
        };

        #[cfg(target_os = "macos")]
        let _ = app.run_on_main_thread(work);
        // Everywhere else, run it right here. This thread wakes on its own.
        #[cfg(not(target_os = "macos"))]
        work();
    });
}
