# Session 3 — fixes patch 2 (pre-3c.4)

Applies cleanly on top of the first fixes patch. Two remaining
failures from the last run addressed; both were mine to fix.

    cd /Users/aben/RustroverProjects/situation_room
    tar -xzvf ~/Downloads/situation_room_session3_fixes2_patch.tar.gz

## What failed

### 1. Assertion missing claimant and stance

Compile error in recipe_apply.rs. I constructed Assertion with
`{id, dedup_key, envelope, content}` fields, but Assertion actually
carries two more: `claimant: EntityId` and `stance: Stance`.

This exposed a real design gap, not just a typo. Assertions wrap
content with metadata that records *who* is claiming and *how
strongly* — that metadata can't be populated from a recipe's
scalar field-mappings. Assertions are the LLM extraction layer's
job (ADR 0004); the recipe-apply runtime (ADR 0007) produces
observations, events, and relations deterministically.

Fix: narrowed the surface area so the authoring schema and the
runtime tell the same story.

- `AuthoredRecordType` in recipe_author.rs: dropped the
  `Assertion` variant. The schema the LLM sees now only allows
  observation / event / relation. A well-behaved LLM can't
  hallucinate an Assertion recipe; a badly behaved one fails the
  serde deserialization step with a clear error.
- `build_record` in recipe_apply.rs: the `RecordType::Assertion`
  arm now returns `ApplyError::Binding` alongside Document and
  Entity, with a reason string that names the architectural gap
  ("assertions carry a claimant and stance that recipe
  field-mappings don't populate — see ADR 0007 and ADR 0004").
- The prompt at config/prompts/recipe_author.md had told the LLM
  assertion was a valid choice. Updated to match the schema and
  added a v1.1 changelog entry.

Files changed: crates/pipeline/src/recipe_apply.rs,
crates/pipeline/src/recipe_author.rs,
config/prompts/recipe_author.md.

### 2. fs_guard accepts_plain_filename — macOS symlink

After the parallel-race fix, the test surfaced a different
failure: `assert!(resolved.starts_with(&root))` fails, because
on macOS `std::env::temp_dir()` returns `/var/folders/...` but
`/var` is a symlink to `/private/var`. `FsGuard::new` canonicalizes
the root into `/private/var/...`, so `resolve()` correctly produces
a path under `/private/var/...` — but the test was comparing
against the original un-canonicalized `root`, which sits under
`/var/...`. Different prefixes, same directory.

The security property is fine; the test was wrong. Fix: the test
now canonicalizes `root` before the `starts_with` check, with a
comment naming the macOS case so nobody reverts it later.

Files changed: crates/secure/src/fs_guard.rs.

## What to run

    cargo check --workspace
    cargo test -p situation_room-secure
    cargo test -p situation_room-pipeline

Both should now be fully green. Everything else was already green
in the previous run.

## Files in this archive

    SESSION3_FIXES2_PATCH.md
    crates/pipeline/src/recipe_apply.rs
    crates/pipeline/src/recipe_author.rs
    crates/secure/src/fs_guard.rs
    config/prompts/recipe_author.md
