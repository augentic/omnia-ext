//! Tests for the query builders driven exactly as a guest uses them: each
//! built `Query` is executed through `omnia_test::guest::Provider`, whose
//! `ScriptedTables` records the SQL text and `$n` parameters that reach
//! `TableStore` and answers with scripted rows for `Entity::from_row`.

// Host-only: the test doubles are `not(wasm32)` dev-dependencies.
#![cfg(not(target_arch = "wasm32"))]
// `entity!` only accepts `pub` fields, and the fixtures are not an API.
#![expect(missing_docs, reason = "test fixtures are not a public API")]

use std::fmt::Debug;

use chrono::{DateTime, Utc};
use omnia_orm::{
    DataType, DeleteBuilder, Entity, Field, Filter, InsertBuilder, Join, Row, SelectBuilder,
    UpdateBuilder, entity,
};
use omnia_sdk::TableStore as _;
use omnia_test::guest::{Provider, ScriptedTables, Statement};

const CONNECTION: &str = "db";

entity!(
    table = "agency",
    #[derive(Debug, Clone)]
    pub struct Agency {
        pub agency_id: i64,
        pub name: String,
        pub url: Option<String>,
        pub created_at: DateTime<Utc>,
    }
);

entity!(
    table = "feed",
    #[derive(Debug, Clone)]
    pub struct Feed {
        pub feed_id: i64,
        pub agency_id: i64,
        pub description: String,
    }
);

// Fields named in `columns` are sourced from the joined table under an
// alias; the rest resolve against `feed`.
entity!(
    table = "feed",
    columns = [("agency", "name", "agency_name")],
    joins = [Join::left("agency", Filter::col_eq("feed", "agency_id", "agency", "agency_id"))],
    #[derive(Debug, Clone)]
    pub struct FeedWithAgency {
        pub feed_id: i64,
        pub description: String,
        pub agency_name: String,
    }
);

fn row(fields: Vec<(&str, DataType)>) -> Row {
    Row {
        index: "0".to_owned(),
        fields: fields
            .into_iter()
            .map(|(name, value)| Field {
                name: name.to_owned(),
                value,
            })
            .collect(),
    }
}

fn str(value: &str) -> DataType {
    DataType::Str(Some(value.to_owned()))
}

/// The one statement a scenario issued.
fn only_statement(provider: &Provider) -> Statement {
    let statements = provider.tables.statements();
    assert_eq!(statements.len(), 1, "statements: {statements:?}");
    statements.into_iter().next().expect("one statement")
}

/// The bound parameters as `Kind(value)` / `Kind(NULL)`; the WIT-generated
/// enum carries no `PartialEq`, so this is the comparable shape.
fn params(statement: &Statement) -> Vec<String> {
    fn render<T: Debug>(kind: &str, value: Option<&T>) -> String {
        value.map_or_else(|| format!("{kind}(NULL)"), |v| format!("{kind}({v:?})"))
    }

    statement
        .params
        .iter()
        .map(|param| match param {
            DataType::Int32(v) => render("Int32", v.as_ref()),
            DataType::Int64(v) => render("Int64", v.as_ref()),
            DataType::Uint32(v) => render("Uint32", v.as_ref()),
            DataType::Uint64(v) => render("Uint64", v.as_ref()),
            DataType::Float(v) => render("Float", v.as_ref()),
            DataType::Double(v) => render("Double", v.as_ref()),
            DataType::Str(v) => render("Str", v.as_ref()),
            DataType::Boolean(v) => render("Boolean", v.as_ref()),
            DataType::Date(v) => render("Date", v.as_ref()),
            DataType::Time(v) => render("Time", v.as_ref()),
            DataType::Timestamp(v) => render("Timestamp", v.as_ref()),
            DataType::Binary(v) => render("Binary", v.as_ref()),
        })
        .collect()
}

