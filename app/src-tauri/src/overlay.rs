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

#[cfg(not(target_os = "macos"))]
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
