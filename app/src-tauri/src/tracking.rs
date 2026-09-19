//! The running tracker, and the snapshot the windows render from.

use std::sync::Mutex;

use chrono::{DateTime, Duration, Local};
use fk_core::activity::{Activity, ActivityProbe, AppInfo, CloseOutcome};
use fk_core::rules::TargetKey;
use fk_core::tracker::Action;
use fk_core::{human_secs, store, Config, Tracker};
use serde::Serialize;

/// The name to show for a tracking key.
///
/// App keys hold the BUNDLE ID (`com.apple.ical`), but every visible surface —
/// rows, bubbles, the diary — should say `Calendar`. Mixing the two is what
/// stopped app limits working at all: the expiry looked up a rule by matching
/// the bundle id against `app_name`, found nothing, and silently gave up.
fn display_label(tracker: &Tracker, key: &TargetKey) -> String {
    match key {
        TargetKey::Site(d) => d.clone(),
        TargetKey::App(id) => tracker
            .config
            .apps
            .iter()
            .find(|r| r.app_id.eq_ignore_ascii_case(id))
            .map(|r| r.app_name.clone())
            .unwrap_or_else(|| id.clone()),
    }
}

/// What the cat is about to go and deal with.
#[derive(Debug, Clone)]
pub enum PendingClose {
    /// A browser tab, which gets closed.
    Tab(String, fk_core::activity::TabRef),
    /// A native app, which gets hidden -- never quit, in case of unsaved work.
    App(String, String),
}

/// Idle this long and the cat finds something else to do.
const IDLE_AFTER: u64 = 75;
/// How long it sticks with each activity before moving on.
const IDLE_SPELL: u64 = 120;

/// How long the cat stays cross after closing something.
const ANGRY_FOR: i64 = 8;

/// Our own bundle id. Time spent in FocusKitty's own windows is not screen time
/// worth policing, and counting it would make the popover itself a distraction.
/// How FocusKitty appears to its OWN probe.
///
/// macOS and Android answer with a bundle/package id; Windows answers with an
/// executable name. Getting this wrong is silent and total: `is_self` never
/// matches, so the moment you click the cat -- which gives the popover focus
/// -- the tick takes the "you switched to something new" branch and forgets
/// what you were actually looking at. "Watch this" then has nothing to offer,
/// which is precisely what it did on Windows.
#[cfg(target_os = "windows")]
const SELF_BUNDLE: &str = "focuskitty.exe";
#[cfg(not(target_os = "windows"))]
const SELF_BUNDLE: &str = "com.siva.focuskitty";

/// How long a line stays in the cat's bubble.
const SAY_FOR: i64 = 8;

/// Seconds between one over-limit app being sent away and the next attempt.
///
/// Long enough that the launcher has come forward and a single tap on the
/// notification shade is not fought over, short enough that reopening the app
/// puts you straight back out.
#[cfg(target_os = "android")]
const KICK_COOLDOWN: i64 = 4;

pub struct AppState {
    pub tracker: Mutex<Tracker>,
    pub inner: Mutex<Inner>,
}

