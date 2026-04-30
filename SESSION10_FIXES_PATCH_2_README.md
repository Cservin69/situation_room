# Session 10 — round 2 fixes

Apply on top of `session10_fixes_patch` (do not re-extract earlier
patches; this one updates files from them):

    tar -xzf ~/Downloads/session10_fixes_patch_2.tar.gz --strip-components=1 -C .
    cargo check --workspace
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings

## What this fixes

### Real test failure (1 file)

`fetch_executor::tests::author_one_uses_endpoint_hint_url_and_prefetched_excerpt`
was failing on `recipes_succeeded == 1` — got 0. The provider was
called and the prompt content was right (the earlier asserts
passed); the recipe was authored and persisted; but the canned
recipe failed to *succeed at runtime*.

Diagnosis: the canned `csv_cell` recipe had no `row_filter`, and
the test's fixture body had two data rows (`Chile,49000` +
`Australia,88000`). `recipe_apply` correctly rejects ambiguous
multi-row CSVs without a filter — see the existing
`csv_cell_errors_on_ambiguous_multi_row_without_filter` test that
covers the other branch. My happy-path test wanted the success
branch, so it needs a single-row body.

Fix: split `hint_body` (multi-row, asserted to appear in the
prompt) from `recipe_body` (single-row, fed to the recipe-execution
fetch). The pre-fetch and recipe-execution URLs were always
distinct; nothing else needed to change. The fix is in one test;
the production code path is correct as written.

### More pre-existing clippy lints in `crates/storage/` (5 files)

Same situation as round 1: `cargo test` getting further this time
allowed clippy to reach `stockpile-storage`, which had its own
backlog of clippy-1.88 lints in untouched code:

- `crates/storage/src/assertions.rs:38` — `needless_borrows_for_generic_args`
  on `serde_json::to_value(&a.stance)`. `Stance: Copy`, so removing
  `&` is safe; `a` continues to be used.
- `crates/storage/src/entities.rs:151` — `uninlined_format_args`.
- `crates/storage/src/envelope_io.rs:107` — `needless_borrows_for_generic_args`
  on `serde_json::to_value(&d.role)`. `DerivationRole: Copy`, same
  pattern as `Stance`.
- `crates/storage/src/research_plans.rs:23` — `doc_overindented_list_items`.
  The continuation line of a markdown list item was indented by 11
  more spaces than the item text. Fixed to match the item-text
  column (delta 0).
- `crates/storage/src/migrate.rs:95` — `uninlined_format_args` on
  `format!("... {}", e)`. Inlined `e`; the field-access args
  (`m.version`, `m.description`) were rebound to locals so the
  format string is uniformly `{ident}` form.

### Proactive scan (0 hits in unmodified code)

I scanned all remaining unmodified `.rs` files for the same lint
families that have fired so far:

- `uninlined_format_args` with bare-ident args → 0 likely hits.
- `needless_borrows_for_generic_args` on `serde_json::to_value(&_)`
  → 11 occurrences, all on non-Copy struct types where the borrow
  is required.
- `doc_overindented_list_items` with significant over-indent
  (delta ≥ 4) → 0 hits.
- `manual_repeat_n` (`iter::repeat().take()`) → 0 occurrences.
- `io_other_error` (`io::Error::new(ErrorKind::Other, _)`) → 0
  occurrences outside the already-fixed file.

If clippy 1.88 fires on a different lint family I haven't
anticipated, send the output and I'll knock it out.

## Files in this patch

    crates/pipeline/src/fetch_executor.rs    (UPDATED: test fixture fix only)
    crates/storage/src/assertions.rs         (NEW: clippy fix)
    crates/storage/src/entities.rs           (NEW: clippy fix)
    crates/storage/src/envelope_io.rs        (NEW: clippy fix)
    crates/storage/src/migrate.rs            (NEW: clippy fix)
    crates/storage/src/research_plans.rs     (NEW: clippy fix — doc only)

Six files. None of the round-1 files needed re-shipping; they're
already in place from `session10_fixes_patch`.
