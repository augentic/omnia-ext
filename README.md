# omnia-ext

Guest-side libraries that compose on [omnia](https://github.com/augentic/omnia)
capability traits. They are not part of the runtime: omnia never depends on
this repository. Each crate is an opt-in decorator or helper that a guest
wraps around the SDK, and the same code runs inside a `wasm32-wasip2`
component and against `omnia_test` doubles natively.

## Relation to omnia

Dependency is strictly one-way. The omnia crates are published crates.io
dependencies (currently 0.38.0) declared once under
`[workspace.dependencies]` in `Cargo.toml`, and every `omnia-*` crate moves
together. Until omnia publishes the 0.38.0 capability contract,
`[patch.crates-io]` points every `omnia-*` at omnia's `capablities` branch,
and `cargo update -p omnia-sdk` re-locks them as one. Production backends
live in [omnia-backends](https://github.com/augentic/omnia-backends) and are
a host-side concern; nothing here talks to them.

## Crates

| Crate | Description |
| --- | --- |
| [`omnia-http-cache`](crates/http-cache) | `HttpCache<H, S>` decorator over `HttpClient` + `StateStore`. Requests carrying `Cache-Control` and `If-None-Match` are answered from, and written back through, the injected store. |
| [`omnia-orm`](crates/orm) | `entity!` mapping and typed `SELECT` / `INSERT` / `UPDATE` / `DELETE` builders that render parameterized SQL for `SqlDb` to execute over `omnia:sql`. |

## Quick start

Tasks run through [mise](https://mise.jdx.dev/getting-started.html), which
must be installed by hand; `make <task>` is a pass-through to
`mise run <task>` and fails, rather than installing it, when it is missing.

Build the HTTP cache example guest and run it with the example host.
`UPSTREAM_URL` is read from the runtime environment via `wasi:config`; any
origin that answers `GET /resource` will do:

```shell
make build http-cache
UPSTREAM_URL=https://jsonplaceholder.cypress.io/posts/1 make run http-cache
```

`GET /cached` fetches `{UPSTREAM_URL}/resource` through `HttpCache`. The
first call reaches the origin and is stored for 300 seconds; the second is
served from the store. See [`examples/http-cache/README.md`](examples/http-cache/README.md)
and the crate [header contract](crates/http-cache/README.md).

The ORM example is a two-table CRUD service over the runtime's in-memory
`SQLite`:

```shell
make build orm
make run orm
```

`POST /agencies`, `POST /agencies/{id}/feeds`, `GET /feeds` (a JOIN entity),
`PATCH /agencies/{id}`, and `DELETE /feeds/{id}` each exercise one builder;
the curl walk-through is in [`examples/orm/README.md`](examples/orm/README.md)
and the builder vocabulary in the crate [README](crates/orm/README.md).

## Testing

```shell
mise run test              # cargo nextest run --workspace --all-features --locked
```

The suite is laid out as omnia's three rungs (see omnia's
[Testing Omnia-Based Code](https://github.com/augentic/omnia/blob/main/docs/guides/testing-omnia-code.md)
guide) plus a build gate for the example hosts. Everything runs under
`mise run test`: nothing is `#[ignore]`d, and nothing needs installing
beyond `rust-toolchain.toml`, which carries the `wasm32-wasip2` target the
component rung compiles for.

### Handler rung

`crates/http-cache/tests/cache.rs` drives `HttpCache` natively against
`omnia_test::guest::Provider` (`MatchedHttp` + `Memory`). Scenarios cover
miss-then-hit, `no-cache` refresh (with `max-age`) and bypass (without),
`max-age=0` revalidation, `no-store` bypass, non-2xx and 206 not stored, a corrupt
entry and a store outage both degrading to the origin, an entity-tag that is
no portable key cached over a store that refuses such keys, pass-through
without `Cache-Control` or with only unrecognised directives, and malformed
directives.

`crates/orm/tests/orm.rs` runs each builder's `Query` through the same
`Provider`, whose `ScriptedTables` records the SQL text and `$n` parameters
that reach `SqlDb` and answers with scripted rows. Scenarios cover a
filtered, ordered, limited select mapped back through `Entity::from_row`
(including `NULL` → `None` and RFC 3339 timestamps), insert from an entity,
an `ON CONFLICT` upsert, a filtered update and delete, a JOIN entity with
aliased columns, a missing column failing row mapping, and a limit too
large for a portable integer refusing to build.

### Component rung

`examples/tests/component.rs` boots the HTTP cache example guest — compiled
to a `wasm32-wasip2` component by `examples/build.rs` — through the example
host's own `runtime!` wiring over `omnia_test::host::Backends`. A loopback
origin records hits; one scenario, `cached_miss_then_hit`, asserts both
handler responses, a single origin fetch, and the keyvalue envelope under
the etag's `store_key`.

`examples/tests/orm.rs` boots the ORM example guest the same way, over the
bundle's private in-memory `SQLite`. One scenario, `agency_feed_lifecycle`,
chains the routes in-process: create an agency, list and update it, add a
feed, read the feed back through the JOIN entity carrying the updated agency
name, delete it, read an empty list, and see the second delete answered as
not found.

### Examples gate

`examples/tests/examples.rs::build` runs `cargo build --locked --examples`
from the package root, so both example hosts compile. The hosts in
`examples/http-cache/runtime.rs` and `examples/orm/runtime.rs` are servers
that never exit, so they are compiled, not run; the component rung is where
their wiring is exercised.

## Development

```shell
mise run ci     # fmt-check, clippy (native, then wasm32-wasip2), test, test-docs, docs, vet, deny
mise run check  # local advisories: audit, fmt (in place), lint, outdated, deps
```

`mise run ci` is the whole gate and runs exactly the jobs of the CI workflow:
every rung above, including both component rungs and the examples gate, runs
inside `mise run test`. `mise tasks` lists everything else; the generic Rust
tasks are included from
[`augentic/toolkit`](https://github.com/augentic/toolkit/blob/main/mise/rust.toml)
by `mise.toml`, which adds only the example `build`/`run` tasks and a `wasm`
release build of the guests.

Clippy runs twice: natively with `--all-targets`, then for `wasm32-wasip2`
over libs and examples only (never tests or benches). The whole workspace is
in the wasm32 scope, so every runtime example stays `cfg_select!`-gated to an empty
`main` on wasm32 and every host-only integration test stays
`#![cfg(not(target_arch = "wasm32"))]`. Regenerate the supply-chain files with
`mise run vet-regen`; `vet` itself only checks.

The workspace follows omnia's conventions: stable toolchain
(`rust-toolchain.toml`, with the `wasm32-wasip2` target), edition 2024,
workspace lints, `cargo vet` supply-chain audits (`supply-chain/`), and CI as
thin wrappers over the reusable workflows in `augentic/toolkit`, pinned to a
release tag.

## License

MIT OR Apache-2.0