#[derive(Default)]
pub struct Inner {
    pub events: Vec<String>,
    /// Enforcement pause. Time still accrues -- only the closing stops -- so
    /// the diary stays honest about the hour you spent while paused.
    pub paused_until: Option<DateTime<Local>>,
    pub sleeping: bool,
    pub last_expiry: Option<DateTime<Local>>,
    pub warning: bool,
    pub say: Option<String>,
    pub current_label: Option<String>,
    pub tick_count: u64,
    /// What the probe last saw. Commands run on worker threads and the macOS
    /// probe is main-thread only, so they read this instead of probing.
    pub current_activity: Option<Activity>,
    /// The last activity that actually had a browser tab, ignoring our own
    /// windows. Opening the popover makes FocusKitty frontmost, so `current`
    /// becomes tabless the instant you go to press "Watch this site" -- which
    /// is exactly when you need to know what you were looking at.
    pub last_tab: Option<Activity>,
    /// Refreshed on the main thread; the picker reads the cache.
    pub apps: Vec<AppInfo>,
    /// The last thing in front that was not us.
    pub last_counted: Option<Activity>,
    /// Consecutive ticks with FocusKitty itself frontmost.
    pub self_front: u64,
    /// When each target was last closed, so the popover can say so for a beat.
    pub closed_at: std::collections::HashMap<String, DateTime<Local>>,
    /// Raised when a tab was just closed, so the cat can jump.
    /// Something waiting to be dealt with on the swipe's impact frame.
    pub pending_close: Option<PendingClose>,
    /// Last thing logged, so the change-log does not repeat itself.
    pub last_logged: Option<String>,
    /// Seconds with nothing watched in front; the cat gets bored.
    pub idle_for: u64,
    /// When the cat last said something, so a line clears itself instead of
    /// hanging over the cat an hour after it mattered.
    pub said_at: Option<DateTime<Local>>,
    /// When the cat last sent an over-limit app to the home screen.
    ///
    /// Android enforcement has to keep working after the first time: a tab
    /// stays closed, but an app is one tap away from being reopened. So the
    /// check runs every tick and this only stops it firing repeatedly while
    /// the launcher is still coming forward.
    pub last_kick: Option<DateTime<Local>>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Row {
    pub label: String,
    pub used: u64,
    pub limit: u64,
    pub used_human: String,
    pub limit_human: String,
    /// Seconds left before this one trips. The number people actually watch.
    pub remaining: u64,
    pub remaining_human: String,
    /// True when this is what is in front right now.
    pub active: bool,
    /// What is happening to this rule, in plain words:
    /// "watching" | "time up" | "closed the tab" | "paused" | "waiting".
    pub status: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Snapshot {
    pub pose: String,
    pub say: Option<String>,
    pub current: Option<String>,
    pub rows: Vec<Row>,
    pub paused: bool,
    pub paused_mins_left: i64,
    pub sleeping: bool,
    pub watching: usize,
    pub writing: bool,
    /// What "watch this" would add right now, and whether it is an app.
    pub target_label: Option<String>,
    pub target_is_app: bool,
}

impl Inner {
    /// Give the cat something to say, and remember when.
    ///
    /// Both halves in one place: a message written without its timestamp
    /// would never expire, which is how a bubble ends up reporting a limit
    /// that ran out an hour ago.
    fn speak(&mut self, msg: impl Into<String>) {
        self.say = Some(msg.into());
        self.said_at = Some(Local::now());
    }
}

impl AppState {
    pub fn new() -> anyhow::Result<Self> {
        // Starting empty is only correct when there is genuinely nothing saved.
        // Anything else -- a parse failure, a permissions problem -- must be
        // said out loud rather than quietly costing the user their rules.
        let config = match store::load_config() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("could not load config ({e}); starting empty");
                Config::starter()
            }
        };
        // Today's totals survive a restart. Without this every limit reset
        // itself the moment the app was reopened -- and an app that has just
        // thrown you out of something is exactly the app you are tempted to
        // restart, so the limit was one relaunch away from meaning nothing.
        let mut tracker = Tracker::new(config, Local::now());
        match store::load_day(tracker.day) {
            Ok(Some(log)) => tracker.state = log.totals.into_iter().collect(),
            Ok(None) => {}
            Err(e) => tracing::error!("could not load today's totals ({e})"),
        }

        Ok(Self {
            tracker: Mutex::new(tracker),
            inner: Mutex::new(Inner::default()),
        })
    }

