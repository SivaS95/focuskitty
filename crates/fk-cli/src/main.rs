//! FocusKitty's headless half — the tracker with no cat attached.
//!
//! This exists so the risky layer can be proved before a single pixel is drawn,
//! and so a destructive action (closing your tabs) is never armed until its
//! every path has been watched in `--dry-run`.

use std::time::Duration;

use anyhow::{Context, Result};
use fk_core::activity::{ActivityProbe, CloseOutcome};
use fk_core::rules::{BackgroundMode, SiteRule};
use fk_core::tracker::{Action, Tracker};
use fk_core::{human_secs, store};

const USAGE: &str = "\
focuskitty — headless tracker (phase 1)

USAGE:
    focuskitty probe [n]          watch what the tracker sees, once a second
    focuskitty uia [depth] [exe]  (Windows) dump a window's a11y tree
    focuskitty tabs               list every open tab in every window
    focuskitty apps               list apps you could set a limit on
    focuskitty watch [--live] [--for N]   run the timers; --for N exits after N seconds
    focuskitty add <domain> <minutes> [--mode <m>]
    focuskitty rules              show the current config
    focuskitty bench              measure how long reading the front tab costs

MODES (for background tabs — audio is not detectable, so this is the choice):
    foreground-only     time counts only while the tab is in front
    open-anywhere       time counts whenever the site is open in any tab
    foreground-notice   counts in front only, but the cat comments (default)
";

