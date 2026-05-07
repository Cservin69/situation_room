# situation_room — Session 35 patch 2 (registry expansion)

**Trigger:** Session 35's first patch shipped recipe_author v1.11
(plan-first frame inversion) and research_classifier v1.5
(multi-source by default). The next live run of `hungarian
barley production` (2026-05-07) confirmed v1.5 worked
end-to-end at the **classifier** level: the plan nominated 5
sources across tiers (KSH primary, Eurostat secondary, World
Bank secondary, FAO STAT secondary, EU CAP/DG AGRI primary).

The executor, however, ran only **1** recipe — `total_sources=1`
in the fetch_executor logs. Of the 5 nominations, only
`world_bank_indicators` was registered in `config/sources.toml`;
the other 4 were nominated by description with empty
`preferred_source_ids`, so the executor had no `SourceDescriptor`
for them and skipped them.

This is structurally correct (the executor cannot fabricate an
endpoint from prose), but it exposes the bottleneck v1.5 just
unblocked: **the multi-source norm cannot deliver records until
the registry catches up to it.** With 12 sources registered and
most commodities/finance-tilted, agriculture / EU statistics /
country-specific primary sources are absent. KSH alone would
have made the run a 2-source one, qualitatively different from
1-source.

This patch expands the registry from 12 to 17.

## What this patch ships

A one-file patch:

1. `config/sources.toml` — appends 5 new source descriptors
   between `imf_weo` and the `# ---- Authoritative secondary ----`
   block, preserving the file's existing tier ordering.

No prompt changes. No code changes. No DTO changes. No migration.
`cargo check`, `cargo test`, ts-rs codegen, and the desktop UI
are all unaffected. The next time the classifier runs, the LLM
sees the expanded registry via `{{REGISTERED_SOURCES}}` and can
nominate any of the 17 ids; the next time the recipe author
runs against any of the 5 new ids, it pre-fetches against the
new `endpoint_hint` and authors with real bytes.

## The 5 new sources

Tiered, with rationale per entry. Each carries a real
`endpoint_hint` per the file's own discipline (Session 23
finding: an endpoint hint is the highest-leverage configuration
concern; without it the recipe is stub-authored).

### `eurostat`

