//! "What is in front of you?", answered on Windows.
//!
//! The macOS probe asks the browser directly: Chrome's AppleScript dictionary
//! hands over a tab's id, URL and title, and closing one is a single scripted
//! sentence. Windows has no such door. There is no scripting interface on any
//! browser here, so everything is read the way a screen reader reads it --
//! through UI Automation, off the window's accessibility tree -- and the tab
//! is closed the way a person would close it, with Ctrl+W.
//!
//! That difference decides the shape of this file:
//!
//! * **No tab ids.** Chromium exposes no per-tab identity to UIA, so a tab is
//!   identified by its URL, exactly as Safari's tabs are identified by index.
//!   The close path re-reads the URL immediately before acting, which is what
//!   keeps "close that tab" from becoming "close whatever is in front now".
//! * **Foreground only.** UIA can only see windows that exist; it cannot
//!   enumerate tabs cheaply across every window, so `open_tabs` stays empty
//!   and the background modes degrade to foreground counting.
//! * **Ctrl+W is a keystroke, not a command.** It lands wherever focus is. So
//!   it is sent only after confirming the browser is *still* frontmost and
//!   *still* showing the URL we mean to close.

#![cfg(target_os = "windows")]

use std::cell::RefCell;

use anyhow::{anyhow, Result};
use fk_core::activity::{Activity, ActivityProbe, AppInfo, CloseOutcome, TabRect, TabRef};

use uiautomation::controls::ControlType;
use uiautomation::patterns::{UISelectionItemPattern, UIValuePattern};
use uiautomation::types::Handle;
use uiautomation::{UIAutomation, UIElement};

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, MAX_PATH, RECT, TRUE};
use windows::core::BOOL;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, OpenInputDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VK_CONTROL, VK_W,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsWindowVisible, ShowWindow, SW_MINIMIZE,
};
use windows::core::PWSTR;

/// Executable name -> the name a person would call it.
///
/// Matched on the executable rather than the window title because a title is
/// whatever page is open, and localised besides.
const BROWSERS: &[(&str, &str)] = &[
    ("chrome.exe", "Google Chrome"),
    ("msedge.exe", "Microsoft Edge"),
    ("brave.exe", "Brave"),
    ("vivaldi.exe", "Vivaldi"),
    ("opera.exe", "Opera"),
    ("opera_gx.exe", "Opera GX"),
    ("firefox.exe", "Firefox"),
    ("librewolf.exe", "LibreWolf"),
    ("arc.exe", "Arc"),
];

fn browser_name(exe: &str) -> Option<&'static str> {
    BROWSERS
        .iter()
        .find(|(e, _)| e.eq_ignore_ascii_case(exe))
        .map(|(_, n)| *n)
}

pub struct WinProbe {
    /// Created once and reused. Building a UIAutomation is a COM activation;
    /// doing it every tick would be the most expensive thing in the loop.
    ///
    /// `RefCell` rather than a plain field because the trait hands out `&self`
    /// and COM objects are not `Sync` -- which is exactly why `ActivityProbe`
    /// is deliberately not `Send`/`Sync`.
    automation: RefCell<Option<UIAutomation>>,
    /// The last URL read, against the window and title it was read from.
    ///
    /// Reading a URL means walking the accessibility tree, which is expensive
    /// -- and on a browser that does not expose a Document it is expensive AND
    /// fruitless, burning the search timeout twice a second for nothing. That
    /// cost lands on the main thread, and a tick that arrives more than five
    /// seconds late is charged ZERO, because that is how a closed lid is meant
    /// to look. So a slow read does not merely lag: it stops the clock.
    ///
    /// The title is the key because it is nearly free to read and it changes
    /// on exactly the events that change the URL -- navigating, or switching
    /// tabs. One search per page, instead of one per second.
    cache: RefCell<Option<Cached>>,
}

/// What was last read, and what it cost.
struct Cached {
    hwnd: isize,
    title: String,
    url: Option<String>,
    at: std::time::Instant,
    /// How long the read took. A slow window is not asked again soon: the
    /// clock is charged from the gap between ticks, and a probe that blocks
    /// is indistinguishable from a machine that was asleep.
    cost: std::time::Duration,
}

impl Default for WinProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl WinProbe {
    pub fn new() -> Self {
        Self { automation: RefCell::new(None), cache: RefCell::new(None) }
    }

