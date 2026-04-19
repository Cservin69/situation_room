# Adding a new data source

1. Create a new folder under `crates/sources/src/adapters/your_source/`.
2. Implement the [`Source`](../../crates/sources/src/traits.rs) trait.
3. Register the adapter in `crates/sources/src/adapters/mod.rs`.
4. Add a config file at `config/sources/your_source.toml` declaring the
   schedule, license, and (if applicable) what metrics this source is
   authoritative for.
5. Add fixtures under `tests/fixtures/your_source/` and a golden test in
   `tests/golden/`.
6. Document the source in `docs/sources/source_catalog.md`.