fn probe() -> impl ActivityProbe {
    #[cfg(target_os = "macos")]
    {
        fk_probe_macos::MacProbe::new()
    }
    #[cfg(target_os = "windows")]
    {
        fk_probe_windows::WinProbe::new()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        compile_error!("no probe for this platform")
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");

    match cmd {
        "probe" => cmd_probe(),
        // Windows only: print the accessibility tree of whatever is in front.
        // "No URL" has several causes that look identical from outside; this
        // is what tells them apart without another build cycle.
        #[cfg(target_os = "windows")]
        "uia" => {
            let depth = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(8);
            // An executable name skips the countdown: there is a specific
            // window to look at, so nothing needs bringing forward.
            let exe = args.get(2).cloned();
            if let Some(name) = exe.as_deref() {
                let tree = fk_probe_windows::dump_window(depth, Some(name))?;
                println!("{tree}");
                return Ok(());
            }
            // The dump is of whatever is IN FRONT -- which, run from a
            // terminal, is the terminal. Counting down first is the whole
            // difference between a dump of Chrome and a dump of PowerShell.
            for n in (1..=6).rev() {
                println!("switch to the window you want dumped... {n}");
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            let tree = fk_probe_windows::dump_front_window(depth)?;
            println!("{tree}");
            // Also written to a file, because a long tree scrolls out of the
            // terminal's buffer and the interesting part is usually the top.
            let path = std::env::current_dir()?.join("uia-dump.txt");
            std::fs::write(&path, &tree)?;
            println!("\n(also saved to {})", path.display());
            Ok(())
        }
        "tabs" => cmd_tabs(),
        "apps" => cmd_apps(),
        "watch" => cmd_watch(args.iter().any(|a| a == "--live")),
        "add" => cmd_add(&args[1..]),
        "rules" => cmd_rules(),
        "bench" => cmd_bench(),
        _ => {
            print!("{USAGE}");
            Ok(())
        }
    }
}

/// Print what the probe sees, every second. The first line of defence against
/// "it says I'm on YouTube but I'm not".
fn cmd_probe() -> Result<()> {
    // `probe <n>` stops after n reads. A run that ends by itself is the only
    // kind whose output survives: killed from outside, everything it had
    // written was lost and the log came back empty.
    let ticks: Option<u64> = std::env::args().nth(2).and_then(|s| s.parse().ok());
    let p = probe();
    match ticks {
        Some(n) => println!("watching the frontmost window for {n} reads\n"),
        None => println!("watching the frontmost window — ctrl-c to stop\n"),
    }
    let mut last = String::new();
    let mut seen = 0u64;

    loop {
        if let Some(n) = ticks {
            if seen >= n {
                return Ok(());
            }
            seen += 1;
        }
        let line = match p.current() {
            None => "· idle / locked".to_string(),
            Some(a) => match &a.tab {
                Some(t) => format!(
                    "{:<22} tab={:<12} {}\n{:>23}  {}",
                    a.app_name,
                    t.id,
                    t.url.chars().take(90).collect::<String>(),
                    "",
                    t.title.chars().take(70).collect::<String>()
                ),
                None => format!("{:<22} (native app) {}", a.app_name, a.app_id),
            },
        };

        // Only reprint on change, so the log reads as a history of what you
        // did -- except when counting down, where every read is evidence.
        if line != last || ticks.is_some() {
            println!("[{}] {line}", chrono::Local::now().format("%H:%M:%S"));
            last = line;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Verify the cost of a tick, because the whole design rests on it being cheap
/// enough to run once a second on the main thread without the cat stuttering.
fn cmd_bench() -> Result<()> {
    let p = probe();
    // Warm up: the first call compiles and makes the first Apple Event.
    let _ = p.current();

    let runs = 50;
    let start = std::time::Instant::now();
    for _ in 0..runs {
        let _ = p.current();
    }
    let per = start.elapsed() / runs;
    println!("current() over {runs} runs: {per:?} each");

    let start = std::time::Instant::now();
    let tabs = p.open_tabs();
    println!("open_tabs() over {} tabs: {:?}", tabs.len(), start.elapsed());
    Ok(())
}

fn cmd_tabs() -> Result<()> {
    let tabs = probe().open_tabs();
    if tabs.is_empty() {
        println!("no tabs readable — is a browser running, and was automation allowed?");
        return Ok(());
    }
    println!("{} open tab(s):\n", tabs.len());
    for t in tabs {
        println!("  {:<12} {}", t.id, t.url.chars().take(100).collect::<String>());
    }
    Ok(())
}

fn cmd_apps() -> Result<()> {
    let apps = probe().installed_apps();
    println!("{} app(s) you could limit:\n", apps.len());
    for a in apps {
        println!("  {:<38} {}", a.name, a.id);
    }
    Ok(())
}

fn cmd_rules() -> Result<()> {
    let cfg = store::load_config()?;
    println!("config: {}\n", store::config_path().display());
    if cfg.sites.is_empty() && cfg.apps.is_empty() {
        println!("nothing watched yet — try:  focuskitty add youtube.com 30");
        return Ok(());
    }
    for r in &cfg.sites {
        println!(
            "  site  {:<24} {:>8}/day   mode={:?}  warn at {}",
            r.domain,
            human_secs(r.daily_limit_secs),
            r.background_mode,
            human_secs(r.warn_lead_secs)
        );
    }
    for r in &cfg.apps {
        println!("  app   {:<24} {:>8}/day", r.app_name, human_secs(r.daily_limit_secs));
    }
    Ok(())
}

fn cmd_add(args: &[String]) -> Result<()> {
    let domain = args.first().context("usage: focuskitty add <domain> <minutes>")?;
    let minutes: u64 = args
        .get(1)
        .context("usage: focuskitty add <domain> <minutes>")?
        .parse()
        .context("minutes must be a number")?;

    let mode = match args.iter().position(|a| a == "--mode").and_then(|i| args.get(i + 1)) {
        Some(m) if m == "foreground-only" => BackgroundMode::ForegroundOnly,
        Some(m) if m == "open-anywhere" => BackgroundMode::OpenAnywhere,
        Some(m) if m == "foreground-notice" => BackgroundMode::ForegroundNotice,
        Some(m) => anyhow::bail!("unknown mode {m:?} — see `focuskitty` for the list"),
        None => BackgroundMode::ForegroundNotice,
    };

    let mut cfg = store::load_config()?;
    cfg.sites.retain(|r| !r.domain.eq_ignore_ascii_case(domain));
    cfg.sites.push(SiteRule {
        domain: domain.to_lowercase(),
        daily_limit_secs: minutes * 60,
        session_limit_secs: None,
        background_mode: mode,
        warn_lead_secs: 300,
        notice_after_secs: 1800,
        enabled: true,
    });
    store::save_config(&cfg)?;
    println!("watching {domain} for {minutes}m/day ({mode:?})");
    Ok(())
}

/// The real loop. Closes nothing unless `--live`.
fn cmd_watch(live: bool) -> Result<()> {
    // `--for N` runs for N seconds and then prints what it charged. Without
    // it the command runs forever and has to be killed, and a killed run
    // reports nothing -- which is no use for proving the clock moved.
    let run_for: Option<u64> = std::env::args()
        .position(|a| a == "--for")
        .and_then(|i| std::env::args().nth(i + 1))
        .and_then(|v| v.parse().ok());
    let started = std::time::Instant::now();

    let cfg = store::load_config()?;
    if cfg.sites.is_empty() && cfg.apps.is_empty() {
        println!("nothing watched yet — try:  focuskitty add youtube.com 30");
        return Ok(());
    }

    let p = probe();
    let mut tracker = Tracker::new(cfg, chrono::Local::now());
    let mut events: Vec<String> = Vec::new();

    if live {
        println!("LIVE — tabs will actually be closed.\n");
    } else {
        println!("DRY RUN — nothing will be closed. Pass --live to arm it.\n");
    }

    // Scanning every tab is far more expensive than reading the front one, so
    // the background sweep runs at a tenth of the tick rate.
    let mut ticks: u64 = 0;
    let mut open_tabs = Vec::new();

    loop {
        if let Some(secs) = run_for {
            if started.elapsed().as_secs() >= secs {
                println!("\n--- charged after {secs}s");
                let mut any = false;
                for (key, st) in &tracker.state {
                    println!("TOTAL {key:?} used_ms={}", st.used_ms);
                    any = true;
                }
                if !any {
                    println!("TOTAL (nothing was tracked at all)");
                }
                return Ok(());
            }
        }
        let now = chrono::Local::now();
        let current = p.current();

        let needs_background = tracker
            .config
            .sites
            .iter()
            .any(|r| r.enabled && r.background_mode != BackgroundMode::ForegroundOnly);
        if needs_background && ticks % 10 == 0 {
            open_tabs = p.open_tabs();
        }

        for action in tracker.tick(now, current.as_ref(), &open_tabs) {
            let stamp = now.format("%H:%M:%S");
            match action {
                Action::Warn { key, remaining_secs } => {
                    let msg = format!("{} — {} left", key.label(), human_secs(remaining_secs));
                    println!("[{stamp}] 💭 {msg}");
                    events.push(msg);
                }
                Action::Notice { key, open_secs } => {
                    let msg = format!(
                        "{} has been open behind you for {}",
                        key.label(),
                        human_secs(open_secs)
                    );
                    println!("[{stamp}] 👀 {msg}");
                    events.push(msg);
                }
                Action::Expire { key, tab } => {
                    let label = key.label().to_string();
                    match tab {
                        Some(t) if live => match p.close_tab(&t) {
                            Ok(CloseOutcome::Closed) => {
                                let msg = format!("closed {label}");
                                println!("[{stamp}] 🐾 {msg}");
                                events.push(msg);
                            }
                            Ok(CloseOutcome::NoLongerMatching) => {
                                println!(
                                    "[{stamp}] 🐾 {label} expired, but that tab had already \
                                     moved on — nothing closed"
                                );
                            }
                            Ok(CloseOutcome::Unsupported) => {
                                println!("[{stamp}] 🐾 {label} expired (cannot close here)");
                            }
                            Err(e) => tracing::warn!("closing {label}: {e}"),
                        },
                        Some(t) => {
                            println!("[{stamp}] 🐾 WOULD CLOSE {label} — {}", t.url);
                            events.push(format!("would close {label}"));
                        }
                        None => {
                            let msg = format!("{label} is over its limit");
                            println!("[{stamp}] 🐾 {msg}");
                            events.push(msg);
                        }
                    }
                }
            }
        }

        // Persist about once a minute so a crash costs a minute, not a day.
        if ticks % 60 == 0 {
            if let Err(e) = store::save_day(&tracker, &events) {
                tracing::warn!("saving today: {e}");
            }
        }

        ticks += 1;
        std::thread::sleep(Duration::from_secs(1));
    }
}