    /// The automation client, built on first use.
    ///
    /// A failure here is not fatal: without UIA we still know which app is in
    /// front, so app limits keep working and only site limits go quiet.
    fn with_ui<T>(&self, f: impl FnOnce(&UIAutomation) -> Option<T>) -> Option<T> {
        let mut slot = self.automation.borrow_mut();
        if slot.is_none() {
            match UIAutomation::new() {
                Ok(ui) => *slot = Some(ui),
                Err(e) => {
                    tracing::warn!("UI Automation unavailable ({e}); site limits will not count");
                    return None;
                }
            }
        }
        f(slot.as_ref()?)
    }

    /// The URL showing in a browser window, read off its accessibility tree.
    ///
    /// Two routes, in order of trustworthiness:
    ///
    /// 1. The **Document** element's value. Chromium puts the real page URL
    ///    there, which is what we want -- it is the address of what you are
    ///    actually looking at.
    /// 2. The address bar, an **Edit** control. Used only as a fallback,
    ///    because it holds whatever is *typed*: mid-edit it is a half-written
    ///    search, and while focused Chrome may hide the scheme entirely.
    ///
    /// Deliberately not matched on the control's name ("Address and search
    /// bar"), which is localised -- that would work on an English Windows and
    /// silently fail everywhere else.
    fn url_of(&self, hwnd: HWND, title: &str) -> Option<String> {
        let key = hwnd.0 as isize;

        // A title change is the signal that the page changed -- but it is not
        // a reliable one. YouTube rewrites its title for a notification count,
        // a live viewer number, a video advancing; each rewrite would trigger
        // another full walk of the tree. That is what stalls the tick, and a
        // stalled tick is charged ZERO, so the timer appears to stop while the
        // user is sitting on exactly the page they asked to be timed.
        //
        // So the title only earns a re-read after a floor has passed, and a
        // read that proved expensive earns a much longer one.
        const FLOOR: std::time::Duration = std::time::Duration::from_secs(3);
        const SLOW: std::time::Duration = std::time::Duration::from_millis(250);
        const PENALTY: std::time::Duration = std::time::Duration::from_secs(30);

        if let Some(c) = self.cache.borrow().as_ref() {
            let wait = if c.cost > SLOW { PENALTY } else { FLOOR };
            let fresh = c.at.elapsed() < wait;
            if c.hwnd == key && (c.title == title || fresh) {
                return c.url.clone();
            }
        }

        let started = std::time::Instant::now();
        let found = self.read_url(hwnd);
        let cost = started.elapsed();
        if cost > SLOW {
            tracing::warn!("reading the address took {cost:?}; backing off");
        }
        // Cached even when nothing was found: a browser that will not give up
        // its address should be asked once per page, not once per second.
        *self.cache.borrow_mut() = Some(Cached {
            hwnd: key,
            title: title.to_string(),
            url: found.clone(),
            at: std::time::Instant::now(),
            cost,
        });
        found
    }

    /// The URL showing in a browser window, read off its accessibility tree.
    ///
    /// Deliberately NOT a search for a particular control. The first version
    /// looked for a Document element and then an address bar, which can only
    /// work if a guess about how a given browser exposes itself is correct --
    /// and when the guess is wrong the result is silence, indistinguishable
    /// from a browser with no address at all. Chrome, Edge, Firefox and Brave
    /// need not agree, and none of them owes us a stable tree.
    ///
    /// So: walk the window once and take the first value that LOOKS like a
    /// URL, wherever it turns out to live. Whatever holds it -- the omnibox,
    /// the document, something none of us has thought of -- it is found by
    /// what it contains rather than by where it was expected to be.
    ///
    /// Bounded by depth and by a node budget, because this runs on the thread
    /// that keeps time.
    fn read_url(&self, hwnd: HWND) -> Option<String> {
        self.with_ui(|ui| {
            let root = ui.element_from_handle(Handle::from(hwnd.0 as isize)).ok()?;
            let walker = ui.get_control_view_walker().ok()?;
            // Bounded hard: this runs on the thread that keeps time.
            let mut budget = 200usize;
            find_url(&walker, &root, 0, 8, &mut budget)
        })
    }

