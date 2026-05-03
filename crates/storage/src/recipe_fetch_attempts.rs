//! Recipe fetch-attempt storage — Track A, ADR 0012 amendment 1.
//!
//! Persists per-(recipe, run) bytes-and-failure tuples for the manual
//! re-author flow. When a recipe fails at the apply stage, the
//! executor records the failure message *and* the bytes it actually
//! saw so the re-author command later has ground truth to author the
//! next recipe against.
//!
//! ## Why per-(recipe, run), not per-recipe-id-only
//!
//! A single recipe may be fetched many times across many runs. The
//! "latest attempt" is the audit-the-failure shape the manual
//! re-author flow needs; the per-run row preserves the run's audit
//! context (which Run-fetch-button click triggered this attempt) for
//! a later "show me the runs in which this recipe failed" view.
//!
//! Today only the latest attempt is read by the re-author command
//! ([`Store::latest_attempt_for_recipe`]). Tomorrow, when a "fetch
//! history" panel earns its weight, the per-(recipe, run) shape is
//! already there — no schema migration required.
//!
//! ## Why we don't capture every fetch
//!
//! Today only failed-apply attempts are captured. Capturing every
//! successful fetch's bytes would 10× the table size for a use case
//! that doesn't yet exist; the existing record's provenance string
//! already names the source for ADR 0007's traceability promise. When
//! a "what bytes produced this record" panel earns its weight, this
//! same table can absorb successful attempts by changing the write
//! path; the schema doesn't need to change.
//!
//! ## Truncation discipline
//!
//! [`MAX_EXCERPT_BYTES`] = 64 KiB. The write path is responsible for
//! truncating to a UTF-8 char boundary; the load path treats the
//! column as "head of the bytes, possibly partial." See
//! [`truncate_excerpt`] for the truncation algorithm — it mirrors the
//! same lossy-decode + char-boundary discipline `prefetch_excerpt`
//! uses in `fetch_executor::author_one`, so the bytes the operator
//! reviews in the re-author dialog match the bytes the recipe author
//! saw at re-author time.

use chrono::{DateTime, Utc};
use duckdb::params;
use uuid::Uuid;

use crate::connection::Store;
use crate::{Result, StorageError};

/// Maximum size of the bytes excerpt captured per attempt. Documented
/// in `migrations/0013_recipe_fetch_attempts.sql`'s comment block.
///
/// 64 KiB is a balance: large enough that a typical RSS feed, small
/// JSON API response, or HTML article body fits in full; small enough
/// that storage doesn't bloat from API responses that vary in shape.
pub const MAX_EXCERPT_BYTES: usize = 64 * 1024;

/// Columns the storage layer writes when an attempt is recorded.
#[derive(Debug, Clone)]
pub struct RecipeFetchAttemptRow {
    pub id: Uuid,
    pub recipe_id: Uuid,
    pub run_id: Uuid,
    pub attempted_at: DateTime<Utc>,
    pub succeeded: bool,
    /// `None` on success, `Some(msg)` on any failure. The message is
    /// the same one that lands in `RecipeOutcome::Failed::message`,
    /// captured here so the re-author dialog can show it without
    /// joining back through `fetch_runs`.
    pub failure_message: Option<String>,
    /// Head-of-bytes excerpt; up to [`MAX_EXCERPT_BYTES`]. `None` when
    /// no bytes were obtained (e.g. the failure happened at fetch
    /// stage, before any body was read). `Some(s)` when bytes were
    /// fetched, regardless of success — the re-author dialog shows
    /// the excerpt so the operator can correlate the failure with the
    /// actual response shape.
    pub bytes_excerpt: Option<String>,
}

/// An attempt row as it comes back out of storage. Same shape as
/// [`RecipeFetchAttemptRow`].
#[derive(Debug, Clone)]
pub struct StoredRecipeFetchAttempt {
    pub id: Uuid,
    pub recipe_id: Uuid,
    pub run_id: Uuid,
    pub attempted_at: DateTime<Utc>,
    pub succeeded: bool,
    pub failure_message: Option<String>,
    pub bytes_excerpt: Option<String>,
}

