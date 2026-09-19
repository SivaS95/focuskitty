//! Behavioural tests for the tick loop.
//!
//! These cover the cases Siva raised — a background YouTube tab, a minimized
//! window, and closing the right tab out of many — without needing a browser.

use chrono::{Duration, Local, TimeZone};
use fk_core::activity::{Activity, TabRef};
use fk_core::rules::{AppRule, BackgroundMode, Config, SiteRule, TargetKey};
use fk_core::tracker::{Action, Tracker};

fn t0() -> chrono::DateTime<Local> {
    Local.with_ymd_and_hms(2026, 9, 16, 9, 0, 0).unwrap()
}

fn site(domain: &str, limit: u64, mode: BackgroundMode) -> SiteRule {
    SiteRule {
        domain: domain.into(),
        daily_limit_secs: limit,
        session_limit_secs: None,
        background_mode: mode,
        warn_lead_secs: 300,
        notice_after_secs: 1800,
        enabled: true,
    }
}

fn tab(id: &str, url: &str) -> TabRef {
    TabRef { id: id.into(), url: url.into(), title: "t".into() }
}

fn browsing(url: &str, id: &str) -> Activity {
    Activity {
        app_id: "com.google.Chrome".into(),
        app_name: "Google Chrome".into(),
        tab: Some(tab(id, url)),
    }
}

fn cfg(sites: Vec<SiteRule>) -> Config {
    Config { sites, ..Config::starter() }
}

/// Drive `secs` seconds of one-second ticks, returning everything that fired.
fn run(tr: &mut Tracker, start: chrono::DateTime<Local>, secs: i64,
       act: Option<&Activity>, open: &[TabRef]) -> Vec<Action> {
    let mut out = Vec::new();
    for i in 0..=secs {
        out.extend(tr.tick(start + Duration::seconds(i), act, open));
    }
    out
}

#[test]
fn counts_only_the_foreground_site() {
    let mut tr = Tracker::new(cfg(vec![site("youtube.com", 600, BackgroundMode::ForegroundOnly)]), t0());
    let yt = browsing("https://www.youtube.com/watch?v=a", "1");
    run(&mut tr, t0(), 10, Some(&yt), &[]);
    assert_eq!(tr.used(&TargetKey::site("youtube.com")), 10);

    // Switching to an untracked site must stop the clock.
    let other = browsing("https://docs.rs/", "2");
    run(&mut tr, t0() + Duration::seconds(10), 30, Some(&other), &[]);
    assert_eq!(tr.used(&TargetKey::site("youtube.com")), 10);
}

/// Siva's minimized-window case: nothing in front means nothing accrues.
#[test]
fn minimized_or_idle_pauses_the_clock() {
    let mut tr = Tracker::new(cfg(vec![site("youtube.com", 600, BackgroundMode::ForegroundOnly)]), t0());
    let yt = browsing("https://youtube.com/", "1");
    run(&mut tr, t0(), 5, Some(&yt), &[]);
    let before = tr.used(&TargetKey::site("youtube.com"));

    // Chrome minimized -> probe reports None for 60s.
    run(&mut tr, t0() + Duration::seconds(5), 60, None, &[]);
    assert_eq!(tr.used(&TargetKey::site("youtube.com")), before,
               "time must not accrue while minimized");
}

/// Siva's YouTube-in-a-background-tab case, across all three modes.
#[test]
fn background_modes_behave_differently() {
    let yt_tab = tab("9", "https://www.youtube.com/watch?v=music");
    let working = browsing("https://docs.rs/", "2");

    // 1. ForegroundOnly — the loophole stays open, by design.
    let mut tr = Tracker::new(cfg(vec![site("youtube.com", 600, BackgroundMode::ForegroundOnly)]), t0());
    run(&mut tr, t0(), 120, Some(&working), &[yt_tab.clone()]);
    assert_eq!(tr.used(&TargetKey::site("youtube.com")), 0);

    // 2. OpenAnywhere — the clock runs even though it is behind you.
    let mut tr = Tracker::new(cfg(vec![site("youtube.com", 600, BackgroundMode::OpenAnywhere)]), t0());
    run(&mut tr, t0(), 120, Some(&working), &[yt_tab.clone()]);
    assert_eq!(tr.used(&TargetKey::site("youtube.com")), 120);

    // 3. ForegroundNotice — no time charged, but the cat says something.
    let mut rule = site("youtube.com", 600, BackgroundMode::ForegroundNotice);
    rule.notice_after_secs = 60;
    let mut tr = Tracker::new(cfg(vec![rule]), t0());
    let acts = run(&mut tr, t0(), 120, Some(&working), &[yt_tab]);
    assert_eq!(tr.used(&TargetKey::site("youtube.com")), 0);
    let notices: Vec<_> = acts.iter()
        .filter(|a| matches!(a, Action::Notice { .. })).collect();
    assert_eq!(notices.len(), 1, "should notice exactly once, not every tick");
}

