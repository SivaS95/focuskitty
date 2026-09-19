//! The tick loop: turns a stream of observations into timers, warnings and closes.
//!
//! Deliberately pure. `tick` takes what was observed and returns what should
//! happen; it never talks to a browser itself. That is what lets the whole
//! thing — including the close-race — be tested without a real Chrome.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Local, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::activity::{Activity, TabRef};
use crate::rules::{BackgroundMode, Config, TargetKey};

/// Ticks further apart than this count as a break in observation.
///
/// A gap this long means we stopped watching — the lid closed, the machine
/// slept, the day rolled over. We cannot know what happened in the dark, so
/// nothing is charged for it. Crediting even the clamped gap would invent
/// usage on every wake.
const MAX_TICK_GAP_MS: i64 = 5_000;

/// The cat warns again at one minute, whatever the configured lead is.
const FINAL_WARN_SECS: u64 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Put something in the thought bubble.
    Warn {
        key: TargetKey,
        remaining_secs: u64,
    },
    /// Limit reached. `tab` is `Some` when there is something closable.
    Expire {
        key: TargetKey,
        tab: Option<TabRef>,
    },
    /// The cat noticed a tab left open behind you.
    Notice {
        key: TargetKey,
        open_secs: u64,
    },
}

/// Everything here is MILLISECONDS.
///
/// Seconds were losing about half of all tracked time: the tick sleeps a
/// second but scheduling jitter makes the real gap wobble around 1.0s, and
/// truncating to whole seconds charged nothing for every gap that landed
/// under one. The fractions were never recovered. Milliseconds keep them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TargetState {
    #[serde(default)]
    pub used_ms: u64,
    #[serde(default)]
    pub session_ms: u64,
    /// Extra time granted by snoozing.
    #[serde(default)]
    pub grace_ms: u64,
    pub warned: BTreeSet<u64>,
    pub expired: bool,
    /// How long this site has been observed open but not in front.
    #[serde(default)]
    pub bg_open_ms: u64,
    pub bg_noticed: bool,
}

pub struct Tracker {
    pub config: Config,
    pub day: NaiveDate,
    pub state: BTreeMap<TargetKey, TargetState>,
    last_tick: Option<DateTime<Local>>,
    last_foreground: Option<TargetKey>,
}

impl Tracker {
    pub fn new(config: Config, now: DateTime<Local>) -> Self {
        Self {
            config,
            day: now.date_naive(),
            state: BTreeMap::new(),
            last_tick: None,
            last_foreground: None,
        }
    }

    /// Time used today, in whole seconds.
    pub fn used(&self, key: &TargetKey) -> u64 {
        self.state.get(key).map(|s| s.used_ms / 1000).unwrap_or(0)
    }

    /// Grant more time on a target the cat has already closed or warned about.
    pub fn snooze(&mut self, key: &TargetKey, secs: u64) {
        let st = self.state.entry(key.clone()).or_default();
        st.grace_ms += secs * 1000;
        st.expired = false;
        st.warned.clear();
    }

    /// Effective limit for a key, in seconds, plus any snooze grace.
    fn limits_for(&self, key: &TargetKey) -> Option<(u64, Option<u64>, u64)> {
        match key {
            TargetKey::Site(d) => self
                .config
                .sites
                .iter()
                .find(|r| crate::domain::canonical(&r.domain) == *d && r.enabled)
                .map(|r| (r.daily_limit_secs, r.session_limit_secs, r.warn_lead_secs)),
            TargetKey::App(a) => self
                .config
                .apps
                .iter()
                .find(|r| r.app_id.eq_ignore_ascii_case(a) && r.enabled)
                .map(|r| (r.daily_limit_secs, r.session_limit_secs, r.warn_lead_secs)),
        }
    }

