#!/usr/bin/env bash
# Session 70 — iterator-recipe verification on non-TESLA topics.
#
# ## Why this script exists
#
# Session 69 surprised us: TESLA's Yahoo Finance recipe was authored
# as an iterator and produced 252 observations in one fetch — the
# first time the codebase captured a full time series in a single
# pass. Open question: was v1.21's iterator bias generalised, or
# TESLA-shaped?
#
# This script picks three candidate topics whose source-of-truth
# *should* be iterator-friendly time series, and runs one classify
# + fetch per topic. ~1 LLM call to classify + ~1 to author the
# recipe × 3 topics = ~6 calls + ~3 fetches. Operator's
# spending-cap-tight budget per `feedback_eval_cost_discipline`.
# NOT an eval (no per-trial repetition; this is "does the path work
# at all"). If a topic returns 0 observations on the dashboard, the
# v1.21 iterator bias is more TESLA-shaped than generalised.
#
# ## Topics
#
#   1. "fred unemployment rate" — the FRED UNRATE series. Source:
#      `https://fred.stlouisfed.org/series/UNRATE` (the iterator-
#      friendly HTML table; the FRED API needs a key).
#
#   2. "spot gold price"        — commodity time series. Recipe-author
#      should pick a Yahoo-Finance-shaped JSON endpoint
#      (`/v8/finance/chart/GC=F` is the canonical one).
#
#   3. "noaa daily temperature anomaly"
#                               — climate time series. NOAA's NCEI
#      datasets ship CSV; v1.21 should reach for the iterator path
#      to capture row-by-row entries.
#
# ## What "pass" looks like
#
# For each topic, after `cargo run -p situation_room-desktop` and
# classify + fetch:
#
#   - **Pass**: dashboard's OBSERVATIONS bucket has ≥ 10 rows,
#     produced by one iterator-shaped recipe. The author log shows
#     `extracted_inner` non-null on at least one fetch (ADR 0019).
#   - **Marginal**: 1-9 observations. The recipe found a single
#     scalar but not the iterator — v1.21's iterator bias is
#     present but weak on this topic shape.
#   - **Fail**: 0 observations. v1.21 didn't reach for the iterator
#     on this topic, OR every recipe failed at fetch (separate
#     diagnosis — check the RecipesPanel for the failure class).
#
# Three passes = iterator bias is generalised, the TESLA result
# was the first hit of a settled capability. Two passes = mostly
# generalised, room for a Session 71 prompt edit on the failing
# topic shape. One or zero = TESLA was the outlier; bias is not
# generalised and the v1.21 prompt needs targeted work.
#
# ## Spending discipline
#
# This script does NOT run cargo or invoke the LLM provider. It's
# a runbook for the operator. The operator runs `cargo run -p
# situation_room-desktop` once and works through the three topics
# in the UI, then reports the observation counts.

cat <<'EOF'
Session 70 — iterator-recipe verification (operator-driven).

Three topics to try, one fetch each, then read OBSERVATIONS count:

  1. "fred unemployment rate"
  2. "spot gold price"
  3. "noaa daily temperature anomaly"

For each topic in the desktop app:
  (a) classify the topic
  (b) accept the plan
  (c) run fetch
  (d) record OBSERVATIONS count from the dashboard

Pass criteria per topic:
  ≥ 10 observations → iterator bias hit
  1-9 observations  → marginal
  0 observations    → no iterator

Total spend: ~6-9 LLM calls + ~3 fetches (no per-trial repetition).

If you want to keep it cheaper: pick one topic from the list above
and just see whether the recipe lands as an iterator
(observations ≥ 10) or a scalar (observations < 10). Session 71
candidates branch on this.

Report the counts to the next session as:
  fred:    N
  gold:    N
  noaa:    N
EOF