    pub fn snapshot(&self) -> Snapshot {
        let tracker = self.tracker.lock().unwrap();
        let inner = self.inner.lock().unwrap();
        let now = Local::now();

        let paused = inner.paused_until.is_some_and(|t| t > now);
        let mut rows: Vec<Row> = tracker
            .config
            .sites
            .iter()
            .filter(|r| r.enabled)
            .map(|r| {
                let key = TargetKey::site(&r.domain);
                let left = tracker.remaining(&key).unwrap_or(0);
                Row {
                    label: r.domain.clone(),
                    used: tracker.used(&key),
                    limit: r.daily_limit_secs,
                    used_human: human_secs(tracker.used(&key)),
                    limit_human: human_secs(r.daily_limit_secs),
                    remaining: left,
                    remaining_human: human_secs(left),
                    active: false,
                    status: String::new(),
                }
            })
            .chain(tracker.config.apps.iter().filter(|r| r.enabled).map(|r| {
                let key = TargetKey::app(&r.app_id);
                let left = tracker.remaining(&key).unwrap_or(0);
                Row {
                    label: r.app_name.clone(),
                    used: tracker.used(&key),
                    limit: r.daily_limit_secs,
                    used_human: human_secs(tracker.used(&key)),
                    limit_human: human_secs(r.daily_limit_secs),
                    remaining: left,
                    remaining_human: human_secs(left),
                    active: false,
                    status: String::new(),
                }
            }))
            .collect();
        // Mark whatever is in front, then put the worst offender first.
        if let Some(cur) = inner.current_label.as_ref() {
            for r in rows.iter_mut() {
                if r.label.eq_ignore_ascii_case(cur) {
                    r.active = true;
                }
            }
        }

        // The state each rule is in, said plainly. A bare countdown hides the
        // most important moment -- the close -- the instant it has happened.
        for r in rows.iter_mut() {
            let just_closed = inner
                .closed_at
                .get(&r.label)
                .is_some_and(|t| now - *t < Duration::seconds(12));
            r.status = if just_closed {
                if r.label.contains(".") { "closed the tab".into() } else { "hid the app".into() }
            } else if r.used >= r.limit {
                if paused { "time up · paused".into() } else { "time up".into() }
            } else if paused {
                "paused".into()
            } else if r.active {
                "watching".into()
            } else {
                "waiting".into()
            };
        }
        rows.sort_by(|a, b| b.active.cmp(&a.active).then(b.used.cmp(&a.used)));

        // Writing = a watched target is in front with its clock running. That
        // is exactly when the cat has something to record.
        // A sleeping cat is not writing. Leaving this true while asleep kept
        // the diary open AND let the writing rig override the sleep pose's
        // legs and head every frame, which is what threw the body in the air.
        let writing = !inner.sleeping
            && inner.current_label.as_ref().is_some_and(|label| {
                rows.iter().any(|r| r.label.eq_ignore_ascii_case(label))
            });

        let pose = if inner.sleeping {
            "sleep"
        } else if inner.last_expiry.is_some_and(|t| now - t < Duration::seconds(ANGRY_FOR)) {
            "angry"
        } else if inner.warning {
            "confront"
        } else if inner.idle_for > IDLE_AFTER {
            // With nothing to watch, the cat gets on with its own day. The
            // slot is derived from how long it has been idle rather than
            // rolled each tick, so it settles into an activity instead of
            // flickering between them.
            const ACTS: [&str; 3] = ["bored", "read", "eat"];
            let slot = ((inner.idle_for - IDLE_AFTER) / IDLE_SPELL) as usize;
            if slot >= ACTS.len() { "sleep" } else { ACTS[slot] }
        } else {
            "sit"
        };

        // What a one-tap "watch this" would act on. Uses the last thing that
        // was in front that was not us, so opening the panel does not erase it.
        let target = inner.last_counted.clone();
        let target_is_app = target.as_ref().is_some_and(|a| a.tab.is_none());
        let target_label = target.as_ref().map(|a| match &a.tab {
            Some(t) => fk_core::domain::normalize(&t.url),
            None => a.app_name.clone(),
        }).filter(|l| !l.is_empty());

        Snapshot {
            pose: pose.into(),
            target_label,
            target_is_app,
            // Only what the cat is saying NOW. A line that has had its moment
            // is not news, and on a phone it is covering something.
            say: inner
                .said_at
                .filter(|t| now - *t < Duration::seconds(SAY_FOR))
                .and(inner.say.clone()),
            current: inner.current_label.clone(),
            watching: rows.len(),
            rows,
            paused,
            writing,
            paused_mins_left: inner
                .paused_until
                .map(|t| (t - now).num_minutes().max(0))
                .unwrap_or(0),
            sleeping: inner.sleeping,
        }
    }
}

