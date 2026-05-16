//! Hot-reload wrapper around [`AuthorityRegistry`] (Session 84).
//!
//! # Why this module exists
//!
//! Session 82 loaded `config/vocab/authoritative_sources.toml` once
//! at composition-root boot and stored the result as
//! `Arc<AuthorityRegistry>` inside `AppState`. Editing the TOML
//! required restarting the desktop binary before the new entries
//! reached the promote stage — operator-facing friction that gets
//! worse when Session 84 introduces the per-claimant
//! `consensus_quorum` knob (the whole point of the override is rapid
//! iteration on which sources clear the corroboration bar).
//!
//! This module ships the hot-reload surface:
//!
//!   - [`LiveAuthorityRegistry`] — a cheaply-cloneable handle that
//!     wraps `RwLock<Arc<AuthorityRegistry>>`. Readers call
//!     [`LiveAuthorityRegistry::snapshot`] to get an `Arc` view that
//!     doesn't see in-flight reloads; the promote stage clones the
//!     `Arc`'s contents into per-call `PromoteConfig` exactly as
//!     before.
//!   - [`LiveAuthorityRegistry::spawn_watcher`] — a polling thread
//!     that watches the TOML file's modification time at a 2-second
//!     cadence, re-parses on change, swaps the inner `Arc` via a
//!     write lock. Polling instead of `notify` so the workspace
//!     doesn't pick up a platform-specific OS file-watching crate
//!     for what's a single small config file.
//!
//! # Failure posture
//!
//! Parse errors during a re-read are warn-logged and the previous
//! `Arc<AuthorityRegistry>` stays installed. The operator sees the
//! warning in the desktop log without the binary dropping its
//! existing auth pass.

use crate::authoritative::{AuthorityRegistry, AuthorityLoadError};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};
use tracing::{info, warn};

/// Default polling cadence for the file-mtime watcher.
///
/// 2 seconds is a deliberate split between "operator sees the new
/// quorum take effect quickly" (sub-3-second cycle is usable for
/// interactive tuning) and "negligible IPC cost" (one `metadata()`
/// syscall every 2s on a single small file).
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Cheaply-cloneable hot-reload handle for the authoritative
/// registry. Clone the handle to share it across thread/task
/// boundaries; the inner `Arc<RwLock<...>>` is what carries the
/// shared mutability.
#[derive(Clone)]
pub struct LiveAuthorityRegistry {
    inner: Arc<RwLock<Arc<AuthorityRegistry>>>,
    /// Snapshot of the path the watcher polls. Kept here so
    /// diagnostic IPC surfaces (`authoritative_registry_summary`) can
    /// quote the resolved location to the operator.
    source_path: Arc<PathBuf>,
}

impl LiveAuthorityRegistry {
    /// Build a handle pre-populated with `initial`. Used by the
    /// composition root when boot-time loading succeeded; the
    /// watcher (if spawned later) keeps swapping the inner Arc on
    /// file changes.
    pub fn new(initial: AuthorityRegistry, source_path: PathBuf) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Arc::new(initial))),
            source_path: Arc::new(source_path),
        }
    }

    /// Build an empty-registry handle. Test sites + the fallback
    /// path when the composition root fails to find the TOML file at
    /// all. Source path is set to `<empty>` for log clarity.
    pub fn empty() -> Self {
        Self::new(AuthorityRegistry::empty(), PathBuf::from("<empty>"))
    }

    /// Snapshot the current registry. Cheap (single `Arc::clone`).
    /// The returned `Arc` is detached from the inner lock so callers
    /// don't keep the read lock held while running long pipelines.
    pub fn snapshot(&self) -> Arc<AuthorityRegistry> {
        // `read()` returning `PoisonError` means a writer thread
        // panicked mid-swap. We still want to return the last
        // installed registry — the inner `Arc` is intact even when
        // the lock guard reports poisoning. `into_inner` on the
        // poisoned error yields the guard.
        match self.inner.read() {
            Ok(guard) => Arc::clone(&*guard),
            Err(poison) => Arc::clone(&*poison.into_inner()),
        }
    }

    /// The path the watcher is configured against. Surfaced by the
    /// dashboard IPC for "loaded from {path}" copy.
    pub fn source_path(&self) -> &Path {
        self.source_path.as_path()
    }

    /// Re-read the TOML and swap the inner Arc atomically.
    /// Public so test sites and the boot path can trigger a reload
    /// without going through the watcher thread.
    pub fn reload(&self) -> Result<(), AuthorityLoadError> {
        let path = self.source_path.as_path();
        let fresh = AuthorityRegistry::load_from_path(path)?;
        self.install(fresh);
        Ok(())
    }

    /// Replace the inner registry with `next`. Used by `reload` and
    /// by tests that want to inject a precomputed registry without
    /// touching the filesystem.
    pub fn install(&self, next: AuthorityRegistry) {
        let next_arc = Arc::new(next);
        match self.inner.write() {
            Ok(mut guard) => *guard = next_arc,
            Err(poison) => {
                let mut guard = poison.into_inner();
                *guard = next_arc;
            }
        }
    }

    /// Spawn the polling watcher thread. The thread re-checks the
    /// TOML file's `mtime` at `interval` cadence, parses on change,
    /// and swaps the inner `Arc` via [`Self::install`] on success.
    /// Returns immediately; the thread is detached.
    ///
    /// The polling approach (instead of `notify`) deliberately
    /// trades a single sub-millisecond syscall every 2s for not
    /// adding a platform-specific file-watcher dep. The TOML is a
    /// single small file edited rarely; the polling cost is
    /// negligible against any backend the desktop binary already
    /// pays for.
    pub fn spawn_watcher(&self, interval: Duration) {
        let handle = self.clone();
        let path = self.source_path.as_path().to_path_buf();
        let cadence = if interval.is_zero() {
            DEFAULT_POLL_INTERVAL
        } else {
            interval
        };

        std::thread::Builder::new()
            .name("sr-authoritative-watch".to_string())
            .spawn(move || {
                run_watch_loop(handle, &path, cadence);
            })
            .map(|_| ())
            .unwrap_or_else(|e| {
                warn!(
                    error = %e,
                    "failed to spawn authoritative-registry watch thread — hot-reload disabled"
                );
            });
    }
}

