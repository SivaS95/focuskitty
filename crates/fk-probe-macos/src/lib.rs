//! macOS implementation of [`ActivityProbe`].
//!
//! Two sources, deliberately layered by cost and by permission:
//!
//! * **Which app is in front** comes from `NSWorkspace`, which needs no
//!   permission at all and is essentially free.
//! * **Which tab is in front** needs Apple Events, which costs a TCC prompt —
//!   so it is only ever asked when a browser is actually frontmost. Work in
//!   any other app and FocusKitty sends zero Apple Events.

#![cfg(target_os = "macos")]

mod scripts;

use std::cell::RefCell;

use anyhow::{anyhow, Result};
use fk_core::activity::{Activity, ActivityProbe, AppInfo, CloseOutcome, TabRect, TabRef};
use osakit::{Language, Script, Value};

pub const CHROME: &str = "com.google.Chrome";
pub const SAFARI: &str = "com.apple.Safari";

/// macOS puts `loginwindow` in front while the screen is locked.
const LOGIN_WINDOW: &str = "com.apple.loginwindow";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Browser {
    Chrome,
    Safari,
    /// Not a browser: System Events, for native apps.
    System,
}

impl Browser {
    fn bundle_id(self) -> &'static str {
        match self {
            Browser::Chrome => CHROME,
            Browser::Safari => SAFARI,
            Browser::System => "com.apple.systemevents",
        }
    }
}

/// The macOS probe.
///
/// # Threading
///
/// Every method must be called from the **main thread**: OSAScript refuses to
/// run anywhere else. The scripts are compiled once on first use and then
/// re-executed in-process, which keeps a call to roughly ten milliseconds —
/// against roughly a hundred and eighty for spawning `osascript`, almost all
/// of which is process startup rather than the Apple Event itself.
pub struct MacProbe {
    chrome: RefCell<Option<Script>>,
    safari: RefCell<Option<Script>>,
    system: RefCell<Option<Script>>,
}

impl Default for MacProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl MacProbe {
    pub fn new() -> Self {
        Self {
            chrome: RefCell::new(compile(&scripts::chrome(), "Chrome")),
            safari: RefCell::new(compile(&scripts::safari(), "Safari")),
            system: RefCell::new(compile(&scripts::system(), "System Events")),
        }
    }

    fn call(&self, browser: Browser, func: &str, args: Vec<Value>) -> Result<Value> {
        // Addressing an app that is not running LAUNCHES it. A focus app that
        // silently opens Safari every ten seconds would be intolerable, so the
        // running check comes first -- it is a free NSWorkspace read.
        if browser != Browser::System && !is_running(browser.bundle_id()) {
            return Err(anyhow!("{} is not running", browser.bundle_id()));
        }

        let slot = match browser {
            Browser::Chrome => &self.chrome,
            Browser::Safari => &self.safari,
            Browser::System => &self.system,
        };
        let mut guard = slot.borrow_mut();
        let script = guard.as_mut().ok_or_else(|| anyhow!("script unavailable"))?;
        script
            .execute_function(func, args)
            .map_err(|e| anyhow!("{func}: {e}"))
    }

    fn browser_for(bundle_id: &str) -> Option<Browser> {
        match bundle_id {
            CHROME => Some(Browser::Chrome),
            SAFARI => Some(Browser::Safari),
            _ => None,
        }
    }

    /// Which browser owns this tab id. Chrome ids are the browser's own;
    /// Safari has none, so ours are `idx:`-prefixed synthetics.
    fn browser_for_tab(tab: &TabRef) -> Browser {
        if tab.id.starts_with("idx:") {
            Browser::Safari
        } else {
            Browser::Chrome
        }
    }
}

fn compile(source: &str, label: &str) -> Option<Script> {
    let mut script = Script::new_from_source(Language::AppleScript, source);
    match script.compile() {
        Ok(()) => Some(script),
        Err(e) => {
            tracing::warn!("{label} script failed to compile: {e}");
            None
        }
    }
}

