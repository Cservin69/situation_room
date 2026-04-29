//! Assertion → Observation/Event/Relation promotion.
//!
//! Two pathways:
//! 1. Authoritative: an Assertion from a source designated authoritative for
//!    this metric promotes immediately.
//! 2. Consensus: N independent high-confidence Assertions agreeing on a
//!    claim promote together, with provenance pointing to all supporters.
//!
//! See ADR 0004. Phase 3.