/// Body of the watch thread. Pulled out so the test below can
/// exercise the mtime-change detection without spawning an OS
/// thread.
fn run_watch_loop(handle: LiveAuthorityRegistry, path: &Path, interval: Duration) {
    let mut last_seen: Option<SystemTime> = initial_mtime(path);
    info!(
        path = %path.display(),
        cadence_ms = interval.as_millis(),
        "authoritative-registry watcher started"
    );
    loop {
        std::thread::sleep(interval);
        match std::fs::metadata(path).and_then(|m| m.modified()) {
            Ok(mtime) => {
                if last_seen != Some(mtime) {
                    match handle.reload() {
                        Ok(()) => {
                            let snapshot = handle.snapshot();
                            info!(
                                path = %path.display(),
                                entries = snapshot.entries().len(),
                                "authoritative-source registry reloaded"
                            );
                            last_seen = Some(mtime);
                        }
                        Err(e) => {
                            warn!(
                                path = %path.display(),
                                error = %e,
                                "authoritative-source registry reload failed — keeping previous"
                            );
                            // Update last_seen anyway so we don't
                            // spam the warn log every poll cycle on
                            // a syntactically broken edit. Operator
                            // saves a fix → mtime changes again →
                            // we retry.
                            last_seen = Some(mtime);
                        }
                    }
                }
            }
            Err(_) => {
                // File missing/unreadable. Don't change `last_seen`;
                // when the file reappears we'll pick it up.
            }
        }
    }
}

fn initial_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoritative::AuthorityEntry;

    #[test]
    fn snapshot_returns_installed_registry() {
        let initial = AuthorityRegistry::from_entries(vec![AuthorityEntry {
            source_id: "usgs_mcs".into(),
            metric: Some("production".into()),
            topic: None,
            consensus_quorum: None,
        }]);
        let live = LiveAuthorityRegistry::new(initial, PathBuf::from("test.toml"));
        let snap = live.snapshot();
        assert_eq!(snap.entries().len(), 1);
        assert_eq!(snap.entries()[0].source_id, "usgs_mcs");
    }

    #[test]
    fn install_swaps_the_inner_arc() {
        let live = LiveAuthorityRegistry::empty();
        assert!(live.snapshot().is_empty());
        let next = AuthorityRegistry::from_entries(vec![AuthorityEntry {
            source_id: "agency:reuters".into(),
            metric: None,
            topic: None,
            consensus_quorum: Some(2),
        }]);
        live.install(next);
        let snap = live.snapshot();
        assert_eq!(snap.entries().len(), 1);
        assert_eq!(snap.entries()[0].consensus_quorum, Some(2));
    }

    #[test]
    fn snapshot_clones_are_independent_of_subsequent_installs() {
        // Reader holds an Arc<AuthorityRegistry> captured before a
        // writer's install(). The reader's view must not change
        // mid-pipeline — that's the load-bearing guarantee for the
        // promote stage's per-call PromoteConfig.
        let live = LiveAuthorityRegistry::new(
            AuthorityRegistry::from_entries(vec![AuthorityEntry {
                source_id: "v1".into(),
                metric: None,
                topic: None,
                consensus_quorum: None,
            }]),
            PathBuf::from("test.toml"),
        );
        let reader_snapshot = live.snapshot();
        live.install(AuthorityRegistry::from_entries(vec![AuthorityEntry {
            source_id: "v2".into(),
            metric: None,
            topic: None,
            consensus_quorum: None,
        }]));
        let new_snapshot = live.snapshot();
        assert_eq!(reader_snapshot.entries()[0].source_id, "v1");
        assert_eq!(new_snapshot.entries()[0].source_id, "v2");
    }

    #[test]
    fn reload_from_disk_picks_up_edits() {
        // Write the original TOML, build a live handle pointed at
        // it, write a fresh TOML with a different entry, call
        // reload() — the snapshot reflects the new bytes.
        let tmp_dir = std::env::temp_dir();
        let path = tmp_dir.join("sr_session84_authoritative_reload.toml");
        std::fs::write(
            &path,
            r#"
[[authority]]
source_id = "v1"
metric = "production"
"#,
        )
        .expect("write initial toml");
        let initial = AuthorityRegistry::load_from_path(&path).expect("parse initial");
        let live = LiveAuthorityRegistry::new(initial, path.clone());
        assert_eq!(live.snapshot().entries()[0].source_id, "v1");

        std::fs::write(
            &path,
            r#"
[[authority]]
source_id = "v2"
metric = "reserves"
consensus_quorum = 2
"#,
        )
        .expect("rewrite toml");

        live.reload().expect("reload picks up edits");
        let snap = live.snapshot();
        assert_eq!(snap.entries().len(), 1);
        assert_eq!(snap.entries()[0].source_id, "v2");
        assert_eq!(snap.entries()[0].consensus_quorum, Some(2));

        let _ = std::fs::remove_file(&path);
    }
}
