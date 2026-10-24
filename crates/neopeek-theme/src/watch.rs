//! Hot reload of a loaded theme file (`--watch-theme`, PLAN.md §6.2's
//! dev aid; off by default). A notify thread re-parses the file on every
//! filesystem event and sends it over a plain channel — the render loop
//! drains with `try_latest` per wake (no locking).

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

pub struct ReloadWatcher {
    rx: mpsc::Receiver<crate::Theme>,
    /// The notify endpoint; kept alive so events keep flowing.
    #[allow(dead_code)] // lifetime anchor only
    watcher: notify::RecommendedWatcher,
    /// The re-parse thread (kept for hygiene; exits with the channel).
    #[allow(dead_code)]
    _feed: std::thread::JoinHandle<()>,
}

impl ReloadWatcher {
    /// Starts watching `path`. `None` when the watcher can't be created
    /// (a hot-reload miss must never break the session — logged by the caller).
    pub fn spawn(path: PathBuf) -> Option<Self> {
        let (tx, rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(move |res| {
            let _ = ev_tx.send(res);
        })
        .ok()?;
        watcher.watch(&path, RecursiveMode::NonRecursive).ok()?;
        let feed = std::thread::Builder::new()
            .name("neopeek-theme-feed".into())
            .spawn(move || {
                // Editors save through several events (write, rename,
                // metadata). Re-parse per event with short retries so a
                // mid-write failure still lands when the write settles.
                let settle = Duration::from_millis(60);
                while ev_rx.recv().is_ok() {
                    for attempt in 0..3 {
                        match crate::load_file(&path) {
                            Ok(theme) => {
                                let _ = tx.send(theme);
                                break;
                            }
                            Err(_) if attempt < 2 => std::thread::sleep(settle),
                            Err(_) => break,
                        }
                    }
                }
            })
            .ok()?;
        Some(Self {
            rx,
            watcher,
            _feed: feed,
        })
    }

    /// The latest theme parsed since the last call (else `None`).
    pub fn try_latest(&self) -> Option<crate::Theme> {
        let mut latest = None;
        while let Ok(theme) = self.rx.try_recv() {
            latest = Some(theme);
        }
        latest
    }
}
