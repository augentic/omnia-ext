//! Tests for the query builders driven exactly as a guest uses them: each
//! built `Query` is executed through `omnia_test::guest::Provider`, whose
//! `ScriptedTables` records the SQL text and `$n` parameters that reach
//! `SqlDb` and answers with scripted rows for `Entity::from_row`.

// Host-only: the test doubles are `not(wasm32)` dev-dependencies.
#![cfg(not(target_arch = "wasm32"))]
// `entity!` only accepts `pub` fields, and the fixtures are not an API.
#![expect(missing_docs, reason = "test fixtures are not a public API")]

use chrono::{DateTime, Utc};
use omnia_orm::{
    DeleteBuilder, Entity, Field, Filter, InsertBuilder, Join, Row, SelectBuilder, UpdateBuilder,
    Value, entity,
};
use omnia_sdk::SqlDb as _;
use omnia_sdk::capability::{self, ErrorKind};
use omnia_test::guest::{Executed, Provider, ScriptedTables};

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

fn row(fields: Vec<(&str, Value)>) -> Row {
    Row {
        fields: fields
            .into_iter()
            .map(|(name, value)| Field {
                name: name.to_owned(),
                value,
            })
            .collect(),
    }
}

/// The one statement a scenario issued.
fn only_statement(provider: &Provider) -> Executed {
    let statements = provider.sql.statements();
    assert_eq!(statements.len(), 1, "statements: {statements:?}");
    statements.into_iter().next().expect("one statement")
}

#[tokio::test]
async fn filtered_select() {
    let stamp = "2024-01-15T10:30:45+00:00";
    let scripted = ScriptedTables::default().on_query(
        |sql, params| sql.starts_with("SELECT") && params.len() == 2,
        vec![row(vec![
            ("agency_id", Value::from(7_i64)),
            ("name", Value::from("Metro")),
            ("url", Value::Str(None)),
            ("created_at", Value::Timestamp(Some(stamp.to_owned()))),
        ])],
    );
    let provider = Provider::default().sql(scripted);

    let query = SelectBuilder::<Agency>::new()
        .r#where(Filter::eq("agency_id", 7_i64))
        .order_by_desc(None, "created_at")
        .limit(10)
        .build()
        .expect("select builds");
    let rows = provider.query(&query.sql, &query.params).await.expect("scripted query");

    // The unsigned limit binds as `Int64`, the portable integer.
    let statement = only_statement(&provider);
    assert_eq!(statement.binding, "default");
    assert_eq!(
        statement.sql,
        r#"SELECT "agency"."agency_id", "agency"."name", "agency"."url", "agency"."created_at" FROM "agency" WHERE ("agency"."agency_id") = ($1) ORDER BY "agency"."created_at" DESC LIMIT $2"#
    );
    assert_eq!(statement.params, [Value::from(7_i64), Value::from(10_i64)]);

    // SQL `NULL` maps to `None`; the RFC 3339 timestamp parses to `DateTime<Utc>`.
    let agencies: Vec<Agency> =
        rows.iter().map(Agency::from_row).collect::<capability::Result<_>>().expect("rows map");
    assert_eq!(agencies.len(), 1);
    assert_eq!(agencies[0].agency_id, 7);
    assert_eq!(agencies[0].name, "Metro");
    assert_eq!(agencies[0].url, None);
    assert_eq!(agencies[0].created_at, stamp.parse::<DateTime<Utc>>().expect("rfc3339"));
}

#[tokio::test]
async fn insert_from_entity() {
    let provider = Provider::default()
        .sql(ScriptedTables::default().on_exec(|sql, _| sql.starts_with("INSERT"), 1));
    let agency = Agency {
        agency_id: 7,
        name: "Metro".to_owned(),
        url: None,
        created_at: "2024-01-15T10:30:45Z".parse().expect("rfc3339"),
    };

    let query = InsertBuilder::<Agency>::from_entity(&agency).build().expect("insert builds");
    let affected = provider.execute(&query.sql, &query.params).await.expect("scripted execute");
    assert_eq!(affected, 1);

    // Every field travels, in declaration order; `None` is a typed NULL and
    // the timestamp is bound as RFC 3339.
    let statement = only_statement(&provider);
    assert_eq!(
        statement.sql,
        r#"INSERT INTO "agency" ("agency_id", "name", "url", "created_at") VALUES ($1, $2, $3, $4)"#
    );
    assert_eq!(
        statement.params,
        [
            Value::from(7_i64),
            Value::from("Metro"),
            Value::Str(None),
            Value::Timestamp(Some("2024-01-15T10:30:45+00:00".to_owned())),
        ]
    );
}

#[tokio::test]
async fn upsert_on_conflict() {
    let provider = Provider::default()
        .sql(ScriptedTables::default().on_exec(|sql, _| sql.starts_with("INSERT"), 1));

    let query = InsertBuilder::<Feed>::new()
        .set("feed_id", 1_i64)
        .set("agency_id", 7_i64)
        .set("description", "Bus routes")
        .on_conflict("feed_id")
        .do_update_all()
        .build()
        .expect("upsert builds");
    provider.execute(&query.sql, &query.params).await.expect("scripted execute");

    let statement = only_statement(&provider);
    assert_eq!(
        statement.sql,
        r#"INSERT INTO "feed" ("feed_id", "agency_id", "description") VALUES ($1, $2, $3) ON CONFLICT ("feed_id") DO UPDATE SET "agency_id" = "excluded"."agency_id", "description" = "excluded"."description""#
    );
    assert_eq!(
        statement.params,
        [Value::from(1_i64), Value::from(7_i64), Value::from("Bus routes")]
    );
}

