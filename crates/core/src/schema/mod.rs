//! Schema: the six record types, the envelope they all carry, and the
//! [`Record`] enum that ties them together.
//!
//! See `docs/adr/0003-six-record-types.md` for the rationale.

pub mod records;
pub mod envelope;

pub use envelope::Envelope;
pub use records::Record;
