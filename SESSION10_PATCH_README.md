# Session 10 patch — Option F (pre-fetch + endpoint_hint + prompt v1.3)

Apply from the repo root:

    tar -xzf ~/Downloads/session10_patch.tar.gz --strip-components=1 -C .

Then verify:

    cargo check --workspace
    cargo test --workspace
    cargo clippy --workspace --all-targets -- -D warnings

The patch is **not compiler-verified** — it was authored in a
sandbox without `cargo`. See `STOCKPILE_HANDOFF_SESSION11.md` §"Build
/ test state" for the type-checked-by-eyeball constraint and the
short list of failure modes I'd expect.

## Files changed

    apps/desktop/src-tauri/src/main.rs
    apps/situation_room/src/main.rs
    config/prompts/recipe_author.md
    config/sources.toml
    crates/api/src/commands.rs
    crates/pipeline/src/fetch_executor.rs
    crates/pipeline/src/research_classifier.rs
    STOCKPILE_HANDOFF_SESSION11.md         (new)

## What this patch does

Resolves the Session 9 production-run finding that the Level-2
recipe author was echoing `https://example.invalid/{source_id}`
back into recipes, causing 0 of 3 recipes to produce a record on
"bulgaria elections 2026."

The fix:

1. New `endpoint_hint: Option<String>` field on `SourceDescriptor`.
2. The fetch executor pre-fetches the hint before authoring and
   passes the real URL + bytes to the recipe-author prompt.
3. Recipe-author prompt v1.3 explicitly forbids `example.invalid`
   echo-back.
4. `config/sources.toml` carries hints for the five sources where
   they help most: `world_bank_indicators`, `gdelt`, `eur_lex`,
   `csv_demo`, `json_demo`.

Fallbacks are conservative: a missing descriptor, missing hint,
unparseable hint, or fetch failure all degrade to the
pre-Session-10 behaviour (placeholder URL + stub excerpt) with a
logged warning. Output contract on the recipe-author side is
unchanged, so existing authored recipes don't need re-authoring.

See `STOCKPILE_HANDOFF_SESSION11.md` for the full account of what
shipped and what Session 11 should do next (P1 = build, P2 = E
promotion).
