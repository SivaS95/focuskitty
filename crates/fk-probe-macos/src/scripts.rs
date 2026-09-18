//! The AppleScript FocusKitty runs against Chrome and Safari.
//!
//! Each browser gets one script, compiled once at startup and re-executed, so
//! the 1 Hz tick never pays the cost of spawning `osascript`.
//!
//! Every handler is written to fail soft: a browser with no windows, a tab
//! mid-navigation, or a private window must all return "nothing to see"
//! rather than raising, because AppleScript errors surface as hard failures.

/// Shared host-matching, kept identical in spirit to `fk_core::domain`.
///
/// It lives inside the script because the close must be atomic: the check and
/// the close have to happen in one round trip, or the user can switch tabs in
/// between and we close the wrong thing.
const HOST_HELPER: &str = r#"
-- Which window does macOS actually have focused?
--
-- Chrome's own `front window` is its INTERNAL ordering, and that only updates
-- when you click. With two windows open it happily reports the one you are not
-- looking at, so time is charged to the wrong site until you click a tab.
--
-- System Events knows the truth, but needs Accessibility permission. The whole
-- thing is wrapped in `try`: if permission is refused we fall back to Chrome's
-- answer, which is right whenever there is only one window.
on focusedWindowName()
    try
        tell application "System Events"
            set p to first application process whose frontmost is true
            return name of first window of p
        end tell
    end try
    return missing value
end focusedWindowName

on hostOf(u)
    set t to u
    if t starts with "https://" then
        set t to text 9 thru -1 of t
    else if t starts with "http://" then
        set t to text 8 thru -1 of t
    end if
    set AppleScript's text item delimiters to "/"
    set t to text item 1 of t
    set AppleScript's text item delimiters to ""
    set AppleScript's text item delimiters to "?"
    set t to text item 1 of t
    set AppleScript's text item delimiters to ""
    if t contains "@" then
        set AppleScript's text item delimiters to "@"
        set t to last text item of t
        set AppleScript's text item delimiters to ""
    end if
    set AppleScript's text item delimiters to ":"
    set t to text item 1 of t
    set AppleScript's text item delimiters to ""
    if t starts with "www." then set t to text 5 thru -1 of t
    return t
end hostOf

on hostMatches(u, d)
    try
        set h to hostOf(u)
        if h is d then return true
        if h ends with ("." & d) then return true
    end try
    return false
end hostMatches
"#;

pub fn chrome() -> String {
    format!(
        r#"{HOST_HELPER}
on activeTab()
    set wanted to my focusedWindowName()
    tell application "Google Chrome"
        if (count of windows) is 0 then return missing value
        set w to front window
        if wanted is not missing value then
            repeat with ww in windows
                try
                    if (name of ww) is wanted then
                        set w to ww
                        exit repeat
                    end if
                end try
            end repeat
        end if
        try
            if (mode of w) is "incognito" then return missing value
        end try
        set t to active tab of w
        set u to ""
        try
            set u to (URL of t) as text
        end try
        set ti to ""
        try
            set ti to (title of t) as text
        end try
        return {{(id of t) as text, u, ti}}
    end tell
end activeTab

-- Bulk property access: "URL of tabs of w" fetches every URL in ONE Apple
-- Event. Walking tabs one at a time costs an event per property, which measured
-- at 1.1s for 35 tabs -- long enough to visibly stall the cat.
-- Where the focused window is, and which tab is active in it.
on tabRect()
    set wanted to my focusedWindowName()
    tell application "Google Chrome"
        if (count of windows) is 0 then return missing value
        set w to front window
        if wanted is not missing value then
            repeat with ww in windows
                try
                    if (name of ww) is wanted then
                        set w to ww
                        exit repeat
                    end if
                end try
            end repeat
        end if
        set b to bounds of w
        return {{item 1 of b, item 2 of b, item 3 of b, item 4 of b, ¬
                 active tab index of w, count of tabs of w}}
    end tell
end tabRect

on allTabs()
    set out to {{}}
    tell application "Google Chrome"
        repeat with w in windows
            try
                if (mode of w) is not "incognito" then
                    set ids to (id of tabs of w)
                    set urls to (URL of tabs of w)
                    set titles to (title of tabs of w)
                    set end of out to {{ids, urls, titles}}
                end if
            end try
        end repeat
    end tell
    return out
end allTabs

-- Re-verify then close, in one round trip.
on closeTab(savedId, savedDomain)
    tell application "Google Chrome"
        repeat with w in windows
            try
                if (mode of w) is not "incognito" then
                    repeat with t in tabs of w
                        try
                            if ((id of t) as text) is savedId then
                                if my hostMatches((URL of t) as text, savedDomain) then
                                    close t
                                    return "closed"
                                end if
                            end if
                        end try
                    end repeat
                end if
            end try
        end repeat
    end tell
    return "nomatch"
end closeTab
"#
    )
}

/// System Events: where a native app's window is, and how to get it off screen.
///
/// Needs Accessibility permission. Everything is wrapped so that a refusal
/// degrades to "cannot do that" rather than raising.
pub fn system() -> String {
    r#"
on appRect(procName)
    try
        tell application "System Events" to tell process procName
            set p to position of front window
            set sz to size of front window
            return {item 1 of p, item 2 of p, ¬
                    (item 1 of p) + (item 1 of sz), (item 2 of p) + (item 2 of sz), 1, 1}
        end tell
    end try
    return missing value
end appRect

on hideApp(procName)
    try
        tell application "System Events" to set visible of process procName to false
        return "hidden"
    end try
    return "nomatch"
end hideApp
"#
    .to_string()
}

pub fn safari() -> String {
    format!(
        r#"{HOST_HELPER}
on activeTab()
    set wanted to my focusedWindowName()
    tell application "Safari"
        if (count of windows) is 0 then return missing value
        set w to front window
        if wanted is not missing value then
            repeat with ww in windows
                try
                    if (name of ww) is wanted then
                        set w to ww
                        exit repeat
                    end if
                end try
            end repeat
        end if
        set t to current tab of w
        set u to ""
        try
            set u to (URL of t) as text
        end try
        set ti to ""
        try
            set ti to (name of t) as text
        end try
        return {{("idx:" & ((index of t) as text)), u, ti}}
    end tell
end activeTab

-- Same bulk trick as Chrome. Safari has no tab id, so the index list stands in.
on tabRect()
    tell application "Safari"
        if (count of windows) is 0 then return missing value
        set w to front window
        set b to bounds of w
        set idx to 1
        try
            set idx to index of current tab of w
        end try
        return {{item 1 of b, item 2 of b, item 3 of b, item 4 of b, idx, count of tabs of w}}
    end tell
end tabRect

on allTabs()
    set out to {{}}
    tell application "Safari"
        repeat with w in windows
            try
                set idxs to (index of tabs of w)
                set urls to (URL of tabs of w)
                set titles to (name of tabs of w)
                set end of out to {{idxs, urls, titles}}
            end try
        end repeat
    end tell
    return out
end allTabs

-- Safari exposes no tab id, so identity is index + domain. The domain check is
-- what keeps a re-indexed tab from being closed by mistake.
on closeTab(savedId, savedDomain)
    tell application "Safari"
        repeat with w in windows
            try
                repeat with t in tabs of w
                    try
                        if ("idx:" & ((index of t) as text)) is savedId then
                            if my hostMatches((URL of t) as text, savedDomain) then
                                close t
                                return "closed"
                            end if
                        end if
                    end try
                end repeat
            end try
        end repeat
    end tell
    return "nomatch"
end closeTab
"#
    )
}
