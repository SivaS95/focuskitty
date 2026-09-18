//! What the user chose to watch, and for how long.

use serde::{Deserialize, Serialize};

use crate::domain;

/// How a site rule treats the site being open but not in front.
///
/// Background audio is *not* detectable: neither Chrome's nor Safari's
/// AppleScript dictionary exposes any audible/muted property, so "is this tab
/// making noise" cannot be answered without shipping a browser extension.
/// What we can see is whether the site is open somewhere, hence these modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundMode {
    /// Original FocusCat behaviour: time accrues only while the tab is in front.
    ForegroundOnly,
    /// Time accrues whenever the site is open in any tab, hidden or not.
    OpenAnywhere,
    /// Time accrues in the foreground only, but the cat comments on a tab
    /// left open behind you.
    #[default]
    ForegroundNotice,
}

fn default_warn_lead() -> u64 {
    300
}
fn default_notice_after() -> u64 {
    1800
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteRule {
    /// Bare domain, e.g. "youtube.com". Subdomains are included automatically.
    pub domain: String,
    pub daily_limit_secs: u64,
    #[serde(default)]
    pub session_limit_secs: Option<u64>,
    #[serde(default)]
    pub background_mode: BackgroundMode,
    /// How long before the limit the cat warns. Siva's spec: 5–10 minutes.
    #[serde(default = "default_warn_lead")]
    pub warn_lead_secs: u64,
    /// For `ForegroundNotice`: comment once a hidden tab has sat this long.
    #[serde(default = "default_notice_after")]
    pub notice_after_secs: u64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppRule {
    /// Bundle id on macOS, executable name on Windows.
    pub app_id: String,
    pub app_name: String,
    pub daily_limit_secs: u64,
    #[serde(default)]
    pub session_limit_secs: Option<u64>,
    #[serde(default = "default_warn_lead")]
    pub warn_lead_secs: u64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub sites: Vec<SiteRule>,
    pub apps: Vec<AppRule>,
    /// Where the user dragged the cat. Restored on launch, clamped to a screen.
    pub cat_position: Option<(f64, f64)>,
    /// Never track or close private browsing.
    pub skip_incognito: bool,
    /// Close the tab when a site's limit is reached.
    pub enforce: bool,
}

impl Config {
    /// A sane starting config, used on first run.
    pub fn starter() -> Self {
        Self {
            sites: vec![],
            apps: vec![],
            cat_position: None,
            skip_incognito: true,
            enforce: true,
        }
    }

    /// The site rule governing this URL, if any.
    pub fn site_for_url(&self, url: &str) -> Option<&SiteRule> {
        self.sites
            .iter()
            .filter(|r| r.enabled)
            .find(|r| domain::matches_domain(url, &r.domain))
    }

    /// The app rule governing this app id, if any.
    pub fn app_for_id(&self, app_id: &str) -> Option<&AppRule> {
        self.apps
            .iter()
            .filter(|r| r.enabled)
            .find(|r| r.app_id.eq_ignore_ascii_case(app_id))
    }
}

/// What a tracked total is counted against.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TargetKey {
    Site(String),
    App(String),
}

impl TargetKey {
    pub fn site(domain_str: &str) -> Self {
        TargetKey::Site(domain::canonical(domain_str))
    }
    pub fn app(app_id: &str) -> Self {
        TargetKey::App(app_id.to_ascii_lowercase())
    }
    pub fn label(&self) -> &str {
        match self {
            TargetKey::Site(d) => d,
            TargetKey::App(a) => a,
        }
    }
}