    /// Seconds left before this target hits a limit. `None` = untracked.
    ///
    /// Derived from the SAME floored `used` the interface shows, so "1m 40s of
    /// 5m" and "3m 20s left" always sum to the limit. Flooring the two numbers
    /// independently loses up to a second between them, which reads as the app
    /// being unable to subtract. Expiry still uses the exact millisecond value.
    pub fn remaining(&self, key: &TargetKey) -> Option<u64> {
        let (daily, session_limit, _) = self.limits_for(key)?;
        let st = self.state.get(key).cloned().unwrap_or_default();
        let grace = st.grace_ms / 1000;
        let by_daily = (daily + grace).saturating_sub(st.used_ms / 1000);
        let by_session =
            session_limit.map(|s| (s + grace).saturating_sub(st.session_ms / 1000));
        Some(match by_session {
            Some(s) => by_daily.min(s),
            None => by_daily,
        })
    }

    /// Milliseconds left. The expiry check uses this so a limit trips at the
    /// exact moment it is reached rather than a second's rounding either side.
    fn remaining_ms(&self, key: &TargetKey) -> Option<u64> {
        let (daily, session_limit, _) = self.limits_for(key)?;
        let st = self.state.get(key).cloned().unwrap_or_default();
        let by_daily = (daily * 1000 + st.grace_ms).saturating_sub(st.used_ms);
        let by_session =
            session_limit.map(|s| (s * 1000 + st.grace_ms).saturating_sub(st.session_ms));
        Some(match by_session {
            Some(s) => by_daily.min(s),
            None => by_daily,
        })
    }

    fn resolve(&self, activity: &Activity) -> Option<TargetKey> {
        if let Some(tab) = &activity.tab {
            if let Some(rule) = self.config.site_for_url(&tab.url) {
                return Some(TargetKey::site(&rule.domain));
            }
        }
        self.config
            .app_for_id(&activity.app_id)
            .map(|r| TargetKey::app(&r.app_id))
    }

    fn roll_day(&mut self, now: DateTime<Local>) {
        let today = now.date_naive();
        if today != self.day {
            self.day = today;
            self.state.clear();
            self.last_foreground = None;
        }
    }