/// One window as `[[ids], [urls], [titles]]` — three parallel lists, because
/// fetching them in bulk costs three Apple Events instead of three per tab.
fn tabs_from_window(window: &Value, browser: Browser) -> Vec<TabRef> {
    let Some(cols) = window.as_array() else {
        return Vec::new();
    };
    if cols.len() < 3 {
        return Vec::new();
    }
    let (Some(ids), Some(urls), Some(titles)) =
        (cols[0].as_array(), cols[1].as_array(), cols[2].as_array())
    else {
        return Vec::new();
    };

    ids.iter()
        .zip(urls.iter())
        .zip(titles.iter())
        .filter_map(|((id, url), title)| {
            let url = value_str(url)?;
            if url.trim().is_empty() {
                return None; // a blank tab has nothing to track
            }
            let raw_id = value_str(id)?;
            Some(TabRef {
                // Safari indexes rather than identifies, so mark it as ours.
                id: match browser {
                    Browser::Safari => format!("idx:{raw_id}"),
                    // System Events never produces tabs; treat it as Chrome.
                    Browser::Chrome | Browser::System => raw_id,
                },
                url,
                title: value_str(title).unwrap_or_default(),
            })
        })
        .collect()
}

/// AppleScript hands numbers back as numbers, so accept either shape.
fn value_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// `["id", "url", "title"]` as returned by every `activeTab` / `allTabs` handler.
fn tab_from_value(v: &Value) -> Option<TabRef> {
    let arr = v.as_array()?;
    if arr.len() < 3 {
        return None;
    }
    let url = arr[1].as_str()?.trim().to_string();
    // A blank or brand-new tab has no URL worth tracking.
    if url.is_empty() {
        return None;
    }
    Some(TabRef {
        id: arr[0].as_str()?.to_string(),
        url,
        title: arr[2].as_str().unwrap_or_default().to_string(),
    })
}

impl ActivityProbe for MacProbe {
    fn current(&self) -> Option<Activity> {
        let (app_id, app_name) = frontmost_app()?;
        if app_id == LOGIN_WINDOW {
            return None; // screen locked
        }

        // Only pay for Apple Events when a browser is genuinely in front.
        let tab = match Self::browser_for(&app_id) {
            Some(browser) => match self.call(browser, "activeTab", vec![]) {
                Ok(v) => tab_from_value(&v),
                Err(e) => {
                    // AppleScript -1728 is "no front window" (everything
                    // minimized), which is ordinary, not a failure.
                    tracing::debug!("activeTab on {app_id}: {e}");
                    None
                }
            },
            None => None,
        };

        Some(Activity { app_id, app_name, tab })
    }

