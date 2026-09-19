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

use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};

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
    /// The last URL read, and the window it belongs to.
    ///
    /// Filled in by a worker thread, never by the caller. Reading a URL means
    /// a cross-process COM call into the browser, and a browser with nothing
    /// happening in it does not answer promptly -- an idle Chrome renderer can
    /// leave that call outstanding for many seconds. There is no timeout to
    /// set: it returns when the other program feels like answering.
    ///
    /// That cannot be allowed to happen on the thread keeping time. A tick
    /// arriving more than five seconds late is charged ZERO, deliberately,
    /// because that is how a closed lid looks -- so a browser that does not
    /// answer would stop the clock. Which is exactly what it did: the timer
    /// froze on an idle tab and started again the moment the window was
    /// minimised and restored, because that woke the renderer and released
    /// the call.
    ///
    /// So the clock reads this, and only this, and never waits.
    latest: Arc<Mutex<Option<Reading>>>,
    /// Requests to the reader. Bounded to one in flight; a full channel means
    /// the reader is still busy, and the request is simply dropped.
    ask: SyncSender<(isize, String)>,
}

/// A URL, and the window and title it was read from.
#[derive(Clone)]
struct Reading {
    hwnd: isize,
    title: String,
    url: Option<String>,
}


impl Default for WinProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl WinProbe {
    pub fn new() -> Self {
        let latest: Arc<Mutex<Option<Reading>>> = Arc::new(Mutex::new(None));
        // One slot. If the reader is still waiting on a browser, further
        // requests are dropped rather than queued -- by the time it answers,
        // an old request is answering a question nobody is asking any more.
        let (ask, rx) = std::sync::mpsc::sync_channel::<(isize, String)>(1);

        let store = latest.clone();
        std::thread::spawn(move || {
            // Its own automation client, on its own thread. COM wants a
            // multi-threaded apartment, which a plain thread can give it and
            // a GUI main thread cannot.
            let ui = match UIAutomation::new() {
                Ok(ui) => ui,
                Err(e) => {
                    tracing::warn!("UI Automation unavailable ({e}); site limits will not count");
                    return;
                }
            };
            while let Ok((hwnd, title)) = rx.recv() {
                let url = read_url_with(&ui, HWND(hwnd as *mut std::ffi::c_void));
                *store.lock().unwrap() = Some(Reading { hwnd, title, url });
            }
        });

        Self { latest, ask }
    }

    /// A client for the callers that may block: closing, and measuring where
    /// a tab sits. Both run on the animation thread, which has nothing to do
    /// but wait, so a slow browser there costs only the animation.
    fn with_ui<T>(&self, f: impl FnOnce(&UIAutomation) -> Option<T>) -> Option<T> {
        match UIAutomation::new() {
            Ok(ui) => f(&ui),
            Err(e) => {
                tracing::warn!("UI Automation unavailable ({e})");
                None
            }
        }
    }

    /// The URL of a browser window -- whatever the reader last managed to get.
    ///
    /// NEVER waits. If the answer is stale, or missing, that is what comes
    /// back, and a request goes out for next time. One second of a slightly
    /// old address is a far smaller error than a stopped clock.
    fn url_of(&self, hwnd: HWND, title: &str) -> Option<String> {
        let key = hwnd.0 as isize;
        let known = self.latest.lock().unwrap().clone();

        match &known {
            // Current: same window, same title. Nothing to ask.
            Some(r) if r.hwnd == key && r.title == title => r.url.clone(),
            // Stale or absent. Ask, and answer with what we have meanwhile --
            // the same window's previous address is very likely still right.
            _ => {
                let _ = self.ask.try_send((key, title.to_string()));
                known.filter(|r| r.hwnd == key).and_then(|r| r.url)
            }
        }
    }