impl Store {
    /// Insert a recipe fetch attempt. Errors on a primary-key conflict
    /// — the caller mints a fresh UUIDv7 per attempt.
    pub fn insert_recipe_fetch_attempt(&self, r: &RecipeFetchAttemptRow) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        conn.execute(
            "INSERT INTO recipe_fetch_attempts (
                id, recipe_id, run_id, attempted_at,
                succeeded, failure_message, bytes_excerpt
             ) VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![
                r.id,
                r.recipe_id,
                r.run_id,
                r.attempted_at,
                r.succeeded,
                r.failure_message,
                r.bytes_excerpt,
            ],
        )
        .map_err(StorageError::DuckDb)?;

        Ok(())
    }

    /// Fetch the most recent attempt for a recipe. Returns `Ok(None)`
    /// if no attempt exists. The "most recent" criterion is the
    /// `attempted_at` column descending; the
    /// `recipe_fetch_attempts_recipe_id_attempted_at_idx` index from
    /// migration 0013 makes this an index-ordered read.
    ///
    /// This is the read the manual re-author command performs to find
    /// the bytes the operator wants to author against.
    pub fn latest_attempt_for_recipe(
        &self,
        recipe_id: Uuid,
    ) -> Result<Option<StoredRecipeFetchAttempt>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| StorageError::Other(format!("connection poisoned: {e}")))?;

        let mut stmt = conn
            .prepare(
                "SELECT id, recipe_id, run_id, attempted_at,
                        succeeded, failure_message, bytes_excerpt
                 FROM recipe_fetch_attempts
                 WHERE recipe_id = ?
                 ORDER BY attempted_at DESC
                 LIMIT 1",
            )
            .map_err(StorageError::DuckDb)?;

        let mut rows = stmt
            .query(params![recipe_id])
            .map_err(StorageError::DuckDb)?;

        if let Some(row) = rows.next().map_err(StorageError::DuckDb)? {
            Ok(Some(row_to_stored(row)?))
        } else {
            Ok(None)
        }
    }
}

/// Truncate `bytes` to at most [`MAX_EXCERPT_BYTES`], decoding lossily
/// to UTF-8 and respecting char boundaries.
///
/// Mirrors the discipline `prefetch_excerpt` in `fetch_executor.rs`
/// uses for the recipe-author's `document_excerpt` so the bytes the
/// operator reviews in the re-author dialog are byte-identical to what
/// the recipe author saw at re-author time. The lossy decode replaces
/// invalid UTF-8 with `U+FFFD`; the char-boundary truncation prevents
/// a half-sliced multi-byte character at the cap.
pub fn truncate_excerpt(bytes: &[u8]) -> String {
    let head = if bytes.len() > MAX_EXCERPT_BYTES {
        &bytes[..MAX_EXCERPT_BYTES]
    } else {
        bytes
    };
    let lossy = String::from_utf8_lossy(head).into_owned();
    if lossy.len() > MAX_EXCERPT_BYTES {
        // After lossy decode the string may still exceed the cap if
        // the source had many invalid bytes that each expanded to a
        // 3-byte replacement char. Truncate at a char boundary
        // ≤ MAX_EXCERPT_BYTES.
        let mut end = MAX_EXCERPT_BYTES;
        while end > 0 && !lossy.is_char_boundary(end) {
            end -= 1;
        }
        lossy[..end].to_string()
    } else {
        lossy
    }
}