    /// Which tab is selected, and how many there are.
    ///
    /// Only used to aim the cat: it walks to the tab it is about to close, and
    /// the tab strip is reconstructed from the index. Failure is harmless --
    /// the cat then swipes at the middle of the window instead.
    fn tab_position(&self, hwnd: HWND) -> Option<(u32, u32)> {
        self.with_ui(|ui| {
            let root = ui.element_from_handle(Handle::from(hwnd.0 as isize)).ok()?;
            let strip = ui
                .create_matcher()
                .from_ref(&root)
                .control_type(ControlType::Tab)
                .depth(10)
                .timeout(150)
                .find_first()
                .ok()?;
            let tabs = ui
                .create_matcher()
                .from_ref(&strip)
                .control_type(ControlType::TabItem)
                .depth(10)
                .timeout(150)
                .find_all()
                .ok()?;
            if tabs.is_empty() {
                return None;
            }
            let selected = tabs.iter().position(|t| {
                t.get_pattern::<UISelectionItemPattern>()
                    .and_then(|p| p.is_selected())
                    .unwrap_or(false)
            })?;
            Some((selected as u32 + 1, tabs.len() as u32))
        })
    }

    /// The foreground window, if there is one and it belongs to a real app.
    fn front(&self) -> Option<(HWND, u32, String)> {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.is_invalid() {
            return None;
        }
        let pid = pid_of(hwnd)?;
        let exe = exe_of(pid)?;
        Some((hwnd, pid, exe))
    }
}

impl ActivityProbe for WinProbe {
    fn current(&self) -> Option<Activity> {
        let (hwnd, _pid, exe) = self.front()?;

        // The desktop and the taskbar are not something you are doing.
        if is_shell_window(&exe, hwnd) {
            return None;
        }

        // Our own windows ARE reported, deliberately. The tracker needs to
        // know the difference between "FocusKitty is in front" -- keep
        // remembering what they were doing, they only opened the controls --
        // and "nothing readable is in front", which means idle. Answering
        // None for both collapses that distinction and loses the memory.

        let title = window_title(hwnd).unwrap_or_default();
        let tab = browser_name(&exe).and_then(|_| {
            let url = self.url_of(hwnd, &title)?;
            Some(TabRef {
                // No tab ids exist on Windows, so the URL IS the identity --
                // and every close re-reads it rather than trusting this copy.
                id: format!("url:{url}"),
                title: title.clone(),
                url,
            })
        });

        Some(Activity {
            app_name: browser_name(&exe)
                .map(str::to_string)
                .or_else(|| window_title(hwnd))
                .unwrap_or_else(|| pretty_exe(&exe)),
            app_id: exe.to_ascii_lowercase(),
            tab,
        })
    }

    fn installed_apps(&self) -> Vec<AppInfo> {
        let mut out: Vec<AppInfo> = Vec::new();
        for hwnd in visible_windows() {
            let Some(exe) = pid_of(hwnd).and_then(exe_of) else { continue };
            if exe.eq_ignore_ascii_case("focuskitty.exe") {
                continue;
            }
            let id = exe.to_ascii_lowercase();
            if out.iter().any(|a| a.id == id) {
                continue;
            }
            out.push(AppInfo {
                name: browser_name(&exe).map(str::to_string).unwrap_or_else(|| pretty_exe(&exe)),
                id,
                // Pulling an HICON out and re-encoding it as a PNG is a lot of
                // code for a picture; the picker reads fine on names alone.
                icon_b64: None,
            });
        }
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        out
    }

    /// Close the tab -- but only if it is still the one we mean.
    ///
    /// Ctrl+W is not addressed to anything. It goes wherever focus happens to
    /// be, so if the user switched tabs, switched windows, or alt-tabbed to
    /// their editor in the moment between the timer expiring and this running,
    /// an unguarded keystroke would shut whatever they moved to. Hence the
    /// re-read: browser still frontmost, URL still the same, then send. Any
    /// mismatch and nothing is touched.
    fn close_tab(&self, target: &TabRef) -> Result<CloseOutcome> {
        let Some((hwnd, _, exe)) = self.front() else {
            return Ok(CloseOutcome::NoLongerMatching);
        };
        if browser_name(&exe).is_none() {
            return Ok(CloseOutcome::NoLongerMatching);
        }
        let title = window_title(hwnd).unwrap_or_default();
        let Some(now) = self.url_of(hwnd, &title) else {
            return Ok(CloseOutcome::NoLongerMatching);
        };
        if !same_page(&now, &target.url) {
            return Ok(CloseOutcome::NoLongerMatching);
        }

        send_ctrl_w()?;
        Ok(CloseOutcome::Closed)
    }

