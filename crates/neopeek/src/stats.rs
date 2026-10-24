//! `--stats-interval <SECONDS>`: opt-in resident-memory logging. A
//! background thread sleeps the given period, reads `VmRSS` from
//! `/proc/self/status` (Linux only, plain `std::fs` — no new deps) and
//! logs one INFO line: `rss_kb`. This exists so the user can verify the
//! memory fixes on a real host (see the RAM audit: the HUD fontdb and
//! per-frame text shaping were the suspects).
//!
//! The thread is detached; the process exits through the event loop and
//! the kernel reaps it. Logging only starts in the live-window path (CLI
//! actions are short-lived and return before `spawn` is called).

use std::time::Duration;

/// Spawn the periodic RSS logger (never returns; detached). No-op with a
/// warning on non-Linux targets (there is no std RSS read there).
pub fn spawn(every_secs: u64) {
    spawn_rss(every_secs);
}

#[cfg(target_os = "linux")]
fn spawn_rss(every_secs: u64) {
    // Clamp to ≥1 s: 0 = off is filtered by the caller.
    let period = Duration::from_secs(every_secs.max(1));
    if let Err(err) = std::thread::Builder::new()
        .name("rss-stats".to_owned())
        .spawn(move || loop {
            std::thread::sleep(period);
            match read_vm_rss() {
                Some(kb) => {
                    tracing::info!(rss_kb = kb, interval_s = period.as_secs(), "resident memory");
                }
                None => {
                    tracing::warn!("rss read failed (no /proc/self/status VmRSS line)");
                }
            }
        })
    {
        tracing::warn!("rss-stats thread failed to start: {err}");
    }
}

#[cfg(not(target_os = "linux"))]
fn spawn_rss(every_secs: u64) {
    tracing::warn!("--stats-interval is Linux-only (reads /proc/self/status); ignoring");
    let _ = every_secs;
}

/// Current RSS of this process in KiB.
#[cfg(target_os = "linux")]
pub(crate) fn read_vm_rss() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    parse_vm_rss(&status)
}

/// The `VmRSS:` line of a `/proc/*/status` dump → KiB.
/// e.g. `VmRSS:\t  123456 kB`.
#[cfg(target_os = "linux")]
pub(crate) fn parse_vm_rss(status: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    line["VmRSS:".len()..]
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn parses_vmrss_line() {
        let status = "Name:\tneopeek\nVmRSS:\t  123456 kB\nThreads:\t3\n";
        assert_eq!(parse_vm_rss(status), Some(123_456));
        // No tab, single space.
        assert_eq!(parse_vm_rss("VmRSS: 12 kB\n"), Some(12));
        // Missing/odd lines → None, not a panic.
        assert_eq!(parse_vm_rss("VmPeak:\t 999 kB\n"), None);
        assert_eq!(parse_vm_rss("VmRSS:\tkB\n"), None);
        assert_eq!(parse_vm_rss(""), None);
    }

    #[test]
    fn reads_own_rss() {
        let kb = read_vm_rss().expect("test host has /proc/self/status");
        assert!(kb > 0, "rss must be positive, got {kb}");
    }
}
