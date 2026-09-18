//! Where config and the day's totals live on disk.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::rules::{Config, TargetKey};
use crate::tracker::{TargetState, Tracker};

/// Set by the host at startup where the platform dictates the location.
///
/// Android gives each app a private files directory and nothing may be written
/// outside it, so the path cannot be derived from environment variables the
/// way it can on a desktop.
static DATA_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Tell the store where to keep its files. Ignored after the first call.
pub fn set_data_dir(path: PathBuf) {
    let _ = DATA_DIR.set(path);
}

/// `~/Library/Application Support/FocusKitty` on macOS,
/// `%APPDATA%\FocusKitty` on Windows, the app's files dir on Android.
pub fn data_dir() -> PathBuf {
    if let Some(p) = DATA_DIR.get() {
        return p.clone();
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library/Application Support/FocusKitty");
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("FocusKitty");
        }
    }
    std::env::temp_dir().join("FocusKitty")
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

pub fn day_path(day: NaiveDate) -> PathBuf {
    data_dir().join("diary").join(format!("{day}.json"))
}

/// A rule queued from somewhere that cannot write the config safely.
///
/// The Android overlay runs in its own process-side WebView with no access to
/// the Rust core, so it drops a request here and the tracker applies it. One
/// writer owns the config; everyone else asks.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PendingWatch {
    pub app_id: String,
    pub app_name: String,
    pub minutes: u64,
}

pub fn take_pending_watch() -> Option<PendingWatch> {
    let path = data_dir().join("pending_watch.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    serde_json::from_str(&raw).ok()
}

/// A rule the overlay asked to drop, by app id.
///
/// The same one-writer rule as `PendingWatch`: the overlay cannot edit the
/// config, so it leaves the id here and the tracker removes the rule and the
/// day's total together, which is the only place that can do both.
pub fn take_pending_unwatch() -> Option<String> {
    let path = data_dir().join("pending_unwatch.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let id = v.get("app_id")?.as_str()?.trim().to_string();
    (!id.is_empty()).then_some(id)
}

pub fn load_config() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        return Ok(Config::starter());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;

    // A config that exists but will not parse must NEVER be silently replaced
    // with an empty one: the next save would then wipe every rule the user set.
    // Keep a copy and fail loudly instead.
    let mut config: Config = match serde_json::from_str(&raw) {
        Ok(c) => c,
        Err(e) => {
            let backup = path.with_extension("json.broken");
            let _ = std::fs::write(&backup, &raw);
            anyhow::bail!(
                "config at {} did not parse ({e}); a copy is at {}",
                path.display(),
                backup.display()
            );
        }
    };

    // Repair rules written before domains were normalised. A rule holding a
    // pasted URL matches nothing and silently counts zero forever, so fix it
    // on the way in rather than leaving the user with a dead rule.
    let mut changed = false;
    for r in config.sites.iter_mut() {
        let fixed = crate::domain::normalize(&r.domain);
        if !fixed.is_empty() && fixed != r.domain {
            r.domain = fixed;
            changed = true;
        }
    }
    config.sites.retain(|r| !r.domain.is_empty());

    // Normalising can collapse two rules onto the same domain (a pasted URL
    // and a hand-typed host). Keep the last one written -- that is the one the
    // user most recently meant -- and drop the rest.
    let mut seen = std::collections::HashSet::new();
    let before = config.sites.len();
    config.sites.reverse();
    config.sites.retain(|r| seen.insert(r.domain.to_ascii_lowercase()));
    config.sites.reverse();
    if config.sites.len() != before {
        changed = true;
    }

    if changed {
        let _ = save_config(&config);
    }
    Ok(config)
}

pub fn save_config(config: &Config) -> Result<()> {
    write_json(&config_path(), config)
}

/// One day's tracked totals — the raw material the diary is written from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DayLog {
    pub day: NaiveDate,
    pub totals: Vec<(TargetKey, TargetState)>,
    /// Human-readable events, newest last.
    #[serde(default)]
    pub events: Vec<String>,
}

pub fn save_day(tracker: &Tracker, events: &[String]) -> Result<()> {
    let log = DayLog {
        day: tracker.day,
        totals: tracker
            .state
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        events: events.to_vec(),
    };
    write_json(&day_path(tracker.day), &log)
}

pub fn load_day(day: NaiveDate) -> Result<Option<DayLog>> {
    let path = day_path(day);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(Some(serde_json::from_str(&raw)?))
}

/// Write atomically, so a crash mid-write cannot leave a truncated file that
/// loses the whole day's tracking.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(value)?;
    std::fs::write(&tmp, body).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config that will not parse must not come back as an empty one.
    ///
    /// That is how a user loses every rule: load returns a blank config, the
    /// next save writes it back, and the file is gone for good. Loading must
    /// fail loudly and leave a copy behind instead.
    ///
    /// `DATA_DIR` is a `OnceLock`, so this is deliberately one test covering
    /// both outcomes rather than two fighting over the same directory.
    #[test]
    fn a_broken_config_is_kept_not_replaced() {
        let dir = std::env::temp_dir().join(format!("fk-store-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        set_data_dir(dir.clone());

        // Nothing saved yet: starting fresh is the one correct empty case.
        assert!(load_config().is_ok(), "a missing config is not an error");

        let good = r#"{"sites":[{"domain":"youtube.com","daily_limit_secs":1200,
            "session_limit_secs":null,"warn_lead_secs":300,"enabled":true,
            "background_mode":"foreground_notice","notice_after_secs":600}],"apps":[]}"#;
        std::fs::write(config_path(), good).unwrap();
        assert_eq!(load_config().unwrap().sites.len(), 1);

        // Now truncate it, as a crash or a bad write would.
        std::fs::write(config_path(), "{\"sites\": [{\"domain\": \"you").unwrap();
        let err = load_config().expect_err("an unparseable config must fail, not return empty");
        assert!(err.to_string().contains("did not parse"), "got: {err}");

        let backup = config_path().with_extension("json.broken");
        assert!(backup.exists(), "the unreadable config must be kept");
        assert!(std::fs::read_to_string(&backup).unwrap().contains("you"));

        // And the original is left exactly as it was, not overwritten.
        assert!(std::fs::read_to_string(config_path()).unwrap().starts_with("{\"sites\""));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