Authoritative primary for EU member-state statistics
(harmonized national data across all 27 + EFTA). Highest-leverage
addition: covers economy, agriculture, energy, environment,
labour, demographics across every EU country. The hint targets
the cereals-production dataset `tag00115` — small JSON-stat
response that teaches the LLM the JSON-stat envelope shape
(distinct from World Bank's flat array). LLM substitutes the
dataset code in the URL path per plan; URL discipline rules
apply.

### `oecd`

Authoritative secondary for cross-country comparison studies
across 38 OECD members + selected non-members. Covers national
accounts, employment, education, health, environment, public
finance. The hint targets the Quarterly National Accounts (QNA)
dataset for one country, one measure, one quarter — small
SDMX-JSON response. The LLM learns the SDMX-JSON envelope
(more complex than World Bank's flat shape; values are nested
in `dataSets[0].observations` keyed by dimension index strings).

### `faostat`

Authoritative primary for global agriculture, food security,
forestry, fisheries. The right cross-country source for
agricultural production and trade statistics. The hint targets
`QCL` (production - crops and livestock) for Hungary (area 97),
barley (item 44), one recent year — directly demonstrative for
the failure case that triggered Session 35. The LLM learns the
record shape (`Area Code`, `Item Code`, `Element Code`, `Year`,
`Value`, `Unit`) and the URL pattern with substitutable
parameters.

### `ksh_hungary`

Authoritative primary for Hungarian domestic statistics. Where
Eurostat re-publishes KSH submissions on a lag, KSH is the more
current source for Hungarian-side numbers. The hint targets the
STADAT English directory — a stable HTML index of all
published statistical tables. The LLM uses it as a discovery
surface and authors `css_select` against specific table URLs
(or follows the per-table CSV/XLSX download for `csv_cell`).

### `eu_cap`

Authoritative for EU agricultural markets and CAP monitoring
via DG AGRI's market observatories and the agri-food data
portal. Caveat documented in the descriptor: many DG AGRI
dashboards are JS-rendered SPAs with the underlying data feeds
reachable but not discoverable from the dashboard URL alone.
The hint targets the data portal landing page; the LLM is
expected to hunt the per-topic dashboard's data feed per the
Session 21 "Hunt the URL end-to-end" discipline. Some recipes
will surface as stub-authored when the dashboard's data feed
is not findable from the prefetch alone — ADR 0014's chip
mechanic surfaces this honestly.

## What is NOT in this patch

- **No code.** Pure config.
- **No prompt change.** v1.11 / v1.5 stand. The classifier
  picks up the new ids via `{{REGISTERED_SOURCES}}` substitution
  at the next call; the recipe author picks up the new endpoint
  hints when invoked against any of the new ids.
- **No IEA, no USDA PSD, no Bloomberg/Reuters/Argus/Fastmarkets.**
  Each was considered:
    - **IEA**: most data paywalled or registration-gated; public
      data tools at iea.org are JS-rendered. Would be stub-authored
      via ADR 0014. Defer until either (a) a free data API
      surface is registered or (b) a vendor relay is wired.
    - **USDA PSD**: PSD Online's bulk downloads are zip files,
      which the executor's pre-fetch cannot handle directly; the
      OpenData API requires a key not currently provisioned.
      Defer pending a session that wires either zip handling or
      the API key surface.
    - **Bloomberg / Reuters / Argus / Fastmarkets**: commercial
      paywalled; no public endpoint hint. The classifier may
      still nominate them by description (lower-priority,
      `preferred_source_ids` empty) — that is a feature of the
      v1.5 prompt's description-only nomination path.
- **No re-classify of existing plans.** Plans classified before
  the registry expansion stay as they are. The expansion changes
  the shape of *future* classifications.
- **No retroactive recipe-author re-runs.** Recipes already in
  the store stay as data. Reauthor-on-flag remains the
  escalation surface.

## What changes for the operator

After this patch lands, re-running `hungarian barley production`
(or any classify-and-fetch) should show:

- **Classifier output**: nominations may use any of the 17 ids,
  not 12. The Hungarian barley plan in particular should
  nominate `ksh_hungary`, `eurostat`, `faostat`, `eu_cap`, and
  `world_bank_indicators` — all 5 with non-empty
  `preferred_source_ids`.
- **Executor**: `total_sources=5` (or close — the LLM's
  nominations vary per run) instead of `total_sources=1`. Five
  recipe-author runs in parallel-ish (each ~10–60s depending on
  source complexity), each producing either an authored recipe
  or a structured decline.
- **Fetch report**: a multi-row history. Some recipes succeed,
  some decline, some fail at apply (the eu_cap hint pointing at
  a JS-rendered surface in particular). The chip mechanic
  surfaces the per-recipe state. This is the multi-source
  workstation v1.5 was meant to deliver.

## Apply

```
cd /Users/aben/RustroverProjects/situation_room
tar -xzf ~/Downloads/situation_room_session35_p2.tar.gz --strip-components=1 -C .
```

The tarball overwrites one file: `config/sources.toml`. Verify
by counting `[[source]]` blocks:

```
grep -c '^\[\[source\]\]' config/sources.toml
```

Should print `17`. Then re-run the desktop, classify
`hungarian barley production` (or any topic that benefits from
breadth), and observe the executor running multiple recipes.

## What this exposes for Session 36

The 5 new sources will not all author cleanly on first try.
Expected failure shapes:

- **`eu_cap`**: JS-rendered dashboard. Pre-fetch may land on a
  near-empty HTML shell. Recipes likely stub-authored. Honest;
  ADR 0014's chip surfaces it.
- **`oecd`**: SDMX-JSON envelope is more complex than World
  Bank's flat shape. Recipe author may produce positional path
  recipes that fail at apply (analogous to the World Bank
  null-position failure family). The legible apply-error from
  Session 33 will catch this.
- **`faostat`**: REST API param-naming may shift between
  domains. Recipes should work for `QCL` (matches the hint) but
  may need re-authoring for `TCL`, `FBS`, etc. Handled via the
  reauthor flow (ADR 0012 amendment 1).
- **`ksh_hungary`**: STADAT individual-table HTML structure
  varies by table. Recipe author should pick a table URL via
  the directory and `css_select` against that — but some tables
  are pivoted (years across columns, not rows). The recipe
  author's row-filter discipline handles this; first-run
  recipes may need reauthor passes.
- **`eurostat`**: JSON-stat envelope is non-trivial. Recipe
  author may struggle on the first pass; the v1.11 frame
  inversion discipline (read the plan first, identify the
  metric the plan asks for, decline if the catalog doesn't
  cover it) should still apply. JSON-stat envelope traversal
  may surface as a new failure family; if so, that's the
  Session 36 prompt revision opportunity (in the URL discipline
  / type-honesty subsections, *not* as another top-level frame
  shift).

The empirical signal we now want is whether the multi-source
workstation, with 17 registered sources, produces records
across at least some of the 5 nominations on a real plan. One
or two records flowing through end-to-end via at least one
non-`world_bank_indicators` source is the closing-of-the-loop
moment — the proof that v1.5 + this registry expansion + the
existing executor machinery deliver the Palantir-shape product
ADR 0007 amendment 6 ratifies as the architectural norm.

End of patch 2.