#[test]
fn warns_at_lead_then_at_one_minute_then_expires() {
    let mut rule = site("instagram.com", 400, BackgroundMode::ForegroundOnly);
    rule.warn_lead_secs = 300;
    let mut tr = Tracker::new(cfg(vec![rule]), t0());
    let ig = browsing("https://instagram.com/", "5");

    let acts = run(&mut tr, t0(), 400, Some(&ig), &[]);
    let warns: Vec<u64> = acts.iter().filter_map(|a| match a {
        Action::Warn { remaining_secs, .. } => Some(*remaining_secs), _ => None,
    }).collect();
    assert_eq!(warns.len(), 2, "one warning at the 5m lead, one at 1m, got {warns:?}");
    assert!(warns[0] <= 300 && warns[1] <= 60);

    let expiries: Vec<_> = acts.iter().filter(|a| matches!(a, Action::Expire { .. })).collect();
    assert_eq!(expiries.len(), 1, "must expire exactly once, not every tick after");

    // And the expiry carries the tab to close.
    match expiries[0] {
        Action::Expire { tab: Some(t), .. } => assert_eq!(t.id, "5"),
        other => panic!("expected a closable tab, got {other:?}"),
    }
}

/// The race Siva asked about: switching tabs must not hand us the wrong tab.
#[test]
fn expiry_targets_the_tab_that_was_actually_in_front() {
    let mut tr = Tracker::new(cfg(vec![site("reddit.com", 5, BackgroundMode::ForegroundOnly)]), t0());
    let reddit = browsing("https://reddit.com/r/rust", "reddit-tab");

    let acts = run(&mut tr, t0(), 5, Some(&reddit), &[]);
    let expire = acts.iter().find(|a| matches!(a, Action::Expire { .. })).expect("should expire");
    match expire {
        Action::Expire { tab: Some(t), .. } => {
            assert_eq!(t.id, "reddit-tab");
            assert!(t.url.contains("reddit.com"));
        }
        other => panic!("expected reddit tab, got {other:?}"),
    }
}

/// Raising a limit on something already over must restart its clock.
///
/// Reported from Windows: a site that had run out showed "1m left" once the
/// limit was raised, and then sat there. The number came from the new limit;
/// the clock was still stopped by the old one.
#[test]
fn raising_a_limit_starts_the_clock_again() {
    let c = cfg(vec![site("youtube.com", 120, BackgroundMode::ForegroundOnly)]);
    let mut tr = Tracker::new(c, t0());
    let key = TargetKey::site("youtube.com");
    let act = browsing("https://youtube.com/watch", "1");

    for i in 0..130 {
        tr.tick(t0() + Duration::seconds(i), Some(&act), &[]);
    }
    assert!(tr.state[&key].expired, "the budget should be spent");
    let spent = tr.used(&key);

    // The user gives it five minutes instead of two.
    tr.config.sites[0].daily_limit_secs = 300;
    for i in 130..160 {
        tr.tick(t0() + Duration::seconds(i), Some(&act), &[]);
    }

    assert!(!tr.state[&key].expired, "raising the limit must un-expire it");
    assert!(
        tr.used(&key) > spent,
        "the clock must move again: was {spent}s, still {}s",
        tr.used(&key)
    );
}

#[test]
fn snooze_grants_more_time_and_rearms_warnings() {
    let mut tr = Tracker::new(cfg(vec![site("x.com", 10, BackgroundMode::ForegroundOnly)]), t0());
    let x = browsing("https://x.com/home", "7");
    let acts = run(&mut tr, t0(), 10, Some(&x), &[]);
    assert_eq!(acts.iter().filter(|a| matches!(a, Action::Expire { .. })).count(), 1);

    tr.snooze(&TargetKey::site("x.com"), 10);
    assert_eq!(tr.remaining(&TargetKey::site("x.com")), Some(10));

    let more = run(&mut tr, t0() + Duration::seconds(11), 10, Some(&x), &[]);
    assert_eq!(more.iter().filter(|a| matches!(a, Action::Expire { .. })).count(), 1,
               "should expire again once the snooze is used up");
}

#[test]
fn app_rules_track_native_apps() {
    let mut cfg = Config::starter();
    cfg.apps.push(AppRule {
        app_id: "com.tinyspeck.slackmacgap".into(),
        app_name: "Slack".into(),
        daily_limit_secs: 100,
        session_limit_secs: None,
        warn_lead_secs: 30,
        enabled: true,
    });
    let mut tr = Tracker::new(cfg, t0());
    let slack = Activity {
        app_id: "com.tinyspeck.slackmacgap".into(),
        app_name: "Slack".into(),
        tab: None,
    };
    let acts = run(&mut tr, t0(), 100, Some(&slack), &[]);
    assert_eq!(tr.used(&TargetKey::app("com.tinyspeck.slackmacgap")), 100);
    match acts.iter().find(|a| matches!(a, Action::Expire { .. })) {
        Some(Action::Expire { tab, .. }) => assert!(tab.is_none(), "native apps have no tab to close"),
        other => panic!("expected an expiry, got {other:?}"),
    }
}