fn row_to_stored(row: &duckdb::Row<'_>) -> Result<StoredRecipeFetchAttempt> {
    Ok(StoredRecipeFetchAttempt {
        id: row.get(0).map_err(StorageError::DuckDb)?,
        recipe_id: row.get(1).map_err(StorageError::DuckDb)?,
        run_id: row.get(2).map_err(StorageError::DuckDb)?,
        attempted_at: row.get(3).map_err(StorageError::DuckDb)?,
        succeeded: row.get(4).map_err(StorageError::DuckDb)?,
        failure_message: row.get(5).map_err(StorageError::DuckDb)?,
        bytes_excerpt: row.get(6).map_err(StorageError::DuckDb)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn sample_attempt(recipe_id: Uuid, run_id: Uuid) -> RecipeFetchAttemptRow {
        RecipeFetchAttemptRow {
            id: Uuid::now_v7(),
            recipe_id,
            run_id,
            attempted_at: Utc.with_ymd_and_hms(2026, 5, 3, 16, 10, 20).unwrap(),
            succeeded: false,
            failure_message: Some("extraction [regex_capture]: pattern matched nothing".into()),
            bytes_excerpt: Some(
                "<rss><channel><title>BBC News</title><item><title>example</title></item></channel></rss>"
                    .into(),
            ),
        }
    }

    #[test]
    fn attempt_round_trips() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let recipe_id = Uuid::now_v7();
        let run_id = Uuid::now_v7();
        let row = sample_attempt(recipe_id, run_id);
        store.insert_recipe_fetch_attempt(&row).unwrap();

        let got = store
            .latest_attempt_for_recipe(recipe_id)
            .unwrap()
            .expect("attempt should exist");
        assert_eq!(got.id, row.id);
        assert_eq!(got.recipe_id, recipe_id);
        assert_eq!(got.run_id, run_id);
        assert!(!got.succeeded);
        assert_eq!(
            got.failure_message.as_deref(),
            Some("extraction [regex_capture]: pattern matched nothing")
        );
        assert!(got.bytes_excerpt.as_ref().unwrap().contains("BBC News"));
    }

    #[test]
    fn latest_attempt_returns_none_when_recipe_has_no_attempts() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let recipe_id = Uuid::now_v7();
        let got = store.latest_attempt_for_recipe(recipe_id).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn latest_attempt_picks_newest_by_attempted_at() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let recipe_id = Uuid::now_v7();

        let mut older = sample_attempt(recipe_id, Uuid::now_v7());
        older.attempted_at = Utc.with_ymd_and_hms(2026, 5, 1, 10, 0, 0).unwrap();
        older.failure_message = Some("older failure".into());
        store.insert_recipe_fetch_attempt(&older).unwrap();

        let mut newer = sample_attempt(recipe_id, Uuid::now_v7());
        newer.attempted_at = Utc.with_ymd_and_hms(2026, 5, 3, 16, 10, 20).unwrap();
        newer.failure_message = Some("newer failure".into());
        store.insert_recipe_fetch_attempt(&newer).unwrap();

        let got = store
            .latest_attempt_for_recipe(recipe_id)
            .unwrap()
            .expect("attempt should exist");
        assert_eq!(got.id, newer.id);
        assert_eq!(got.failure_message.as_deref(), Some("newer failure"));
    }

    #[test]
    fn latest_attempt_filters_by_recipe_id() {
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();

        let recipe_a = Uuid::now_v7();
        let recipe_b = Uuid::now_v7();
        let run_id = Uuid::now_v7();

        let row_a = sample_attempt(recipe_a, run_id);
        store.insert_recipe_fetch_attempt(&row_a).unwrap();

        let row_b = sample_attempt(recipe_b, run_id);
        store.insert_recipe_fetch_attempt(&row_b).unwrap();

        let got_a = store
            .latest_attempt_for_recipe(recipe_a)
            .unwrap()
            .expect("a should have an attempt");
        let got_b = store
            .latest_attempt_for_recipe(recipe_b)
            .unwrap()
            .expect("b should have an attempt");

        assert_eq!(got_a.recipe_id, recipe_a);
        assert_eq!(got_b.recipe_id, recipe_b);
        assert_ne!(got_a.id, got_b.id);
    }

    #[test]
    fn truncate_excerpt_passes_short_input_through() {
        let bytes = b"short input";
        let s = truncate_excerpt(bytes);
        assert_eq!(s, "short input");
    }

    #[test]
    fn truncate_excerpt_caps_at_max() {
        // 80 KiB of ASCII — exceeds MAX_EXCERPT_BYTES.
        let bytes = vec![b'a'; 80 * 1024];
        let s = truncate_excerpt(&bytes);
        assert!(s.len() <= MAX_EXCERPT_BYTES);
        assert!(s.starts_with("aaaa"));
    }

    #[test]
    fn truncate_excerpt_handles_invalid_utf8() {
        let mut bytes = b"prefix ".to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe, 0xfd]); // invalid UTF-8
        bytes.extend_from_slice(b" suffix");
        let s = truncate_excerpt(&bytes);
        assert!(s.starts_with("prefix "));
        assert!(s.contains("suffix"));
    }

    #[test]
    fn truncate_excerpt_respects_char_boundary_at_cap() {
        // Construct bytes that, when lossy-decoded, would produce a
        // string straddling MAX_EXCERPT_BYTES at a multi-byte char.
        // We use a ‹é› (2-byte UTF-8) right at the boundary.
        let mut bytes = vec![b'a'; MAX_EXCERPT_BYTES - 1];
        bytes.extend_from_slice(b"\xc3\xa9"); // é
        let s = truncate_excerpt(&bytes);
        // The truncated string must remain valid UTF-8 (Rust strings
        // always are; the test asserts the boundary backoff didn't
        // panic and produced a sensible head of the input).
        assert!(s.len() <= MAX_EXCERPT_BYTES);
        // Either the é made it in (length < cap) or it was excluded
        // (length is at the cap minus the partial char). Both are
        // valid; the invariant is the string is valid UTF-8.
        s.chars().next(); // would panic on invalid UTF-8 — we know it doesn't
    }
}