    fn open_tabs(&self) -> Vec<TabRef> {
        let mut out = Vec::new();
        for browser in [Browser::Chrome, Browser::Safari] {
            match self.call(browser, "allTabs", vec![]) {
                Ok(Value::Array(windows)) => {
                    for w in &windows {
                        out.extend(tabs_from_window(w, browser));
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::debug!("allTabs: {e}"),
            }
        }
        out
    }

    fn installed_apps(&self) -> Vec<AppInfo> {
        running_apps()
    }

    fn app_window_rect(&self, app_name: &str) -> Option<TabRect> {
        let v = self
            .call(Browser::System, "appRect", vec![Value::String(app_name.into())])
            .ok()?;
        let a = v.as_array()?;
        if a.len() < 6 {
            return None;
        }
        let n = |i: usize| a.get(i).and_then(|x| x.as_f64());
        Some(TabRect {
            left: n(0)?,
            top: n(1)?,
            right: n(2)?,
            bottom: n(3)?,
            index: 1,
            count: 1,
        })
    }

    /// Hide an app through AppKit, not through System Events.
    ///
    /// The old route asked System Events to make the process invisible, which
    /// needs Accessibility permission -- and the script swallowed the refusal
    /// and answered "nomatch", so a denied permission looked exactly like an
    /// app that had already gone. That permission is also tied to the app's
    /// code signature, so it lapses on every rebuild of an unsigned build.
    ///
    /// `NSRunningApplication.hide()` is the API for this and needs no
    /// permission at all. It also matches on the BUNDLE ID where it can, which
    /// is stable, rather than on a display name that is localised.
    fn hide_app(&self, app_name: &str) -> Result<CloseOutcome> {
        use objc2_app_kit::NSWorkspace;

        let workspace = NSWorkspace::sharedWorkspace();
        let hidden = workspace.runningApplications().iter().any(|app| {
            let matches = app
                .bundleIdentifier()
                .is_some_and(|id| id.to_string().eq_ignore_ascii_case(app_name))
                || app
                    .localizedName()
                    .is_some_and(|n| n.to_string().eq_ignore_ascii_case(app_name));
            if matches {
                // Already hidden counts as done: the limit wanted it off the
                // screen, and it is off the screen.
                if app.isHidden() {
                    return true;
                }
                return app.hide();
            }
            false
        });

        if !hidden {
            tracing::info!("hide refused: nothing running called {app_name:?}");
        }
        Ok(if hidden { CloseOutcome::Closed } else { CloseOutcome::NoLongerMatching })
    }

    fn active_tab_rect(&self) -> Option<TabRect> {
        let (app_id, _) = frontmost_app()?;
        let browser = Self::browser_for(&app_id)?;
        let v = self.call(browser, "tabRect", vec![]).ok()?;
        let a = v.as_array()?;
        if a.len() < 6 {
            return None;
        }
        let n = |i: usize| a.get(i).and_then(|x| x.as_f64());
        Some(TabRect {
            left: n(0)?,
            top: n(1)?,
            right: n(2)?,
            bottom: n(3)?,
            index: n(4)? as u32,
            count: n(5)?.max(1.0) as u32,
        })
    }

    fn close_tab(&self, target: &TabRef) -> Result<CloseOutcome> {
        let host = fk_core::domain::host_of(&target.url)
            .ok_or_else(|| anyhow!("no host in {}", target.url))?;
        let domain = host.strip_prefix("www.").unwrap_or(&host).to_string();

        let result = self.call(
            Self::browser_for_tab(target),
            "closeTab",
            vec![Value::String(target.id.clone()), Value::String(domain)],
        )?;

        Ok(match result.as_str() {
            Some("closed") => CloseOutcome::Closed,
            // "nomatch" — the tab moved, navigated away, or was already gone.
            // Nothing was touched, which is the whole point of re-verifying.
            _ => CloseOutcome::NoLongerMatching,
        })
    }

    fn is_idle(&self) -> bool {
        matches!(frontmost_app(), Some((id, _)) if id == LOGIN_WINDOW)
    }
}

// --- AppKit -----------------------------------------------------------------

/// Is this app running right now? Needs no permission, and costs nothing.
///
/// Guards every AppleScript call, because `tell application "X"` will start X.
fn is_running(bundle_id: &str) -> bool {
    use objc2_app_kit::NSWorkspace;

    let workspace = NSWorkspace::sharedWorkspace();
    workspace.runningApplications().iter().any(|app| {
        app.bundleIdentifier()
            .is_some_and(|id| id.to_string() == bundle_id)
    })
}

/// The frontmost app's bundle id and display name. Needs no TCC permission.
fn frontmost_app() -> Option<(String, String)> {
    use objc2_app_kit::NSWorkspace;

    let workspace = NSWorkspace::sharedWorkspace();
    let app = workspace.frontmostApplication()?;
    let bundle_id = app.bundleIdentifier()?.to_string();
    let name = app
        .localizedName()
        .map(|n| n.to_string())
        .unwrap_or_else(|| bundle_id.clone());
    Some((bundle_id, name))
}

/// Apps with a Dock presence, for the settings picker.
fn running_apps() -> Vec<AppInfo> {
    use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};

    let workspace = NSWorkspace::sharedWorkspace();
    let apps = workspace.runningApplications();

    let mut out: Vec<AppInfo> = apps
        .iter()
        .filter(|app| {
            // Regular = has a Dock icon. Skips agents and daemons, which the
            // user has no business setting a screen-time limit on.
            app.activationPolicy() == NSApplicationActivationPolicy::Regular
        })
        .filter_map(|app| {
            let id = app.bundleIdentifier()?.to_string();
            let name = app
                .localizedName()
                .map(|n| n.to_string())
                .unwrap_or_else(|| id.clone());
            Some(AppInfo { id, name, icon_b64: None })
        })
        .collect();

    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out.dedup_by(|a, b| a.id == b.id);
    out
}
