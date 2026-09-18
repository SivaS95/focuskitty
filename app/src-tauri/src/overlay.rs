//! Native window tweaks Tauri does not expose.
//!
//! Tauri's `alwaysOnTop` puts a window at the floating level, which still sits
//! *below* the Dock and vanishes over a fullscreen app. A desktop pet has to
//! outrank both. The constants here are the ones already proven on this machine
//! by `~/wizardfly/wizardfly.swift:164-170`.

#[cfg(target_os = "macos")]
pub fn make_overlay(window: &tauri::WebviewWindow) -> anyhow::Result<()> {
    use objc2_app_kit::{NSWindow, NSWindowCollectionBehavior};

    // NSScreenSaverWindowLevel. Above the Dock, above fullscreen apps.
    const SCREEN_SAVER_LEVEL: isize = 1000;

    let ptr = window
        .ns_window()
        .map_err(|e| anyhow::anyhow!("no ns_window: {e}"))?;
    if ptr.is_null() {
        anyhow::bail!("ns_window was null");
    }

    unsafe {
        let ns: &NSWindow = &*(ptr as *const NSWindow);
        ns.setLevel(SCREEN_SAVER_LEVEL);

        // CanJoinAllSpaces  - follow the user between Spaces instead of being
        //                     stranded on the one it was born in.
        // FullScreenAuxiliary - allowed to float over a fullscreen app.
        // Stationary        - do not slide around during Mission Control.
        ns.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary,
        );

        // Never steal focus: clicking the cat must not pull you out of your editor.
        ns.setIgnoresMouseEvents(false);
    }

    Ok(())
}

/// Windows: keep the cat out of Alt+Tab, and stop it ever taking focus.
///
/// `alwaysOnTop` and `skipTaskbar` get most of the way, but two things they do
/// not do would each break something real:
///
/// * **Alt+Tab.** `skipTaskbar` removes the taskbar button and nothing else, so
///   the cat would sit in the switcher as though it were an application you
///   might want to work in. `WS_EX_TOOLWINDOW` removes it from both.
///
/// * **Focus.** Clicking the cat opens the quick controls -- and a click on an
///   ordinary window ACTIVATES it. FocusKitty would become the foreground
///   window, so the probe would answer "you are using FocusKitty", the timer
///   for whatever you were actually doing would stop, and a close arriving in
///   that moment would refuse because the browser was no longer in front.
///   `WS_EX_NOACTIVATE` lets the click land without the window ever coming
///   forward, which is what `acceptFirstMouse` plus an accessory activation
///   policy buys on macOS.
#[cfg(target_os = "windows")]
pub fn make_overlay(window: &tauri::WebviewWindow) -> anyhow::Result<()> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    };

    let raw = window.hwnd().map_err(|e| anyhow::anyhow!("no hwnd: {e}"))?;
    // Rebuilt from the raw pointer on purpose: Tauri and this crate need not
    // agree on which windows-rs version's HWND they mean, and the two types
    // are unrelated as far as the compiler is concerned.
    let hwnd = HWND(raw.0 as *mut std::ffi::c_void);

    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let want = ex | WS_EX_TOOLWINDOW.0 as isize | WS_EX_NOACTIVATE.0 as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want);
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn make_overlay(_window: &tauri::WebviewWindow) -> anyhow::Result<()> {
    Ok(())
}

/// Force a window's level. Tauri's `alwaysOnTop` is not granular enough to put
/// the popover above an overlay that already sits at screen-saver level.
#[cfg(target_os = "macos")]
pub fn set_level(window: &tauri::WebviewWindow, level: isize) -> anyhow::Result<()> {
    use objc2_app_kit::NSWindow;
    let ptr = window.ns_window().map_err(|e| anyhow::anyhow!("no ns_window: {e}"))?;
    if ptr.is_null() {
        anyhow::bail!("ns_window was null");
    }
    unsafe {
        let ns: &NSWindow = &*(ptr as *const NSWindow);
        ns.setLevel(level);
    }
    Ok(())
}

/// Park a window just under the tray icon, clamped onto the screen.
pub fn place_popover(
    window: &tauri::WebviewWindow,
    near: tauri::PhysicalPosition<f64>,
) -> anyhow::Result<()> {
    use tauri::{PhysicalPosition, PhysicalSize};

    let size: PhysicalSize<u32> = window.outer_size()?;
    let monitor = window.current_monitor()?.ok_or_else(|| anyhow::anyhow!("no monitor"))?;
    let m = monitor.size();
    let scale = monitor.scale_factor();

    let w = size.width as f64;
    let h = size.height as f64;
    let margin = 8.0 * scale;

    // Horizontal: centre on the anchor, then pull back onto the display.
    let mut x = near.x - w / 2.0;
    x = x.max(margin).min((m.width as f64 - w - margin).max(margin));

    // Vertical: prefer below the anchor, but a popover opened from a cat parked
    // at the bottom of the screen would hang off it with no way to reach the
    // buttons. Flip above, then clamp, so it is always fully on screen.
    let mut y = near.y + 6.0 * scale;
    if y + h > m.height as f64 - margin {
        y = near.y - h - 6.0 * scale;
    }
    y = y.max(margin).min((m.height as f64 - h - margin).max(margin));

    window.set_position(PhysicalPosition::new(x, y))?;
    Ok(())
}
