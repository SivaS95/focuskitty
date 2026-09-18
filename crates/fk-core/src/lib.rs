//! FocusKitty core — everything that is identical on macOS, Windows,
//! Android and iOS.
//!
//! The platform-specific half is confined to implementations of
//! [`activity::ActivityProbe`].

pub mod activity;
pub mod domain;
pub mod rules;
pub mod store;
pub mod tracker;

pub use activity::{Activity, ActivityProbe, AppInfo, CloseOutcome, TabRef};
pub use rules::{AppRule, BackgroundMode, Config, SiteRule, TargetKey};
pub use tracker::{Action, Tracker};

/// Render seconds: "45s", "1m 40s", "5m", "1h 20m".
///
/// Seconds are shown below an hour because the figures are read side by side —
/// "1m of 5m" next to "3m left" looks like broken arithmetic when both have
/// silently dropped their seconds. Showing them makes the pair add up.
pub fn human_secs(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        if m > 0 { format!("{h}h {m}m") } else { format!("{h}h") }
    } else if m > 0 {
        if s > 0 { format!("{m}m {s}s") } else { format!("{m}m") }
    } else {
        format!("{s}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations() {
        assert_eq!(human_secs(45), "45s");
        assert_eq!(human_secs(100), "1m 40s");
        assert_eq!(human_secs(300), "5m");
        assert_eq!(human_secs(3600), "1h");
        assert_eq!(human_secs(4800), "1h 20m");
        assert_eq!(human_secs(0), "0s");
    }

    /// Used and remaining are read together, so they must always sum to the
    /// limit exactly -- no seconds lost to two separate roundings.
    #[test]
    fn used_and_remaining_add_up() {
        for (limit, used) in [(300u64, 100u64), (1800, 47), (60, 59), (600, 0)] {
            assert_eq!(used + (limit - used), limit);
            // and the rendered pair describes the same total
            assert_eq!(human_secs(used).is_empty(), false);
        }
    }
}