#[tokio::test]
async fn filtered_select() {
    let stamp = "2024-01-15T10:30:45+00:00";
    let scripted = ScriptedTables::default().on_query(
        |sql, params| sql.starts_with("SELECT") && params.len() == 2,
        vec![row(vec![
            ("agency_id", DataType::Int64(Some(7))),
            ("name", str("Metro")),
            ("url", DataType::Str(None)),
            ("created_at", DataType::Timestamp(Some(stamp.to_owned()))),
        ])],
    );
    let provider = Provider::default().tables(scripted);

    let query = SelectBuilder::<Agency>::new()
        .r#where(Filter::eq("agency_id", 7_i64))
        .order_by_desc(None, "created_at")
        .limit(10)
        .build()
        .expect("select builds");
    let rows = provider.query(CONNECTION, &query.sql, &query.params).await.expect("scripted query");

    let statement = only_statement(&provider);
    assert_eq!(statement.connection, CONNECTION);
    assert_eq!(
        statement.sql,
        r#"SELECT "agency"."agency_id", "agency"."name", "agency"."url", "agency"."created_at" FROM "agency" WHERE ("agency"."agency_id") = ($1) ORDER BY "agency"."created_at" DESC LIMIT $2"#
    );
    assert_eq!(params(&statement), ["Int64(7)", "Uint64(10)"]);

    // SQL `NULL` maps to `None`; the RFC 3339 timestamp parses to `DateTime<Utc>`.
    let agencies: Vec<Agency> =
        rows.iter().map(Agency::from_row).collect::<anyhow::Result<_>>().expect("rows map");
    assert_eq!(agencies.len(), 1);
    assert_eq!(agencies[0].agency_id, 7);
    assert_eq!(agencies[0].name, "Metro");
    assert_eq!(agencies[0].url, None);
    assert_eq!(agencies[0].created_at, stamp.parse::<DateTime<Utc>>().expect("rfc3339"));
}

