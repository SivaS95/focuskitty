//! What the user is doing right now, and the one trait every platform implements.

use serde::{Deserialize, Serialize};

/// A browser tab, identified as precisely as the platform allows.
///
/// On Chrome `id` is the browser's own unique tab id, which is stable for the
/// life of the tab. Safari exposes no tab id, so there `id` is a synthetic
/// `"idx:<n>"` built from the tab index — weaker, which is exactly why every
/// close re-verifies the URL as well as the id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabRef {
    pub id: String,
    pub url: String,
    pub title: String,
}

/// The frontmost application, plus its active tab when it is a browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    /// Bundle id on macOS ("com.google.Chrome"), executable name on Windows.
    pub app_id: String,
    pub app_name: String,
    /// `None` for native apps, and for browsers with nothing readable in front.
    pub tab: Option<TabRef>,
}

/// An app the user can pick in settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    /// PNG bytes, base64. `None` when the icon could not be read.
    pub icon_b64: Option<String>,
}

/// Where a browser window is, and which tab is active within it.
///
/// Enough to work out roughly where the active tab sits on screen, so the cat
/// can walk to it rather than swiping at thin air from across the desktop.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TabRect {
    /// Window edges, in screen points.
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    /// 1-based index of the active tab, and how many there are.
    pub index: u32,
    pub count: u32,
}

impl TabRect {
    /// Best guess at the centre of the active tab, in screen points.
    ///
    /// Chrome does not expose per-tab geometry, so this reconstructs it: the
    /// strip starts clear of the traffic lights, tabs share the remaining
    /// width evenly, and Chrome caps a tab at about 240 points.
    pub fn active_tab_center(&self) -> (f64, f64) {
        const LEADING: f64 = 78.0;   // traffic lights + padding
        const TRAILING: f64 = 60.0;  // new-tab button and friends
        const MAX_TAB: f64 = 240.0;

        let usable = (self.right - self.left - LEADING - TRAILING).max(40.0);
        let tab_w = (usable / self.count.max(1) as f64).min(MAX_TAB);
        let i = self.index.max(1) as f64;
        let x = self.left + LEADING + (i - 0.5) * tab_w;
        let y = self.top + 16.0;     // the strip sits at the top of the window
        (x.clamp(self.left, self.right), y)
    }
}

/// Result of asking the platform to close a tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CloseOutcome {
    /// The tab was still there and was closed.
    Closed,
    /// The tab moved, changed, or is no longer in front — nothing was touched.
    NoLongerMatching,
    /// This platform cannot close tabs (e.g. iOS).
    Unsupported,
}

/// The only thing that differs between macOS, Windows, Android and iOS.
///
/// Everything above this trait — timers, warnings, the diary, the UI — is
/// written once and shared.
///
/// **Not `Send`/`Sync` by design.** macOS drives browsers through OSAScript,
/// which is main-thread-only, so a probe that could be shared across threads
/// is a promise this platform cannot keep. The tick loop therefore runs on the
/// main thread — which is where a 1 Hz, ~10 ms call belongs in a GUI app
/// anyway. Anything genuinely slow belongs behind a channel, not behind a lie
/// about thread safety.
pub trait ActivityProbe {
    /// What is in front right now. `None` means idle, locked, or unreadable.
    fn current(&self) -> Option<Activity>;

    /// Every open tab across every window, foreground or not.
    ///
    /// Used only by the background-tab modes, so implementations may be called
    /// far less often than `current`. Returns empty when unsupported.
    fn open_tabs(&self) -> Vec<TabRef> {
        Vec::new()
    }

    /// Apps offerable in the settings picker.
    fn installed_apps(&self) -> Vec<AppInfo> {
        Vec::new()
    }

    /// Re-verify `target` is still present and still matches, then close it.
    ///
    /// Implementations MUST make the check and the close atomic, so a tab the
    /// user switched away from in the meantime is never closed by mistake.
    fn close_tab(&self, _target: &TabRef) -> anyhow::Result<CloseOutcome> {
        Ok(CloseOutcome::Unsupported)
    }

    /// Where the active tab is on screen, when the platform can say.
    fn active_tab_rect(&self) -> Option<TabRect> {
        None
    }

    /// Where a native app's front window is.
    fn app_window_rect(&self, _app_name: &str) -> Option<TabRect> {
        None
    }

    /// Get an app off the screen without destroying anything.
    ///
    /// Hiding, not quitting. Quitting someone's editor when a timer runs out
    /// could lose work; hiding is exactly reversible and gets it out of sight,
    /// which is all a limit should ever do.
    fn hide_app(&self, _app_name: &str) -> anyhow::Result<CloseOutcome> {
        Ok(CloseOutcome::Unsupported)
    }

    /// True when the screen is locked or the screensaver is up.
    fn is_idle(&self) -> bool {
        false
    }
}
