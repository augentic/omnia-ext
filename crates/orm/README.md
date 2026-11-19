# omnia-orm

A lightweight, backend-agnostic object-relational mapper for Omnia guests
using `omnia:sql`. The `entity!` macro maps a struct to a table and typed
builders produce parameterized `SELECT` / `INSERT` / `UPDATE` / `DELETE`
statements as `{ sql, params }` pairs; executing them is left to the
caller's `omnia:sql` database. SQL generation is delegated to
[`sea-query`](https://crates.io/crates/sea-query) and the crate re-exports
`Row`, `Field` and `Value` from `omnia_sdk::sql` so row mapping needs no
extra dependency.

## Defining entities

`entity!` generates the struct plus an `Entity` implementation carrying the
table name, projection, and a `from_row` constructor:

```rust
use omnia_orm::entity;

entity!(
    table = "agency",
    #[derive(Debug, Clone)]
    pub struct Agency {
        pub agency_id: i64,
        pub name: String,
        pub url: Option<String>,
        pub timezone: Option<String>,
        pub created_at: String,
    }
);
```

The struct is otherwise ordinary — derive whatever you need. Every field type
must implement `FetchValue` (for `from_row`) and `Clone + Into<sea_query::Value>`
(for `InsertBuilder::from_entity`, which clones each field out of `&self`
before converting it). All of the types below satisfy this:

| Rust type | `Value` read | `Value` written |
| --- | --- | --- |
| `bool` | `Boolean`, or an integer `0` or `1` | `Boolean` |
| `i32`, `i64` | any integer case the value fits | `Int32`, `Int64` |
| `u32`, `u64` | any integer case the value fits | `Int64` |
| `f32`, `f64` | `Float`; `Float` or `Double` | `Float`, `Double` |
| `String` | `Str`, `Date`, `Time` or `Timestamp` | `Str` |
| `Vec<u8>` | `Binary` | `Binary` |
| `chrono::NaiveDate` | any text case (`%Y-%m-%d`) | `Date` |
| `chrono::DateTime<Utc>` | any text case (RFC 3339 or `%Y-%m-%d %H:%M:%S%.f`) | `Timestamp` (RFC 3339) |
| `serde_json::Value` | `Str` or `Binary` holding JSON | — |
| `Option<T>` | SQL `NULL` → `None`, otherwise as `T` | as `T`, `None` → `Null` |

`omnia:sql` has no unsigned integer, so unsigned values bind as `Int64`,
and a `u64` past `i64::MAX` fails to build. `from_row` fails
with the code `invalid` on a missing column, a mismatched value, or text
that does not parse. A missing column is a projection bug and fails even
for an `Option<T>` field; only SQL `NULL` maps to `None`.

## Queries with the builders

Each builder ends in `build()`, returning a `Query { sql, params }` with
`$1`, `$2`, … placeholders and the matching `Vec<Value>`.

Select with filtering, ordering, and limits:

```rust
# use omnia_orm::entity;
# entity!(table = "agency", #[derive(Debug, Clone)] pub struct Agency { pub agency_id: i64, pub name: String });
use omnia_orm::{Filter, SelectBuilder};

let select = SelectBuilder::<Agency>::new()
    .r#where(Filter::eq("agency_id", 7_i64))
    .order_by_desc(None, "created_at")
    .limit(100)
    .build()?;

assert_eq!(select.params.len(), 2); // $1 = agency_id, $2 = limit
# Ok::<(), anyhow::Error>(())
```

Insert from an entity value:

```rust
# use omnia_orm::entity;
# entity!(table = "agency", #[derive(Debug, Clone)] pub struct Agency { pub agency_id: i64, pub name: String });
use omnia_orm::InsertBuilder;

let agency = Agency { agency_id: 7, name: "Metro".into() };
let insert = InsertBuilder::<Agency>::from_entity(&agency).build()?;

assert_eq!(insert.sql, r#"INSERT INTO "agency" ("agency_id", "name") VALUES ($1, $2)"#);
assert_eq!(insert.params.len(), 2);
# Ok::<(), anyhow::Error>(())
```

Update only the fields that changed, guarded by a filter (an `UPDATE` or
`DELETE` without a `.r#where(..)` clause refuses to build):

```rust
# use omnia_orm::entity;
# entity!(table = "agency", #[derive(Debug, Clone)] pub struct Agency { pub agency_id: i64, pub name: String, pub url: Option<String> });
use omnia_orm::{Filter, UpdateBuilder};

let (name, url): (Option<String>, Option<String>) = (Some("Metro".into()), None);
let mut update = UpdateBuilder::<Agency>::new();
if let Some(name) = name {
    update = update.set("name", name);
}
if let Some(url) = url {
    update = update.set("url", url);
}
let update = update.r#where(Filter::eq("agency_id", 7_i64)).build()?;

assert_eq!(update.params.len(), 2); // $1 = name, $2 = agency_id
# Ok::<(), anyhow::Error>(())
```

Delete, checking the affected-row count for a not-found result:

```rust
# use omnia_orm::entity;
# entity!(table = "feed", #[derive(Debug, Clone)] pub struct Feed { pub feed_id: i64 });
use omnia_orm::{DeleteBuilder, Filter};

let delete = DeleteBuilder::<Feed>::new().r#where(Filter::eq("feed_id", 7_i64)).build()?;

assert_eq!(delete.params.len(), 1); // $1 = feed_id
# Ok::<(), anyhow::Error>(())
```

`Filter` also offers `ne`, `gt`/`gte`/`lt`/`lte`, `r#in`/`not_in`,
`is_null`/`is_not_null`, `like`/`not_like`, `between`/`not_between`, and the
combinators `and`/`or`; `in_table` qualifies a filter's column with a table
when a join makes the name ambiguous.

## Joins

An entity can span a `JOIN`. Fields not listed in `columns` resolve against
the main table; listed ones pull from the joined table under an alias:

```rust
use omnia_orm::{Filter, Join, entity};

entity!(
    table = "feed",
    columns = [
        ("agency", "name", "agency_name"),
        ("agency", "url", "agency_url"),
    ],
    joins = [Join::left("agency", Filter::col_eq("feed", "agency_id", "agency", "agency_id"))],
    #[derive(Debug, Clone)]
    pub struct FeedWithAgency {
        pub feed_id: i64,               // feed.feed_id
        pub agency_id: i64,             // feed.agency_id
        pub description: String,        // feed.description
        pub agency_name: String,        // agency.name AS agency_name
        pub agency_url: Option<String>, // agency.url AS agency_url
    }
);
```

Selecting `FeedWithAgency` then works exactly like a single-table entity —
`order_by_desc(Some("feed"), "created_at")` qualifies the table when the
column name is ambiguous, and `SelectBuilder::join` adds joins beyond the
entity's defaults.

## Usage

Builders know nothing about databases. Hand `query.sql` and `query.params`
to a database opened with `omnia_sdk::sql::open`.
`Database::query_as` returns rows to map with `Entity::from_row`, and
`Database::exec` returns the affected-row count:

```rust,ignore
use omnia_orm::{Entity, Filter, Row, SelectBuilder};
use omnia_sdk::{Result, sql};

let database = sql::open(sql::DEFAULT).await?;
let query = SelectBuilder::<Agency>::new()
    .r#where(Filter::eq("agency_id", id))
    .build()?;
let rows = database.query_as::<Row>(&query.sql, &query.params).await?;
let agencies = rows.iter().map(Agency::from_row).collect::<Result<Vec<_>>>()?;
```

`sql::DEFAULT` names the deployment's `default` database, and `sql::open`
opens any other the deployment declares. The builders and `from_row` need
no host, so the crate's tests assert each rendered `Query` and map rows
built by hand.