    fn read_url(&self, hwnd: HWND) -> Option<String> {
        self.with_ui(|ui| read_url_with(ui, hwnd))
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
        // Read it FRESH, and wait for the answer. `url_of` is deliberately
        // non-blocking and may hand back a second-old address -- fine for
        // counting time, wrong for deciding what to close. Ctrl+W goes
        // wherever focus is, so a stale answer here means shutting a tab the
        // user had already moved away from. This path runs on the animation
        // thread, which has nothing to do but wait.
        let Some(now) = self.read_url(hwnd) else {
            tracing::info!("close refused: could not read the address");
            return Ok(CloseOutcome::NoLongerMatching);
        };
        // The SITE, not the exact page -- which is what macOS has always
        // checked ("does the URL still match this domain"). Demanding the
        // identical URL made this refuse almost every time on a site worth
        // limiting: YouTube rewrites its address on every click, so between
        // the limit expiring and the cat walking over, the page has moved on.
        // The cat swiped and nothing closed.
        //
        // The question this guard exists to answer is "am I still shutting
        // the thing I was asked to shut", and the answer is the domain.
        let want = fk_core::domain::normalize(&target.url);
        if want.is_empty() || !fk_core::domain::matches_domain(&now, &want) {
            tracing::info!("close refused: now on {now:?}, was asked to close {want:?}");
            return Ok(CloseOutcome::NoLongerMatching);
        }
        tracing::info!("closing {want}: still on {now}");

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
        use windows::Win32::UI::WindowsAndMessaging::IsIconic;

        // Only windows that are actually UP. Without this the cat walked over
        // and minimised an already-minimised window, reported success, and was
        // asked to do it again -- over and over at an app that was already
        // out of the way.
        let mut acted = false;
        for hwnd in windows_of_app(app_name) {
            if unsafe { IsIconic(hwnd) }.as_bool() {
                continue;
            }
            let _ = unsafe { ShowWindow(hwnd, SW_MINIMIZE) };
            acted = true;
        }
        if !acted {
            tracing::info!("{app_name} is already out of the way; nothing to hide");
        }
        Ok(if acted { CloseOutcome::Closed } else { CloseOutcome::NoLongerMatching })
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

/// Every visible window belonging to an app.
///
/// Plural on purpose: File Explorer alone routinely has several, and hiding
/// one of them leaves the app still in front of you.
fn windows_of_app(app: &str) -> Vec<HWND> {
    let want = app.trim().to_ascii_lowercase();
    let want_exe = if want.ends_with(".exe") { want.clone() } else { format!("{want}.exe") };
    visible_windows()
        .into_iter()
        .filter(|&hwnd| match pid_of(hwnd).and_then(exe_of) {
            Some(exe) => {
                let exe = exe.to_ascii_lowercase();
                (exe == want_exe || exe == want
                    || browser_name(&exe).is_some_and(|n| n.eq_ignore_ascii_case(app)))
                    && !is_shell_window(&exe, hwnd)
            }
            None => false,
        })
        .collect()
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

/// Walk a window and return the first thing that looks like a web address.
///
/// May block for as long as the browser takes to answer, so it belongs on the
/// reader thread or the animation thread -- never on the clock.
fn read_url_with(ui: &UIAutomation, hwnd: HWND) -> Option<String> {
    let root = ui.element_from_handle(Handle::from(hwnd.0 as isize)).ok()?;
    let walker = ui.get_control_view_walker().ok()?;
    let mut budget = 200usize;
    find_url(&walker, &root, 0, 8, &mut budget)
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

    /// The close re-check is about the SITE, not the page.
    ///
    /// Demanding the identical URL made closing refuse almost every time on
    /// the sites people actually limit: YouTube rewrites its address on every
    /// click, so it had always moved on by the time the cat got there.
    #[test]
    fn the_close_check_follows_the_site_not_the_page() {
        let want = fk_core::domain::normalize("https://www.youtube.com/watch?v=aaa");
        assert_eq!(want, "youtube.com");

        // Navigated within the site: still the thing we were asked to close.
        assert!(fk_core::domain::matches_domain("https://www.youtube.com/watch?v=zzz", &want));
        assert!(fk_core::domain::matches_domain("https://m.youtube.com/feed", &want));
        assert!(fk_core::domain::matches_domain("youtube.com/results?q=x", &want));

        // Genuinely somewhere else: leave it alone.
        assert!(!fk_core::domain::matches_domain("https://github.com/x", &want));
        assert!(!fk_core::domain::matches_domain("https://notyoutube.com/", &want));
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