    /// Advance the clock.
    ///
    /// * `activity` — what is in front, or `None` when idle/locked.
    /// * `open_tabs` — every open tab; only consulted for background modes.
    ///   Pass an empty slice when you have not scanned.
    pub fn tick(
        &mut self,
        now: DateTime<Local>,
        activity: Option<&Activity>,
        open_tabs: &[TabRef],
    ) -> Vec<Action> {
        self.roll_day(now);

        let elapsed = match self.last_tick {
            Some(prev) => match (now - prev).num_milliseconds() {
                gap if (0..=MAX_TICK_GAP_MS).contains(&gap) => gap as u64,
                _ => 0, // discontinuity: charge nothing for unobserved time
            },
            None => 0,
        };
        self.last_tick = Some(now);

        let foreground = activity.and_then(|a| self.resolve(a));

        // A change of foreground ends the previous session.
        if self.last_foreground != foreground {
            if let Some(prev) = &self.last_foreground {
                if let Some(st) = self.state.get_mut(prev) {
                    st.session_ms = 0;
                }
            }
            self.last_foreground = foreground.clone();
        }

        let mut actions = Vec::new();
        let mut charged: BTreeSet<TargetKey> = BTreeSet::new();

        // --- a limit that is no longer spent is no longer spent --------------
        //
        // `expired` latches on purpose: once the budget is gone the clock
        // stops, because a number climbing forever past the limit says nothing
        // that "over" did not already say. But raising a limit gives time
        // back, and nothing cleared the flag -- so the interface read "1m
        // left" while the clock stayed stopped, which looks exactly like a
        // broken timer.
        //
        // Stated as an invariant rather than patched at each place a limit can
        // change: if there is time remaining, the target is not expired. That
        // covers editing a limit, snoozing, the day rolling over, and whatever
        // changes a limit next.
        let revived: Vec<TargetKey> = self
            .state
            .iter()
            .filter(|(_, st)| st.expired)
            .map(|(k, _)| k.clone())
            .filter(|k| self.remaining_ms(k).is_some_and(|ms| ms > 0))
            .collect();
        for key in revived {
            if let Some(st) = self.state.get_mut(&key) {
                st.expired = false;
                st.warned.clear();
            }
        }

        // --- foreground time -------------------------------------------------
        if let Some(key) = &foreground {
            let st = self.state.entry(key.clone()).or_default();
            // Stop the clock once the budget is spent. Carrying on past the
            // limit just produces a number that climbs forever and says
            // nothing: the answer is already "over". Snoozing clears `expired`
            // and the clock starts again, as does a new day.
            if !st.expired {
                st.used_ms += elapsed;
                st.session_ms += elapsed;
            }
            st.bg_open_ms = 0;
            st.bg_noticed = false;
            charged.insert(key.clone());
        }

        // --- background tabs -------------------------------------------------
        // Collected first to avoid holding a borrow of `self.config` across the
        // mutable `self.state` access below.
        let bg: Vec<(TargetKey, BackgroundMode, u64, TabRef)> = open_tabs
            .iter()
            .filter_map(|tab| {
                let rule = self.config.site_for_url(&tab.url)?;
                let key = TargetKey::site(&rule.domain);
                if charged.contains(&key) {
                    return None; // it is the foreground tab; already counted
                }
                Some((key, rule.background_mode, rule.notice_after_secs, tab.clone()))
            })
            .collect();

        let mut expire_targets: BTreeMap<TargetKey, TabRef> = BTreeMap::new();

        for (key, mode, notice_after, tab) in bg {
            if charged.contains(&key) {
                continue; // another window of the same site already counted
            }
            let st = self.state.entry(key.clone()).or_default();
            st.bg_open_ms += elapsed;
            match mode {
                BackgroundMode::OpenAnywhere => {
                    if !st.expired {
                        st.used_ms += elapsed;
                    }
                    charged.insert(key.clone());
                    expire_targets.insert(key.clone(), tab);
                }
                BackgroundMode::ForegroundNotice => {
                    if st.bg_open_ms >= notice_after * 1000 && !st.bg_noticed {
                        st.bg_noticed = true;
                        actions.push(Action::Notice {
                            key: key.clone(),
                            open_secs: st.bg_open_ms / 1000,
                        });
                    }
                }
                BackgroundMode::ForegroundOnly => {}
            }
        }

        // --- warnings and expiry --------------------------------------------
        let keys: Vec<TargetKey> = charged.into_iter().collect();
        for key in keys {
            let Some((_, _, warn_lead)) = self.limits_for(&key) else {
                continue;
            };
            let Some(remaining_ms) = self.remaining_ms(&key) else {
                continue;
            };
            let remaining = remaining_ms / 1000;

            if remaining_ms == 0 {
                let already = self.state.get(&key).map(|s| s.expired).unwrap_or(false);
                if !already {
                    if let Some(st) = self.state.get_mut(&key) {
                        st.expired = true;
                    }
                    let tab = if Some(&key) == foreground.as_ref() {
                        activity.and_then(|a| a.tab.clone())
                    } else {
                        expire_targets.get(&key).cloned()
                    };
                    actions.push(Action::Expire { key, tab });
                }
                continue;
            }

            // A mark at or above the whole budget would fire on the first tick,
            // warning the user before they have used anything. Skip those, so a
            // short limit simply warns less rather than warning nonsensically.
            let budget = self
                .limits_for(&key)
                .map(|(daily, session, _)| match session {
                    Some(s) => daily.min(s),
                    None => daily,
                })
                .unwrap_or(0);

            for mark in [warn_lead, FINAL_WARN_SECS] {
                if mark >= budget {
                    continue;
                }
                if mark > 0 && remaining <= mark {
                    let fired = self
                        .state
                        .get(&key)
                        .map(|s| s.warned.contains(&mark))
                        .unwrap_or(false);
                    if !fired {
                        if let Some(st) = self.state.get_mut(&key) {
                            st.warned.insert(mark);
                        }
                        actions.push(Action::Warn {
                            key: key.clone(),
                            remaining_secs: remaining,
                        });
                    }
                }
            }
        }

        actions
    }
}
