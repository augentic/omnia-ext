# ORM Example

Demonstrates `omnia-orm` over `omnia:sql` using the default (in-memory)
implementation: raw `wasi:sql` prepared statements for schema creation, then
one endpoint per query builder — `SelectBuilder`, `InsertBuilder`,
`UpdateBuilder`, `DeleteBuilder` — plus an `entity!` JOIN mapping across a
two-table agency/feed schema. Every query runs through the SDK's `SqlDb`
capability on the `default` database.

## Quick Start

```bash
make build orm
make run orm
```

Or, more manually, for debugging:

```bash
# build the guest
cargo build -p examples --example orm-wasm --target wasm32-wasip2

# run the host
export RUST_LOG="info,opentelemetry_sdk=off,omnia_wasi_sql=debug,omnia_wasi_http=debug,omnia_orm=debug,orm=debug"
cargo run -p examples --example orm -- run ./target/wasm32-wasip2/debug/examples/orm_wasm.wasm
```

## Test

```bash
# create an agency (InsertBuilder)
curl -X POST http://localhost:8080/agencies \
  -H 'Content-Type: application/json' \
  -d '{"agency_id":1,"name":"Ritchies Transport","url":"https://ritchies.co.nz","timezone":"Pacific/Auckland"}'

# list agencies (SelectBuilder)
curl http://localhost:8080/agencies

# update an agency (UpdateBuilder)
curl -X PATCH http://localhost:8080/agencies/1 \
  -H 'Content-Type: application/json' \
  -d '{"name":"Ritchies Transport Agency","timezone":"Pacific/Auckland"}'

# create a feed for the agency (InsertBuilder + existence check)
curl -X POST http://localhost:8080/agencies/1/feeds \
  -H 'Content-Type: application/json' \
  -d '{"feed_id":1,"description":"Bus routes and schedules"}'

# list all feeds with agency info (entity! JOIN)
curl http://localhost:8080/feeds

# delete a feed (DeleteBuilder)
curl -X DELETE http://localhost:8080/feeds/1
```

## Features Demonstrated

- **Prepared statements** — schema creation via `Statement::prepare` + `readwrite::exec`
- **ORM entities** — the `entity!` macro, including a JOIN entity with column aliasing
- **Query builders** — `SelectBuilder` (with `order_by_desc`, `limit`), `InsertBuilder`, `UpdateBuilder`, `DeleteBuilder`
- **Parameterized filters** — `Filter::eq` WHERE clauses (`$1`, `$2`, ... placeholders)
- **`SqlDb`** — the SDK capability that executes each built query on the `default` database

See the [`omnia-orm` README](../../crates/orm/README.md) for the full builder
and filter vocabulary.
