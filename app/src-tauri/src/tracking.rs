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
/// A short name for a key, for the log.
fn k_label(k: &fk_core::rules::TargetKey) -> String {
    match k {
        fk_core::rules::TargetKey::Site(d) => d.clone(),
        fk_core::rules::TargetKey::App(a) => a.clone(),
    }
}

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
const IDLE_AFTER: u64 = 40;

/// What the cat does with its own time, and for how long (seconds, min..max).
///
/// Ordered by nothing: the next one is drawn at random, so the cat does not
/// march through a fixed programme. Sleep is in here like any other activity,
/// which is what makes it wake up again -- before, sleep was the last slot and
/// the cat stayed there until you came back.
const IDLE_ACTS: &[(&str, u64, u64)] = &[
    ("bored", 18, 40),
    ("read", 50, 110),
    ("eat", 20, 40),
    ("sleep", 70, 150),
    ("sit", 15, 35),
    // Long enough for the whole back-and-forth burst the host performs.
    ("wander", 20, 30),
];

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
    /// Seconds with nothing WATCHED in front; the cat gets on with its day.
    pub idle_for: u64,
    /// What it is currently doing with that time, and the tick it ends on.
    pub idle_act: Option<String>,
    pub idle_until: u64,
    /// A walk the host should take the cat on: how far across the screen, 0..1.
    pub pending_wander: Option<f64>,
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

/// Enough randomness to stop the cat being predictable, without a dependency.
fn roll(range: std::ops::Range<u64>) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let mut x = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(0x2545F4914F6CDD1D)
        | 1;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    let span = range.end.saturating_sub(range.start).max(1);
    range.start + x % span
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
        // ORDER MATTERS: `inner` before `tracker`, always, everywhere.
        //
        // The tick takes inner and then reaches for tracker while still
        // holding it. This took them the other way round, and two threads
        // taking the same two locks in opposite orders deadlock the moment
        // they interleave.
        //
        // The thread that polls this once a second is the quick controls
        // panel, which is why the clock stalled precisely when the panel was
        // open -- and why it seemed to recover when it was closed. Nothing to
        // do with idleness, browsers, or the event loop: two threads each
        // holding what the other was waiting for.
        let inner = self.inner.lock().unwrap();
        let tracker = self.tracker.lock().unwrap();
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
        } else if let Some(act) = inner.idle_act.as_deref() {
            // Whatever it chose to do with its own time. Held for the length
            // the tick decided, so it settles into an activity instead of
            // flickering between them -- and "wander" is a walk, which the
            // host performs; the animal itself walks.
            if act == "wander" { "walk" } else { act }
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
            // Only REPLACE the memory when there is something to replace it
            // with. Nothing in front means the screen is locked, or focus has
            // fallen somewhere that is not an application -- neither of which
            // means "you have finished with what you were doing", and both of
            // which would otherwise erase the target that "Watch this" offers.
            if current.is_some() {
                inner.last_counted = current.clone();
            }
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
    // Idle means "nothing I am WATCHING is in front" -- not "the screen is
    // empty". That distinction is the whole difference between a cat with a
    // life and a cat that sits still forever: something is almost always in
    // front of you, so the old test left the entire repertoire unreachable.
    let watching_now = inner.current_label.as_ref().is_some_and(|label| {
        let tracker = state.tracker.lock().unwrap();
        tracker
            .config
            .sites
            .iter()
            .any(|r| r.enabled && r.domain.eq_ignore_ascii_case(label))
            || tracker
                .config
                .apps
                .iter()
                .any(|r| r.enabled && (r.app_name.eq_ignore_ascii_case(label)
                    || r.app_id.eq_ignore_ascii_case(label)))
    });
    if watching_now {
        inner.idle_for = 0;
        inner.idle_act = None;
        inner.idle_until = 0;
    } else {
        inner.idle_for += 1;
    }

    // Pick the next thing to do, whenever the last one has run its course.
    if inner.idle_for > IDLE_AFTER && !inner.sleeping && inner.tick_count >= inner.idle_until {
        let last = inner.idle_act.clone();
        // Never the same thing twice running -- that is what "stuck in bored
        // for ten minutes" looked like.
        let mut pick = IDLE_ACTS[roll(0..IDLE_ACTS.len() as u64) as usize];
        if Some(pick.0.to_string()) == last {
            let i = (IDLE_ACTS.iter().position(|a| a.0 == pick.0).unwrap_or(0) + 1) % IDLE_ACTS.len();
            pick = IDLE_ACTS[i];
        }
        let (name, lo, hi) = pick;
        inner.idle_act = Some(name.to_string());
        inner.idle_until = inner.tick_count + roll(lo..hi + 1);
        if name == "wander" {
            // Somewhere well across the screen. A two-step shuffle reads as a
            // glitch; a cat crossing the desk reads as a cat.
            inner.pending_wander = Some(roll(5..96) as f64 / 100.0);
        }
        tracing::debug!("idle -> {name} until tick {}", inner.idle_until);
    }

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
                        // The bundle id, not the display name. Names are
                        // localised and can differ from what the system calls
                        // the process; the id is what the app IS.
                        let proc = {
                            let tracker = state.tracker.lock().unwrap();
                            match &key {
                                TargetKey::App(id) => tracker
                                    .config
                                    .apps
                                    .iter()
                                    .find(|r| r.app_id.eq_ignore_ascii_case(id))
                                    .map(|r| (r.app_name.clone(), r.app_id.clone())),
                                _ => None,
                            }
                        };
                        match proc {
                            Some((name, id)) => {
                                inner.pending_close = Some(PendingClose::App(name, id));
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

    // A heartbeat, every five seconds.
    //
    // Six fixes have gone out for a clock that stops, each argued from what
    // somebody saw on a machine nobody could inspect. This is the record that
    // ends that: when it ran, how long since the last one, what was in front,
    // and what the thing being watched has been charged. A gap in these
    // timestamps IS the bug, and it names itself.
    if inner.tick_count % 5 == 0 {
        let front = inner.current_label.clone().unwrap_or_else(|| "(nothing)".into());
        let charged = {
            let tracker = state.tracker.lock().unwrap();
            tracker
                .config
                .sites
                .iter()
                .map(|r| fk_core::rules::TargetKey::site(&r.domain))
                .chain(
                    tracker
                        .config
                        .apps
                        .iter()
                        .map(|r| fk_core::rules::TargetKey::app(&r.app_id)),
                )
                .map(|k| format!("{}={}s", k_label(&k), tracker.used(&k)))
                .collect::<Vec<_>>()
                .join(" ")
        };
        tracing::info!("tick {} front={front:?} {charged}", inner.tick_count);
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
        // Set now means from now. Without this, watching an app on the phone
        // charged it for everything it had already done today, so a fresh
        // limit was over before the panel had finished redrawing.
        tracker.state.remove(&TargetKey::app(&req.app_id));
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