/// One second of tracking. MUST be called on the main thread: the macOS probe
/// drives browsers through OSAScript, which refuses to run anywhere else.
pub fn tick(state: &AppState, probe: &dyn ActivityProbe) {
    let now = Local::now();
    let current = probe.current();

    let needs_bg = {
        let t = state.tracker.lock().unwrap();
        t.config.sites.iter().any(|r| {
            r.enabled && r.background_mode != fk_core::BackgroundMode::ForegroundOnly
        })
    };

    // Scanning every tab costs ~180ms, so it runs at a tenth of the tick rate.
    let open_tabs = {
        let mut inner = state.inner.lock().unwrap();
        inner.tick_count += 1;
        let should = needs_bg && inner.tick_count % 10 == 1;
        drop(inner);
        if should { probe.open_tabs() } else { Vec::new() }
    };

    // Opening our own controls must not pause your timer.
    //
    // The popover takes focus, so Chrome stops being frontmost and the clock
    // would stop -- meaning a video kept playing while the tracker looked away.
    // While FocusKitty is in front we keep charging whatever was in front
    // before it, bounded so that leaving the panel open forever does not bill
    // you for a video you stopped watching.
    const SELF_GRACE_TICKS: u64 = 120;
    #[allow(unused_assignments)]
    let mut carried = None;
    let counted = {
        let mut inner = state.inner.lock().unwrap();
        let is_self = current
            .as_ref()
            .is_some_and(|a| a.app_id.eq_ignore_ascii_case(SELF_BUNDLE));
        if is_self {
            inner.self_front += 1;
            carried = if inner.self_front <= SELF_GRACE_TICKS {
                inner.last_counted.clone()
            } else {
                None
            };
            carried.as_ref()
        } else {
            inner.self_front = 0;
            inner.last_counted = current.clone();
            current.as_ref()
        }
    };

    let actions = {
        let mut tracker = state.tracker.lock().unwrap();
        tracker.tick(now, counted, &open_tabs)
    };

    let mut inner = state.inner.lock().unwrap();

    // Never report our own windows as "what you are doing".
    let is_self = current
        .as_ref()
        .is_some_and(|a| a.app_id.eq_ignore_ascii_case(SELF_BUNDLE));
    if !is_self {
        inner.current_activity = current.clone();
        if let Some(a) = current.as_ref() {
            if a.tab.is_some() {
                inner.last_tab = Some(a.clone());
            }
        }
    }
    let label_from = if is_self { inner.last_counted.clone() } else { current.clone() };
    inner.current_label = label_from.as_ref().map(|a| {
        a.tab
            .as_ref()
            .and_then(|t| fk_core::domain::host_of(&t.url))
            .unwrap_or_else(|| a.app_name.clone())
    });
    let enforcing = !inner.paused_until.is_some_and(|t| t > now);
    inner.warning = false;
    if inner.current_label.is_some() { inner.idle_for = 0; } else { inner.idle_for += 1; }

    for action in actions {
        match action {
            Action::Warn { key, remaining_secs } => {
                let name = {
                    let tracker = state.tracker.lock().unwrap();
                    display_label(&tracker, &key)
                };
                let msg = format!("{name} — {} left", human_secs(remaining_secs));
                inner.warning = true;
                inner.speak(msg.clone());
                inner.events.push(msg);
            }
            Action::Notice { key, open_secs } => {
                let name = {
                    let tracker = state.tracker.lock().unwrap();
                    display_label(&tracker, &key)
                };
                let msg = format!("{name} has been open behind you for {}", human_secs(open_secs));
                inner.speak(msg.clone());
                inner.events.push(msg);
            }
            Action::Expire { key, tab } => {
                let label = {
                    let tracker = state.tracker.lock().unwrap();
                    display_label(&tracker, &key)
                };
                inner.last_expiry = Some(now);
                if !enforcing {
                    inner.speak(format!("{label} is over — but you paused me"));
                    inner.events.push(format!("{label} over limit (paused)"));
                    continue;
                }
                match tab {
                    // Queued, not closed here. The caller plays the swipe and
                    // performs the close on its impact frame, so the cat is
                    // visibly what shut the tab rather than a bystander
                    // reacting after the fact.
                    Some(t) => {
                        inner.pending_close = Some(PendingClose::Tab(label.clone(), t));
                    }
                    // A native app has no tab. It still gets dealt with: the
                    // cat goes over and hides it, which is reversible and
                    // loses nothing, rather than the limit doing nothing at all.
                    None => {
                        // Match on the BUNDLE ID, which is what the key holds.
                        let proc = {
                            let tracker = state.tracker.lock().unwrap();
                            match &key {
                                TargetKey::App(id) => tracker
                                    .config
                                    .apps
                                    .iter()
                                    .find(|r| r.app_id.eq_ignore_ascii_case(id))
                                    .map(|r| r.app_name.clone()),
                                _ => None,
                            }
                        };
                        match proc {
                            Some(name) => {
                                inner.pending_close =
                                    Some(PendingClose::App(name.clone(), name));
                            }
                            None => {
                                inner.speak(format!("{label} is over its limit"));
                                inner.events.push(format!("{label} over limit"));
                            }
                        }
                    }
                }
            }
        }
    }

    // --- Android: a limit has to actually stop something --------------------
    //
    // There is no tab to close on a phone, so the limit is enforced by putting
    // the app down -- the mobile form of the same act. This reads the live
    // state rather than the one-shot Expire action, because an app, unlike a
    // closed tab, is one tap from being reopened, and a limit you can walk
    // straight back through is not a limit.
    #[cfg(target_os = "android")]
    if enforcing && !inner.sleeping {
        let front = inner.current_activity.as_ref().map(|a| a.app_id.clone());
        if let Some(app_id) = front {
            let key = TargetKey::app(&app_id);
            let spent = {
                let tracker = state.tracker.lock().unwrap();
                tracker.config.app_for_id(&app_id).is_some()
                    && tracker.state.get(&key).is_some_and(|st| st.expired)
            };
            let cooling = inner
                .last_kick
                .is_some_and(|t| now - t < Duration::seconds(KICK_COOLDOWN));
            if spent && !cooling {
                match fk_probe_android::go_home() {
                    Ok(()) => {
                        let label = {
                            let tracker = state.tracker.lock().unwrap();
                            display_label(&tracker, &key)
                        };
                        inner.last_kick = Some(now);
                        // Drives the cat's angry pose, the same as a close does.
                        inner.last_expiry = Some(now);
                        inner.speak(format!("{label} is done for today"));
                        inner.events.push(format!("sent {label} away"));
                        tracing::info!("{label} is over its limit; sent it away");
                    }
                    Err(e) => tracing::error!("could not send {app_id} away: {e}"),
                }
            }
        }
    }

    // Log every CHANGE in what is detected, so a tab switch that fails to
    // resume shows up as the probe reporting the wrong thing rather than as a
    // guess about why.
    {
        let seen = inner
            .current_activity
            .as_ref()
            .map(|a| match &a.tab {
                Some(t) => format!("{} [{}]", fk_core::domain::normalize(&t.url), a.app_name),
                None => format!("(app) {}", a.app_name),
            })
            .unwrap_or_else(|| "(nothing)".into());
        if inner.last_logged.as_deref() != Some(seen.as_str()) {
            tracing::info!("front -> {seen}");
            inner.last_logged = Some(seen);
        }
    }

    // Apply anything the overlay queued. One writer owns the config.
    if let Some(req) = store::take_pending_watch() {
        let mut tracker = state.tracker.lock().unwrap();
        tracker
            .config
            .apps
            .retain(|r| !r.app_id.eq_ignore_ascii_case(&req.app_id));
        tracker.config.apps.push(fk_core::rules::AppRule {
            app_id: req.app_id.clone(),
            app_name: req.app_name.clone(),
            daily_limit_secs: req.minutes.clamp(1, 24 * 60) * 60,
            session_limit_secs: None,
            warn_lead_secs: 300,
            enabled: true,
        });
        let _ = store::save_config(&tracker.config);
        tracing::info!("watching {} ({}m), queued from the overlay", req.app_name, req.minutes);
    }

    // ...and anything it asked to stop watching. Dropping the rule alone would
    // leave the day's total behind, so a rule added again an hour later would
    // start already spent; the state goes with it.
    if let Some(app_id) = store::take_pending_unwatch() {
        let mut tracker = state.tracker.lock().unwrap();
        let before = tracker.config.apps.len();
        tracker.config.apps.retain(|r| !r.app_id.eq_ignore_ascii_case(&app_id));
        if tracker.config.apps.len() != before {
            tracker.state.remove(&TargetKey::app(&app_id));
            let _ = store::save_config(&tracker.config);
            tracing::info!("stopped watching {app_id}, queued from the overlay");
        }
    }

    // Publish a status snapshot for the Android overlay.
    //
    // The overlay runs in its own WebView with no Tauri bridge, so it cannot
    // ask for state. Rust owns the config and the totals, so it writes a small
    // read-only snapshot and the overlay renders whatever it finds.
    #[cfg(target_os = "android")]
    if inner.tick_count % 2 == 0 {
        drop(inner);
        let snap = state.snapshot();
        let path = store::data_dir().join("status.json");
        if let Ok(json) = serde_json::to_string(&snap) {
            let _ = std::fs::create_dir_all(store::data_dir());
            let _ = std::fs::write(path, json);
        }
        inner = state.inner.lock().unwrap();
    }

    // A line every 10s so tracking can be checked against a stopwatch rather
    // than argued about.
    if inner.tick_count % 10 == 0 {
        let tracker = state.tracker.lock().unwrap();
        let front = inner.current_label.clone().unwrap_or_else(|| "-".into());
        let totals: Vec<String> = tracker
            .config
            .sites
            .iter()
            .filter(|r| r.enabled)
            .map(|r| {
                let k = TargetKey::site(&r.domain);
                format!("{}={}s", r.domain, tracker.used(&k))
            })
            .collect();
        if !totals.is_empty() {
            tracing::info!("front={front} | {}", totals.join(" "));
        }
    }

    // The app list changes rarely; half a minute is plenty, and it must be
    // gathered here because this is the only place running on the main thread.
    if inner.apps.is_empty() || inner.tick_count % 30 == 0 {
        inner.apps = probe.installed_apps();
    }

    // A minute of crash costs a minute, not a day.
    if inner.tick_count % 60 == 0 {
        let tracker = state.tracker.lock().unwrap();
        if let Err(e) = store::save_day(&tracker, &inner.events) {
            tracing::warn!("saving today: {e}");
        }
    }
}