#[tokio::test]
async fn insert_from_entity() {
    let provider = Provider::default()
        .tables(ScriptedTables::default().on_exec(|sql, _| sql.starts_with("INSERT"), 1));
    let agency = Agency {
        agency_id: 7,
        name: "Metro".to_owned(),
        url: None,
        created_at: "2024-01-15T10:30:45Z".parse().expect("rfc3339"),
    };

    let query = InsertBuilder::<Agency>::from_entity(&agency).build().expect("insert builds");
    let affected =
        provider.exec(CONNECTION, &query.sql, &query.params).await.expect("scripted exec");
    assert_eq!(affected, 1);

    // Every field travels, in declaration order; `None` is a typed NULL and
    // the timestamp is bound as RFC 3339.
    let statement = only_statement(&provider);
    assert_eq!(
        statement.sql,
        r#"INSERT INTO "agency" ("agency_id", "name", "url", "created_at") VALUES ($1, $2, $3, $4)"#
    );
    assert_eq!(
        params(&statement),
        ["Int64(7)", r#"Str("Metro")"#, "Str(NULL)", r#"Timestamp("2024-01-15T10:30:45+00:00")"#]
    );
}

#[tokio::test]
async fn upsert_on_conflict() {
    let provider = Provider::default()
        .tables(ScriptedTables::default().on_exec(|sql, _| sql.starts_with("INSERT"), 1));

    let query = InsertBuilder::<Feed>::new()
        .set("feed_id", 1_i64)
        .set("agency_id", 7_i64)
        .set("description", "Bus routes")
        .on_conflict("feed_id")
        .do_update_all()
        .build()
        .expect("upsert builds");
    provider.exec(CONNECTION, &query.sql, &query.params).await.expect("scripted exec");

    let statement = only_statement(&provider);
    assert_eq!(
        statement.sql,
        r#"INSERT INTO "feed" ("feed_id", "agency_id", "description") VALUES ($1, $2, $3) ON CONFLICT ("feed_id") DO UPDATE SET "agency_id" = "excluded"."agency_id", "description" = "excluded"."description""#
    );
    assert_eq!(params(&statement), ["Int64(1)", "Int64(7)", r#"Str("Bus routes")"#]);
}

#[tokio::test]
async fn filtered_update() {
    let provider = Provider::default()
        .tables(ScriptedTables::default().on_exec(|sql, _| sql.starts_with("UPDATE"), 0));

    let query = UpdateBuilder::<Agency>::new()
        .set("name", "Metro Transit")
        .set("url", Some("https://metro.test".to_owned()))
        .r#where(Filter::eq("agency_id", 7_i64))
        .build()
        .expect("update builds");
    let affected =
        provider.exec(CONNECTION, &query.sql, &query.params).await.expect("scripted exec");

    // The affected-row count is the caller's not-found signal.
    assert_eq!(affected, 0);
    let statement = only_statement(&provider);
    assert_eq!(
        statement.sql,
        r#"UPDATE "agency" SET "name" = $1, "url" = $2 WHERE ("agency"."agency_id") = ($3)"#
    );
    assert_eq!(
        params(&statement),
        [r#"Str("Metro Transit")"#, r#"Str("https://metro.test")"#, "Int64(7)"]
    );
}

#[tokio::test]
async fn filtered_delete() {
    let provider = Provider::default()
        .tables(ScriptedTables::default().on_exec(|sql, _| sql.starts_with("DELETE"), 1));

    let query = DeleteBuilder::<Feed>::new()
        .r#where(Filter::and([Filter::eq("feed_id", 1_i64), Filter::eq("agency_id", 7_i64)]))
        .build()
        .expect("delete builds");
    let affected =
        provider.exec(CONNECTION, &query.sql, &query.params).await.expect("scripted exec");
    assert_eq!(affected, 1);

    let statement = only_statement(&provider);
    assert_eq!(
        statement.sql,
        r#"DELETE FROM "feed" WHERE (("feed"."feed_id") = ($1)) AND (("feed"."agency_id") = ($2))"#
    );
    assert_eq!(params(&statement), ["Int64(1)", "Int64(7)"]);
}

#[tokio::test]
async fn aliased_join() {
    let scripted = ScriptedTables::default().on_query(
        |sql, _| sql.contains("LEFT JOIN"),
        vec![row(vec![
            ("feed_id", DataType::Int64(Some(1))),
            ("description", str("Bus routes")),
            ("agency_name", str("Metro")),
        ])],
    );
    let provider = Provider::default().tables(scripted);

    let query = SelectBuilder::<FeedWithAgency>::new()
        .order_by_desc(Some("feed"), "created_at")
        .limit(100)
        .build()
        .expect("join select builds");
    let rows = provider.query(CONNECTION, &query.sql, &query.params).await.expect("scripted query");

    // The entity's default join and the aliased projection come from
    // `entity!`; the caller only adds ordering and a limit.
    let statement = only_statement(&provider);
    assert_eq!(
        statement.sql,
        r#"SELECT "feed"."feed_id", "feed"."description", "agency"."name" AS "agency_name" FROM "feed" LEFT JOIN "agency" ON ("feed"."agency_id") = ("agency"."agency_id") ORDER BY "feed"."created_at" DESC LIMIT $1"#
    );
    assert_eq!(params(&statement), ["Uint64(100)"]);

    // The row maps by the alias, so the joined column lands in its field.
    let feeds: Vec<FeedWithAgency> =
        rows.iter().map(FeedWithAgency::from_row).collect::<anyhow::Result<_>>().expect("rows map");
    assert_eq!(feeds.len(), 1);
    assert_eq!(feeds[0].feed_id, 1);
    assert_eq!(feeds[0].description, "Bus routes");
    assert_eq!(feeds[0].agency_name, "Metro");
}

#[tokio::test]
async fn missing_column_or_clause() {
    // A projection bug, not a NULL: the row lacks `url` entirely, so even
    // the `Option<String>` field refuses to map.
    let scripted = ScriptedTables::default().on_query(
        |_, _| true,
        vec![row(vec![
            ("agency_id", DataType::Int64(Some(7))),
            ("name", str("Metro")),
            ("created_at", DataType::Timestamp(Some("2024-01-15 10:30:45".to_owned()))),
        ])],
    );
    let provider = Provider::default().tables(scripted);

    let query = SelectBuilder::<Agency>::new().build().expect("select builds");
    let rows = provider.query(CONNECTION, &query.sql, &query.params).await.expect("scripted query");

    let err = Agency::from_row(&rows[0]).expect_err("missing column");
    assert!(err.to_string().contains("missing column 'url'"), "{err}");

    assert!(
        UpdateBuilder::<Agency>::new().r#where(Filter::eq("agency_id", 7_i64)).build().is_err()
    );
    assert!(UpdateBuilder::<Agency>::new().set("name", "x").build().is_err());
    assert!(DeleteBuilder::<Feed>::new().build().is_err());
}