/// A closed lid must not land as usage at all.
#[test]
fn unobserved_time_is_never_charged() {
    let mut tr = Tracker::new(cfg(vec![site("youtube.com", 100_000, BackgroundMode::ForegroundOnly)]), t0());
    let yt = browsing("https://youtube.com/", "1");
    tr.tick(t0(), Some(&yt), &[]);
    // Machine sleeps for eight hours, then one more tick.
    tr.tick(t0() + Duration::hours(8), Some(&yt), &[]);
    assert_eq!(tr.used(&TargetKey::site("youtube.com")), 0,
               "a gap we did not observe must charge nothing, not even the clamp");

    // A clock that jumps backwards (NTP correction) must not underflow either.
    tr.tick(t0() - Duration::hours(1), Some(&yt), &[]);
    assert_eq!(tr.used(&TargetKey::site("youtube.com")), 0);
}

#[test]
fn totals_reset_at_midnight() {
    let mut tr = Tracker::new(cfg(vec![site("youtube.com", 600, BackgroundMode::ForegroundOnly)]), t0());
    let yt = browsing("https://youtube.com/", "1");
    run(&mut tr, t0(), 10, Some(&yt), &[]);
    assert_eq!(tr.used(&TargetKey::site("youtube.com")), 10);

    let tomorrow = Local.with_ymd_and_hms(2026, 9, 17, 9, 0, 0).unwrap();
    tr.tick(tomorrow, Some(&yt), &[]);
    assert_eq!(tr.used(&TargetKey::site("youtube.com")), 0, "a new day starts clean");
}

#[test]
fn subdomains_share_one_budget() {
    let mut tr = Tracker::new(cfg(vec![site("youtube.com", 600, BackgroundMode::ForegroundOnly)]), t0());
    run(&mut tr, t0(), 5, Some(&browsing("https://www.youtube.com/", "1")), &[]);
    // Contiguous: the second stretch resumes on the same second the first ended.
    run(&mut tr, t0() + Duration::seconds(5), 5, Some(&browsing("https://m.youtube.com/", "2")), &[]);

    assert_eq!(tr.used(&TargetKey::site("youtube.com")), 10,
               "www and m must draw from the same budget");
    assert_eq!(tr.state.len(), 1, "they must not become two separate budgets");
}

/// A limit shorter than a warning mark must not fire that warning instantly.
#[test]
fn warnings_never_fire_before_any_time_is_used() {
    // 30s budget, with a 5m lead and the 1m final mark both larger than it.
    let mut rule = site("tiktok.com", 30, BackgroundMode::ForegroundOnly);
    rule.warn_lead_secs = 300;
    let mut tr = Tracker::new(cfg(vec![rule]), t0());
    let tk = browsing("https://tiktok.com/", "1");

    let acts = run(&mut tr, t0(), 30, Some(&tk), &[]);
    assert!(
        !matches!(acts.first(), Some(Action::Warn { .. })),
        "must not warn on the very first tick, got {:?}", acts.first()
    );
    assert_eq!(acts.iter().filter(|a| matches!(a, Action::Warn { .. })).count(), 0,
               "no mark is smaller than the budget, so there is nothing to warn about");
    assert_eq!(acts.iter().filter(|a| matches!(a, Action::Expire { .. })).count(), 1);
}

/// The ordinary case still warns twice.
#[test]
fn a_realistic_limit_warns_at_lead_then_at_one_minute() {
    let mut rule = site("news.ycombinator.com", 1800, BackgroundMode::ForegroundOnly);
    rule.warn_lead_secs = 300;
    let mut tr = Tracker::new(cfg(vec![rule]), t0());
    let hn = browsing("https://news.ycombinator.com/", "1");

    let acts = run(&mut tr, t0(), 1800, Some(&hn), &[]);
    let warns: Vec<u64> = acts.iter().filter_map(|a| match a {
        Action::Warn { remaining_secs, .. } => Some(*remaining_secs), _ => None,
    }).collect();
    assert_eq!(warns, vec![300, 60], "30m limit should warn at 5m and 1m");
}

/// Past the limit the number should stop, not climb forever.
#[test]
fn the_clock_stops_once_the_limit_is_spent() {
    let mut tr = Tracker::new(cfg(vec![site("x.com", 10, BackgroundMode::ForegroundOnly)]), t0());
    let x = browsing("https://x.com/", "1");

    run(&mut tr, t0(), 10, Some(&x), &[]);
    let at_limit = tr.used(&TargetKey::site("x.com"));
    assert_eq!(at_limit, 10);

    // Keep sitting on it for another minute.
    run(&mut tr, t0() + Duration::seconds(10), 60, Some(&x), &[]);
    assert_eq!(tr.used(&TargetKey::site("x.com")), at_limit,
               "time past the limit must not keep accruing");

    // A snooze buys more time, and the clock runs again.
    tr.snooze(&TargetKey::site("x.com"), 10);
    run(&mut tr, t0() + Duration::seconds(71), 5, Some(&x), &[]);
    assert!(tr.used(&TargetKey::site("x.com")) > at_limit,
            "snoozing should start the clock again");
}