    fn active_tab_rect(&self) -> Option<TabRect> {
        let (hwnd, _, exe) = self.front()?;
        browser_name(&exe)?;
        let r = window_rect(hwnd)?;
        let (index, count) = self.tab_position(hwnd).unwrap_or((1, 1));
        Some(TabRect { index, count, ..r })
    }

    fn app_window_rect(&self, app_name: &str) -> Option<TabRect> {
        let hwnd = window_of_app(app_name)?;
        window_rect(hwnd)
    }

    /// Minimise, never close. A limit should get something out of your sight,
    /// not throw away whatever was unsaved in it.
    fn hide_app(&self, app_name: &str) -> Result<CloseOutcome> {
        let Some(hwnd) = window_of_app(app_name) else {
            return Ok(CloseOutcome::NoLongerMatching);
        };
        let _ = unsafe { ShowWindow(hwnd, SW_MINIMIZE) };
        Ok(CloseOutcome::Closed)
    }

    /// Locked, or on the secure desktop (UAC, Ctrl+Alt+Del).
    ///
    /// `OpenInputDesktop` is refused to an ordinary process whenever the input
    /// desktop is not the user's own, which is precisely those cases.
    fn is_idle(&self) -> bool {
        unsafe {
            // DESKTOP_READOBJECTS (0x0001) -- the least we can ask for, and
            // still refused when the input desktop is not the user's own.
            match OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS_FLAGS(0x0001)) {
                Ok(desk) => {
                    let _ = CloseDesktop(desk);
                    false
                }
                Err(_) => true,
            }
        }
    }
}

// --- Win32 odds and ends ----------------------------------------------------

fn pid_of(hwnd: HWND) -> Option<u32> {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (pid != 0).then_some(pid)
}

/// The executable's file name, e.g. "chrome.exe".
fn exe_of(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; MAX_PATH as usize];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(handle);
        ok.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/']).next().map(str::to_string)
    }
}

/// "code.exe" -> "Code". Good enough for a picker entry; the friendly names
/// that matter are in BROWSERS.
fn pretty_exe(exe: &str) -> String {
    let stem = exe.strip_suffix(".exe").unwrap_or(exe);
    let mut c = stem.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => stem.to_string(),
    }
}

/// Window class name. The only way to tell Explorer-the-shell (the taskbar,
/// the desktop) from Explorer-the-file-manager: both are explorer.exe.
fn window_class(hwnd: HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

/// Is this the desktop or the taskbar rather than an application?
///
/// Clicking the cat deactivates whatever you were using, and because the cat
/// window refuses focus Windows hands the foreground to the SHELL. Left alone,
/// the tracker then records the taskbar as "what you were doing" and offers to
/// put a limit on it -- which is what "now: Explorer" meant while the user was
/// plainly sitting on YouTube.
fn is_shell_window(exe: &str, hwnd: HWND) -> bool {
    if !exe.eq_ignore_ascii_case("explorer.exe") {
        return false;
    }
    matches!(
        window_class(hwnd).as_str(),
        // taskbar         | secondary taskbar     | desktop
        "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Progman" | "WorkerW"
    )
}

fn window_title(hwnd: HWND) -> Option<String> {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return None;
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf);
        (n > 0).then(|| String::from_utf16_lossy(&buf[..n as usize]))
    }
}

fn window_rect(hwnd: HWND) -> Option<TabRect> {
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r).ok()? };
    Some(TabRect {
        left: r.left as f64,
        top: r.top as f64,
        right: r.right as f64,
        bottom: r.bottom as f64,
        index: 1,
        count: 1,
    })
}

fn visible_windows() -> Vec<HWND> {
    let mut found: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut found as *mut Vec<HWND> as isize));
    }
    found
}

extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        // A window with no title is a tool window, a tray host or a hidden
        // message sink -- never something the user thinks of as an app.
        if IsWindowVisible(hwnd).as_bool() && GetWindowTextLengthW(hwnd) > 0 {
            let out = &mut *(lparam.0 as *mut Vec<HWND>);
            out.push(hwnd);
        }
    }
    TRUE
}

