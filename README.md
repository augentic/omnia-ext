# omnia-ext

Guest-side libraries that compose on the guest bindings of
[omnia](https://github.com/augentic/omnia)'s `omnia-sdk`. They are not part
of the runtime: omnia never depends on this repository. Each crate is an
opt-in helper that a guest calls where it wants it, beside the SDK.

## Relation to omnia

Dependency is strictly one-way. The omnia crates are published crates.io
dependencies (currently 0.38.0) declared once under
`[workspace.dependencies]` in `Cargo.toml`, and every `omnia-*` crate moves
together. Until omnia publishes the bindings-based SDK,
`[patch.crates-io]` points every `omnia-*` at omnia's default branch, and
`cargo update -p omnia-sdk` re-locks them as one. Production backends
live in [omnia-backends](https://github.com/augentic/omnia-backends) and are
a host-side concern; nothing here talks to them.

## Crates

| Crate | Description |
| --- | --- |
| [`omnia-orm`](crates/orm) | `entity!` mapping and typed `SELECT` / `INSERT` / `UPDATE` / `DELETE` builders that render parameterized SQL for an `omnia:sql` database to execute. |

## Quick start

Tasks run through [mise](https://mise.jdx.dev/getting-started.html), which
must be installed by hand; `make <task>` is a pass-through to
`mise run <task>` and fails, rather than installing it, when it is missing.

Build the ORM example guest and run it with the example host. It is a
two-table CRUD service over the runtime's in-memory `SQLite`:

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

The suite is laid out as omnia's rungs (see omnia's
[Testing Omnia-Based Code](https://github.com/augentic/omnia/blob/main/docs/guides/testing-omnia-code.md)
guide) plus a build gate for the example host. Everything runs under
`mise run test`: nothing is `#[ignore]`d, and nothing needs installing
beyond `rust-toolchain.toml`, which carries the `wasm32-wasip3` target the
guests compile for.

### Component rung

`examples/tests/orm.rs` boots the ORM example guest — compiled to a
`wasm32-wasip3` component by `examples/build.rs` — through the example
host's own `runtime!` wiring over `omnia_test::host::Backends`, with the
bundle's private in-memory `SQLite`. One scenario, `agency_feed_lifecycle`,
chains the routes in-process: create an agency, list and update it, add a
feed, read the feed back through the JOIN entity carrying the updated agency
name, delete it, read an empty list, and see the second delete answered as
not found.

### Native tests

`crates/orm/tests/orm.rs` reaches no host, so it runs natively. It asserts
the SQL text and `$n` parameters each builder renders, and maps rows built
by hand through `Entity::from_row`. Scenarios cover a filtered, ordered,
limited select mapped back through `Entity::from_row` (including `NULL` →
`None` and RFC 3339 timestamps), insert from an entity, an `ON CONFLICT`
upsert, a filtered update and delete, a JOIN entity with aliased columns, a
missing column failing row mapping, and a limit too large for a portable
integer refusing to build.

### Examples gate

`examples/tests/examples.rs::build` runs `cargo build --locked --examples`
from the package root, so the example host compiles. The host in
`examples/orm/runtime.rs` is a server that never exits, so it is compiled,
not run; the component rung is where its wiring is exercised.

## Development

```shell
mise run ci     # fmt-check, clippy (native, then wasm32-wasip3), test, test-docs, docs, vet, deny
mise run check  # local advisories: audit, fmt (in place), lint, outdated, deps
```

`mise run ci` is the whole gate and runs exactly the jobs of the CI workflow:
every rung above, including the component rung and the examples gate, runs
inside `mise run test`. `mise tasks` lists everything else; the generic Rust
tasks are included from
[`augentic/toolkit`](https://github.com/augentic/toolkit/blob/main/mise/rust.toml)
by `mise.toml`, which adds only the example `build`/`run` tasks and a `wasm`
release build of the guests.

Clippy runs twice: natively with `--all-targets`, then for `wasm32-wasip3`
over libs and examples only (never tests or benches). The whole workspace is
in the wasm32 scope, so every runtime example stays `cfg_select!`-gated to an empty
`main` on wasm32 and every host-only integration test stays
`#![cfg(not(target_arch = "wasm32"))]`. Regenerate the supply-chain files with
`mise run vet-regen`; `vet` itself only checks.

The workspace follows omnia's conventions: stable toolchain
(`rust-toolchain.toml`, with the `wasm32-wasip3` target), edition 2024,
workspace lints, `cargo vet` supply-chain audits (`supply-chain/`), and CI as
thin wrappers over the reusable workflows in `augentic/toolkit`, pinned to a
release tag.

## License

MIT OR Apache-2.0
