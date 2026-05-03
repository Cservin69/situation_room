# Class B failure case archive

This directory is the evidence base ADR 0012 §"Documenting observed
Class B failures" calls for. **Do not implement the automated
re-author retry loop** until the gate in ADR 0012 §"When to automate"
is met: 10 or more empirically observed, distinctly-shaped Class B
failures across diverse sources and plan types, all documented here.

## Definition (verbatim from ADR 0012)

> **Class B** — recipe authored cleanly, but the extraction pattern
> matched nothing in the fetched bytes. The LLM authored against a
> description of the source or an idealized mental model of its
> content, rather than the actual bytes at runtime.

The deferred automated detection predicate looks for these specific
strings (each must be verified against ≥ 2 observed live failures
before being added — see ADR 0012 §"Class B detection predicate"):

- `matched 0 times` — RegexCapture
- `path matched no nodes` — JsonPath
- `selector matched no elements` — CssSelect
- `no row matched filter` — CsvCell

A failure that doesn't match any of these strings is **not** strict
Class B by the predicate's definition, but it may still belong here
if the root cause is the same shape: an LLM-authored recipe whose
assumption about the source's response shape was wrong. Document
the case anyway, label the taxonomy honestly (e.g. "Class B-adjacent:
extraction succeeded structurally but produced an untyped value"),
and flag it in the case file. Future sessions deciding whether to
extend the predicate need this evidence.

## File naming and schema

Per ADR 0012:

```
docs/failure_cases/class_b/{YYYY-MM-DD}_{source_id}.md
```

Each file must contain:

1. Source id and plan topic.
2. Extraction mode and the failing spec verbatim.
3. The failure message verbatim.
4. The first 512 bytes of the fetched content (or the full content
   if shorter).
5. Whether re-authoring succeeded, failed, or oscillated.
6. The corrected extraction spec if re-authoring succeeded.

When (5) and (6) cannot be filled in within the same session as the
observation, leave them as "Pending" and add a follow-up note —
ADR 0012 explicitly forbids the shortcut of skipping documentation
because the manual fix hasn't been done yet.

## Directory was empty until Session 24

The Session 23 verification run produced one Class B-adjacent
observation (gdelt rate-limited stub-authored to a wrong-field
recipe) but no Class B file landed because no `failure_cases/class_b/`
directory existed. Session 24's verification run (operator-machine,
fresh classification of "venezuela oil production") produced the
first concrete entry, `2026-05-03_world_bank_indicators.md`. The
directory was created in Session 24 alongside that entry; this
README establishes the convention so subsequent sessions have a
known-good schema to follow.

## What does NOT belong here

- **Class C, D, E** as defined in ADR 0012 §"The failure-mode
  taxonomy" — they have different root causes and different
  remediation paths. Re-authoring does not help any of them; logging
  them as Class B candidates pollutes the evidence base. If a Class
  C/D/E archive becomes useful later, create sibling directories.
- **Classifier failures** (UDB-style framing leak, etc.) — those go
  in `failure_cases/classification/` per the convention established
  in Session 15. Different taxonomy, different ADR.
- **Transient runtime failures** (429 rate-limits, network blips,
  DNS, transient TLS handshake failures) — these are not Class B by
  any reading; they're external state at fetch time. The recipe
  may be perfect; the fetch happened to fail. Logging them here
  inflates the gate count with non-evidence.