#[tokio::test]
async fn filtered_update() {
    let provider = Provider::default()
        .sql(ScriptedTables::default().on_exec(|sql, _| sql.starts_with("UPDATE"), 0));

    let query = UpdateBuilder::<Agency>::new()
        .set("name", "Metro Transit")
        .set("url", Some("https://metro.test".to_owned()))
        .r#where(Filter::eq("agency_id", 7_i64))
        .build()
        .expect("update builds");
    let affected = provider.execute(&query.sql, &query.params).await.expect("scripted execute");

    // The affected-row count is the caller's not-found signal.
    assert_eq!(affected, 0);
    let statement = only_statement(&provider);
    assert_eq!(
        statement.sql,
        r#"UPDATE "agency" SET "name" = $1, "url" = $2 WHERE ("agency"."agency_id") = ($3)"#
    );
    assert_eq!(
        statement.params,
        [Value::from("Metro Transit"), Value::from("https://metro.test"), Value::from(7_i64)]
    );
}

#[tokio::test]
async fn filtered_delete() {
    let provider = Provider::default()
        .sql(ScriptedTables::default().on_exec(|sql, _| sql.starts_with("DELETE"), 1));

    let query = DeleteBuilder::<Feed>::new()
        .r#where(Filter::and([Filter::eq("feed_id", 1_i64), Filter::eq("agency_id", 7_i64)]))
        .build()
        .expect("delete builds");
    let affected = provider.execute(&query.sql, &query.params).await.expect("scripted execute");
    assert_eq!(affected, 1);

    let statement = only_statement(&provider);
    assert_eq!(
        statement.sql,
        r#"DELETE FROM "feed" WHERE (("feed"."feed_id") = ($1)) AND (("feed"."agency_id") = ($2))"#
    );
    assert_eq!(statement.params, [Value::from(1_i64), Value::from(7_i64)]);
}

#[tokio::test]
async fn aliased_join() {
    let scripted = ScriptedTables::default().on_query(
        |sql, _| sql.contains("LEFT JOIN"),
        vec![row(vec![
            ("feed_id", Value::from(1_i64)),
            ("description", Value::from("Bus routes")),
            ("agency_name", Value::from("Metro")),
        ])],
    );
    let provider = Provider::default().sql(scripted);

    let query = SelectBuilder::<FeedWithAgency>::new()
        .order_by_desc(Some("feed"), "created_at")
        .limit(100)
        .build()
        .expect("join select builds");
    let rows = provider.query(&query.sql, &query.params).await.expect("scripted query");

    // The entity's default join and the aliased projection come from
    // `entity!`; the caller only adds ordering and a limit.
    let statement = only_statement(&provider);
    assert_eq!(
        statement.sql,
        r#"SELECT "feed"."feed_id", "feed"."description", "agency"."name" AS "agency_name" FROM "feed" LEFT JOIN "agency" ON ("feed"."agency_id") = ("agency"."agency_id") ORDER BY "feed"."created_at" DESC LIMIT $1"#
    );
    assert_eq!(statement.params, [Value::from(100_i64)]);

    // The row maps by the alias, so the joined column lands in its field.
    let feeds: Vec<FeedWithAgency> = rows
        .iter()
        .map(FeedWithAgency::from_row)
        .collect::<capability::Result<_>>()
        .expect("rows map");
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
            ("agency_id", Value::from(7_i64)),
            ("name", Value::from("Metro")),
            ("created_at", Value::Timestamp(Some("2024-01-15 10:30:45".to_owned()))),
        ])],
    );
    let provider = Provider::default().sql(scripted);

    let query = SelectBuilder::<Agency>::new().build().expect("select builds");
    let rows = provider.query(&query.sql, &query.params).await.expect("scripted query");

    let err = Agency::from_row(&rows[0]).expect_err("missing column");
    assert_eq!(err.kind(), ErrorKind::Invalid);
    assert!(err.to_string().contains("row lacks column `url`"), "{err}");

    assert!(
        UpdateBuilder::<Agency>::new().r#where(Filter::eq("agency_id", 7_i64)).build().is_err()
    );
    assert!(UpdateBuilder::<Agency>::new().set("name", "x").build().is_err());
    assert!(DeleteBuilder::<Feed>::new().build().is_err());
}

#[test]
fn oversized_limit() {
    // No portable SQL integer holds a `u64` past `i64::MAX`, so the limit
    // refuses to build rather than wrap.
    let Err(err) = SelectBuilder::<Agency>::new().limit(u64::MAX).build() else {
        panic!("expected an oversized limit to be refused");
    };
    let err = err.downcast_ref::<capability::Error>().expect("capability error");
    assert_eq!(err.kind(), ErrorKind::Invalid);
}