/// The first visible window belonging to an app, matched on executable or
/// on the name shown in the picker.
fn window_of_app(app: &str) -> Option<HWND> {
    let want = app.trim().to_ascii_lowercase();
    let want_exe = if want.ends_with(".exe") { want.clone() } else { format!("{want}.exe") };
    visible_windows().into_iter().find(|&hwnd| {
        match pid_of(hwnd).and_then(exe_of) {
            Some(exe) => {
                let exe = exe.to_ascii_lowercase();
                exe == want_exe
                    || exe == want
                    || browser_name(&exe).is_some_and(|n| n.eq_ignore_ascii_case(app))
            }
            None => false,
        }
    })
}

/// Ctrl down, W, W up, Ctrl up -- as four events, in order.
fn send_ctrl_w() -> Result<()> {
    fn key(vk: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(vk),
                    dwFlags: flags,
                    ..Default::default()
                },
            },
        }
    }
    let seq = [
        key(VK_CONTROL.0, KEYBD_EVENT_FLAGS(0)),
        key(VK_W.0, KEYBD_EVENT_FLAGS(0)),
        key(VK_W.0, KEYEVENTF_KEYUP),
        key(VK_CONTROL.0, KEYEVENTF_KEYUP),
    ];
    let sent = unsafe { SendInput(&seq, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != seq.len() {
        return Err(anyhow!("SendInput delivered {sent} of {} events", seq.len()));
    }
    Ok(())
}

// --- URL helpers ------------------------------------------------------------

/// Is this string a web address?
///
/// Strict on purpose. The URL is now found by scanning every value in the
/// window rather than by looking in a known place, so this predicate is the
/// only thing standing between "the address" and any other dotted string that
/// happens to be lying around -- a version number, a file size, a timestamp.
/// A false positive here would be charged to a site the user never visited.
fn looks_like_url(s: &str) -> bool {
    let s = s.trim();
    if s.is_empty() || s.contains(char::is_whitespace) || s.len() < 4 {
        return false;
    }
    let after_scheme = s.split_once("://").map(|(_, r)| r).unwrap_or(s);
    // Only a real scheme may carry one; "1.2.3:4" is not an address.
    if s.contains("://") && !s.starts_with("http") && !s.starts_with("file") {
        return false;
    }
    let host = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('.');
    let Some((name, tld)) = host.rsplit_once('.') else {
        return false;
    };
    // A host has something before the dot, and a top-level domain is at least
    // two letters -- which is what rules out "1.0" and "3.14".
    !name.is_empty()
        && tld.len() >= 2
        && tld.chars().all(|c| c.is_ascii_alphabetic())
        && name.chars().any(|c| c.is_ascii_alphanumeric())
}

/// Is this the same page, allowing for the browser's own tidying?
///
/// Chromium hides "https://" and a leading "www." in the address bar and puts
/// them back when the page is read from the Document element, so the two
/// routes can describe one page two ways. Comparing the normalised host and
/// path keeps that from reading as "the user navigated away".
fn same_page(a: &str, b: &str) -> bool {
    fn key(s: &str) -> String {
        let s = s.trim().trim_end_matches('/');
        let s = s.split_once("://").map(|(_, r)| r).unwrap_or(s);
        let s = s.strip_prefix("www.").unwrap_or(s);
        s.to_ascii_lowercase()
    }
    key(a) == key(b)
}

/// Depth-first hunt for something URL-shaped, anywhere under `el`.
///
/// Returns the first match. The address bar generally comes before the
/// document in tree order, and either describes the same page, so first is as
/// good as any -- and far cheaper than collecting them all.
fn find_url(
    walker: &uiautomation::UITreeWalker,
    el: &UIElement,
    depth: usize,
    max_depth: usize,
    budget: &mut usize,
) -> Option<String> {
    if depth > max_depth || *budget == 0 {
        return None;
    }
    *budget -= 1;

    if let Ok(p) = el.get_pattern::<UIValuePattern>() {
        if let Ok(v) = p.get_value() {
            let v = v.trim();
            if looks_like_url(v) {
                return Some(v.to_string());
            }
        }
    }

    let mut child = walker.get_first_child(el).ok()?;
    loop {
        if let Some(found) = find_url(walker, &child, depth + 1, max_depth, budget) {
            return Some(found);
        }
        match walker.get_next_sibling(&child) {
            Ok(next) => child = next,
            Err(_) => return None,
        }
    }
}

/// Print the accessibility tree of whatever is in front, for diagnosis.
///
/// Kept even though `find_url` no longer needs to be told where to look: when
/// a browser yields nothing, this is what distinguishes "the tree is empty"
/// from "the value is there and my test rejected it".
pub fn dump_front_window(max_depth: usize) -> Result<String> {
    dump_window(max_depth, None)
}

/// The same, for a named executable rather than whatever is in front.
///
/// A machine with nobody sitting at it has no meaningful foreground window,
/// so naming the process is the only way to diagnose a browser on a CI runner
/// -- which is the one Windows machine always available.
pub fn dump_window(max_depth: usize, exe: Option<&str>) -> Result<String> {
    let hwnd = match exe {
        Some(name) => window_of_app(name).ok_or_else(|| anyhow!("no visible window for {name}"))?,
        None => unsafe { GetForegroundWindow() },
    };
    if hwnd.is_invalid() {
        return Err(anyhow!("nothing is in front"));
    }
    let exe = pid_of(hwnd).and_then(exe_of).unwrap_or_default();
    let ui = UIAutomation::new().map_err(|e| anyhow!("no UI Automation: {e}"))?;
    let root = ui
        .element_from_handle(Handle::from(hwnd.0 as isize))
        .map_err(|e| anyhow!("element_from_handle failed: {e}"))?;
    let walker = ui.get_control_view_walker().map_err(|e| anyhow!("{e}"))?;

    let mut out = format!("front: {exe}  title: {:?}\n", window_title(hwnd));
    fn walk(
        w: &uiautomation::UITreeWalker,
        el: &UIElement,
        depth: usize,
        max: usize,
        out: &mut String,
    ) {
        if depth > max {
            return;
        }
        let kind = el.get_control_type().map(|c| format!("{c:?}")).unwrap_or_default();
        let name = el.get_name().unwrap_or_default();
        let value = el
            .get_pattern::<UIValuePattern>()
            .ok()
            .and_then(|p| p.get_value().ok())
            .unwrap_or_default();
        let name: String = name.chars().take(60).collect();
        let value: String = value.chars().take(90).collect();
        out.push_str(&format!(
            "{:indent$}{kind} name={name:?}{}\n",
            "",
            if value.is_empty() {
                String::new()
            } else {
                format!(" VALUE={value:?} url?={}", looks_like_url(&value))
            },
            indent = depth * 2
        ));
        if let Ok(child) = w.get_first_child(el) {
            let mut cur = child;
            for _ in 0..40 {
                walk(w, &cur, depth + 1, max, out);
                match w.get_next_sibling(&cur) {
                    Ok(next) => cur = next,
                    Err(_) => break,
                }
            }
        }
    }
    walk(&walker, &root, 0, max_depth, &mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tidied_address_bar_is_still_the_same_page() {
        // What the Document element says vs what the address bar shows.
        assert!(same_page("https://www.youtube.com/feed", "youtube.com/feed"));
        assert!(same_page("https://example.com/", "https://example.com"));
        assert!(!same_page("https://youtube.com/a", "https://youtube.com/b"));
    }

    #[test]
    fn recognises_addresses_and_rejects_everything_else() {
        assert!(looks_like_url("https://youtube.com"));
        assert!(looks_like_url("youtube.com/watch?v=x"));
        assert!(looks_like_url("www.bbc.co.uk"));
        assert!(looks_like_url("http://localhost.dev/x"));

        // Typed searches.
        assert!(!looks_like_url("how to focus"));
        assert!(!looks_like_url(""));

        // The reason this predicate has to be strict: the URL is found by
        // scanning every value in the window, so any dotted string in the
        // tree is a candidate. None of these may pass.
        assert!(!looks_like_url("1.0"));
        assert!(!looks_like_url("3.14159"));
        assert!(!looks_like_url("v2.1.4"));
        assert!(!looks_like_url("12.5"));
        assert!(!looks_like_url("file.7z"));
        assert!(!looks_like_url("mailto://someone.com"));
    }

    #[test]
    fn browsers_are_matched_case_insensitively() {
        assert_eq!(browser_name("Chrome.exe"), Some("Google Chrome"));
        assert_eq!(browser_name("msedge.exe"), Some("Microsoft Edge"));
        assert_eq!(browser_name("notepad.exe"), None);
    }

    #[test]
    fn exe_names_become_readable() {
        assert_eq!(pretty_exe("code.exe"), "Code");
        assert_eq!(pretty_exe("slack.exe"), "Slack");
    }
}
