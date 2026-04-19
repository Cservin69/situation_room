//! Tauri commands — actions the frontend triggers.
//!
//! ## Security discipline (MANDATORY)
//!
//! Every `#[tauri::command]` handler in this crate MUST:
//!   1. Validate any URL input via `stockpile_secure::url_guard::UrlGuard`.
//!   2. Validate any path input via `stockpile_secure::fs_guard::FsGuard`.
//!   3. Check any string input against `stockpile_secure::bounds::Bounds`.
//!   4. Never `expose_secret()` on an `ApiKey` except when passing to an
//!      HTTP Authorization header.
//!   5. Return typed errors. Never panic on user input.
//!
//! Enforcement: `cargo clippy` plus a custom check in Phase 4 CI that
//! greps every `#[tauri::command]` handler for the presence of at least
//! one validation call.
//!
//! Examples (Phase 4):
//! - `research_topic(topic: String) -> ResearchPlan`  (topic bounded by
//!   `Bounds::RESEARCH_TOPIC`)
//! - `get_topic_screen(topic: Topic) -> TopicScreen`
//!   (topic validated via Topic::new)
//! - `open_article(url: String) -> ArticleView`  (url via UrlGuard, file
//!   target via FsGuard)
//! - `toggle_offline_mode(enabled: bool) -> ()`  (no input validation needed)
