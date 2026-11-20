## 0.36.0

Unreleased

### Changed

- **Breaking:** MSRV is Rust 1.100, and guests build for `wasm32-wasip3`,
  because omnia 0.38 runs WASI 0.3 guests alone.
- **Breaking:** `omnia-orm` re-exports `Value` in place of `DataType`,
  `Entity::from_row` and `FetchValue::fetch` return `omnia_sdk::Result`,
  unsigned values bind as `Int64`, and a `None` field binds as `Null`. A
  row that does not map fails with the code `invalid`. A built `Query` runs
  on an `omnia:sql` `Database`, through `query_as` and `exec`.
- **Breaking:** `Value`'s cases carry their `omnia:sql` names (`Text`,
  `Float32`, `Float64`), and a `DateTime<Utc>` binds as `TimestampTz`.

### Removed

- **Breaking:** `omnia-http-cache`. Its `HttpCache`, `Control`, and
  `store_key` are `omnia_sdk::http::cache`, behind `omnia-sdk`'s
  `http-cache` feature, with the same header contract and store keys.

---

Release notes for previous releases can be found on the respective release branches of the repository.

<!-- ARCHIVE_START -->
* [0.35.x](https://github.com/augentic/omnia-ext/blob/release-0.35.0/RELEASES.md)
* [0.34.x](https://github.com/augentic/omnia-ext/blob/release-0.34.0/RELEASES.md)
