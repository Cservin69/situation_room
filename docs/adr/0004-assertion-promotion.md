# ADR 0004 — Assertion promotion model

**Status**: Accepted
**Date**: 2026-04-19

## Decision

Assertions are first-class records, separate from Observations. Promotion
from Assertion to Observation/Event/Relation happens via two pathways:
authoritative-source designation, or N-source consensus. The Assertion
layer is preserved indefinitely so disagreement between sources remains
queryable.

## Rationale

[To be written together. Captures why mixing extracted-from-news Assertions
with directly-fetched Observations would corrupt the trustworthiness
guarantee, and how preserving Assertions enables the contrarian/disagreement
detection that anomaly detectors depend on.]
