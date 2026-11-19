## 0.36.0

Unreleased

### Changed

- **Breaking:** MSRV is Rust 1.100, and guests build for `wasm32-wasip3`,
  because omnia 0.38 runs WASI 0.3 guests alone.
- **Breaking:** `HttpCache` wraps an `omnia:keyvalue` `Bucket` and sends
  through `omnia_sdk::http::fetch`, as omnia's SDK has no capability traits.
  `HttpCache::new(bucket).fetch(request)` replaces the `HttpClient` and
  `StateStore` decorator, so a caller opts in at the call site rather than
  at provider assembly.
- **Breaking:** `HttpCache` keys entries on `store_key`, a portable key
  derived from the entity-tag, so an entry an older cache wrote is a miss.
  A malformed `Cache-Control` fails with the code `invalid`.
- **Breaking:** `omnia-orm` re-exports `Value` in place of `DataType`,
  `Entity::from_row` and `FetchValue::fetch` return `omnia_sdk::Result`,
  unsigned values bind as `Int64`, and a `None` field binds as `Null`. A
  row that does not map fails with the code `invalid`. A built `Query` runs
  on an `omnia:sql` `Database`, through `query_as` and `exec`.

---

Release notes for previous releases can be found on the respective release branches of the repository.

<!-- ARCHIVE_START -->
* [0.35.x](https://github.com/augentic/omnia-ext/blob/release-0.35.0/RELEASES.md)
* [0.34.x](https://github.com/augentic/omnia-ext/blob/release-0.34.0/RELEASES.md)
