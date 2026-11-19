//! Tests for the query builders driven exactly as a guest uses them: each
//! scenario builds a `Query` and asserts the SQL text and `$n` parameters a
//! guest hands to `omnia:sql`, and maps rows shaped as `omnia:sql` returns
//! them through `Entity::from_row`.

// `entity!` only accepts `pub` fields, and the fixtures are not an API.
#![expect(missing_docs, reason = "test fixtures are not a public API")]

use chrono::{DateTime, Utc};
use omnia_orm::{
    DeleteBuilder, Entity, Field, Filter, InsertBuilder, Join, Row, SelectBuilder, UpdateBuilder,
    Value, entity,
};

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

#[test]
fn filtered_select() {
    let query = SelectBuilder::<Agency>::new()
        .r#where(Filter::eq("agency_id", 7_i64))
        .order_by_desc(None, "created_at")
        .limit(10)
        .build()
        .expect("select builds");

    // The unsigned limit binds as `Int64`, the portable integer.
    assert_eq!(
        query.sql,
        r#"SELECT "agency"."agency_id", "agency"."name", "agency"."url", "agency"."created_at" FROM "agency" WHERE ("agency"."agency_id") = ($1) ORDER BY "agency"."created_at" DESC LIMIT $2"#
    );
    assert_eq!(query.params, [Value::from(7_i64), Value::from(10_i64)]);

    // SQL `NULL` maps to `None`; the RFC 3339 timestamp parses to `DateTime<Utc>`.
    let stamp = "2024-01-15T10:30:45+00:00";
    let agency = Agency::from_row(&row(vec![
        ("agency_id", Value::from(7_i64)),
        ("name", Value::from("Metro")),
        ("url", Value::Null),
        ("created_at", Value::Timestamp(stamp.to_owned())),
    ]))
    .expect("the row maps");
    assert_eq!(agency.agency_id, 7);
    assert_eq!(agency.name, "Metro");
    assert_eq!(agency.url, None);
    assert_eq!(agency.created_at, stamp.parse::<DateTime<Utc>>().expect("rfc3339"));
}

#[test]
fn insert_from_entity() {
    let agency = Agency {
        agency_id: 7,
        name: "Metro".to_owned(),
        url: None,
        created_at: "2024-01-15T10:30:45Z".parse().expect("rfc3339"),
    };

    let query = InsertBuilder::<Agency>::from_entity(&agency).build().expect("insert builds");

    // Every field travels, in declaration order; `None` binds as `NULL` and
    // the timestamp is bound as RFC 3339.
    assert_eq!(
        query.sql,
        r#"INSERT INTO "agency" ("agency_id", "name", "url", "created_at") VALUES ($1, $2, $3, $4)"#
    );
    assert_eq!(
        query.params,
        [
            Value::from(7_i64),
            Value::from("Metro"),
            Value::Null,
            Value::Timestamp("2024-01-15T10:30:45+00:00".to_owned()),
        ]
    );
}

#[test]
fn upsert_on_conflict() {
    let query = InsertBuilder::<Feed>::new()
        .set("feed_id", 1_i64)
        .set("agency_id", 7_i64)
        .set("description", "Bus routes")
        .on_conflict("feed_id")
        .do_update_all()
        .build()
        .expect("upsert builds");

    assert_eq!(
        query.sql,
        r#"INSERT INTO "feed" ("feed_id", "agency_id", "description") VALUES ($1, $2, $3) ON CONFLICT ("feed_id") DO UPDATE SET "agency_id" = "excluded"."agency_id", "description" = "excluded"."description""#
    );
    assert_eq!(query.params, [Value::from(1_i64), Value::from(7_i64), Value::from("Bus routes")]);
}

#[test]
fn filtered_update() {
    let query = UpdateBuilder::<Agency>::new()
        .set("name", "Metro Transit")
        .set("url", Some("https://metro.test".to_owned()))
        .r#where(Filter::eq("agency_id", 7_i64))
        .build()
        .expect("update builds");

    assert_eq!(
        query.sql,
        r#"UPDATE "agency" SET "name" = $1, "url" = $2 WHERE ("agency"."agency_id") = ($3)"#
    );
    assert_eq!(
        query.params,
        [Value::from("Metro Transit"), Value::from("https://metro.test"), Value::from(7_i64)]
    );
}

#[test]
fn filtered_delete() {
    let query = DeleteBuilder::<Feed>::new()
        .r#where(Filter::and([Filter::eq("feed_id", 1_i64), Filter::eq("agency_id", 7_i64)]))
        .build()
        .expect("delete builds");

    assert_eq!(
        query.sql,
        r#"DELETE FROM "feed" WHERE (("feed"."feed_id") = ($1)) AND (("feed"."agency_id") = ($2))"#
    );
    assert_eq!(query.params, [Value::from(1_i64), Value::from(7_i64)]);
}

#[test]
fn aliased_join() {
    let query = SelectBuilder::<FeedWithAgency>::new()
        .order_by_desc(Some("feed"), "created_at")
        .limit(100)
        .build()
        .expect("join select builds");

    // The entity's default join and the aliased projection come from
    // `entity!`; the caller only adds ordering and a limit.
    assert_eq!(
        query.sql,
        r#"SELECT "feed"."feed_id", "feed"."description", "agency"."name" AS "agency_name" FROM "feed" LEFT JOIN "agency" ON ("feed"."agency_id") = ("agency"."agency_id") ORDER BY "feed"."created_at" DESC LIMIT $1"#
    );
    assert_eq!(query.params, [Value::from(100_i64)]);

    // The row maps by the alias, so the joined column lands in its field.
    let feed = FeedWithAgency::from_row(&row(vec![
        ("feed_id", Value::from(1_i64)),
        ("description", Value::from("Bus routes")),
        ("agency_name", Value::from("Metro")),
    ]))
    .expect("the row maps");
    assert_eq!(feed.feed_id, 1);
    assert_eq!(feed.description, "Bus routes");
    assert_eq!(feed.agency_name, "Metro");
}

#[test]
fn missing_column_or_clause() {
    // A projection bug, not a NULL: the row lacks `url` entirely, so even
    // the `Option<String>` field refuses to map.
    let err = Agency::from_row(&row(vec![
        ("agency_id", Value::from(7_i64)),
        ("name", Value::from("Metro")),
        ("created_at", Value::Timestamp("2024-01-15 10:30:45".to_owned())),
    ]))
    .expect_err("missing column");
    assert_eq!(err.code(), "invalid");
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
    let err = err.downcast_ref::<omnia_sdk::Error>().expect("an omnia_sdk error");
    assert_eq!(err.code(), "invalid");
}
