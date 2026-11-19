//! # ORM Wasm Guest
//!
//! Demonstrates `omnia-orm` over `omnia:sql`: raw `exec` statements for the
//! schema, then `SelectBuilder`, `InsertBuilder`, `UpdateBuilder`,
//! `DeleteBuilder`, and an `entity!` JOIN mapping, all run on the `default`
//! database with parameterized queries (`$1`, `$2`, ...).

#![cfg(target_arch = "wasm32")]

use anyhow::{Context as _, anyhow};
use axum::Json;
use axum::extract::Path;
use axum::routing::{delete, get, patch, post};
use chrono::Utc;
use omnia_orm::{
    DeleteBuilder, Entity, Filter, InsertBuilder, Join, Row, SelectBuilder, UpdateBuilder, entity,
};
use omnia_sdk::api::{Handlers, Router};
use omnia_sdk::sql::database::Database;
use omnia_sdk::sql::{DEFAULT, store};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

omnia_sdk::api::http::export!(router);

fn router() -> Router {
    Handlers::new()
        .route("/agencies", get(list_agencies).post(create_agency))
        .route("/agencies/{id}", patch(update_agency))
        .route("/agencies/{id}/feeds", post(create_feed))
        .route("/feeds", get(list_all_feeds))
        .route("/feeds/{id}", delete(delete_feed))
        .into_router()
}

/// List all agencies (`SelectBuilder`).
#[omnia_sdk::otel::instrument]
async fn list_agencies() -> omnia_sdk::Result<Json<Value>> {
    let database = database().await?;

    let select = SelectBuilder::<Agency>::new()
        .order_by_desc(None, "created_at")
        .build()
        .context("building query")?;

    let rows =
        database.query_as::<Row>(&select.sql, &select.params).await.context("executing query")?;

    let agencies = rows
        .iter()
        .map(Agency::from_row)
        .collect::<omnia_sdk::Result<Vec<_>>>()
        .context("mapping rows")?;

    Ok(Json(json!({ "agencies": agencies })))
}

/// Create an agency with a client-supplied id (`InsertBuilder`).
#[omnia_sdk::otel::instrument]
async fn create_agency(Json(req): Json<CreateAgencyRequest>) -> omnia_sdk::Result<Json<Value>> {
    let database = database().await?;

    let agency = Agency {
        agency_id: req.agency_id,
        name: req.name,
        url: req.url,
        timezone: req.timezone,
        created_at: Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    };

    let query = InsertBuilder::<Agency>::from_entity(&agency).build().context("building insert")?;

    database.exec(&query.sql, &query.params).await.context("inserting agency")?;

    Ok(Json(json!({ "agency": agency })))
}

/// Update an agency's mutable fields (`UpdateBuilder`).
#[omnia_sdk::otel::instrument]
async fn update_agency(
    Path(id): Path<i64>, Json(req): Json<UpdateAgencyRequest>,
) -> omnia_sdk::Result<Json<Value>> {
    let database = database().await?;

    let query = UpdateBuilder::<Agency>::new()
        .set("name", req.name)
        .set("timezone", req.timezone)
        .r#where(Filter::eq("agency_id", id))
        .build()
        .context("building update")?;

    let rows_affected =
        database.exec(&query.sql, &query.params).await.context("updating agency")?;

    if rows_affected == 0 {
        return Err(anyhow!("agency not found").into());
    }

    Ok(Json(json!({ "message": "agency updated", "agency_id": id })))
}

/// Create a feed for an existing agency (`InsertBuilder` + existence check).
#[omnia_sdk::otel::instrument]
async fn create_feed(
    Path(agency_id): Path<i64>, Json(req): Json<CreateFeedRequest>,
) -> omnia_sdk::Result<Json<Value>> {
    let database = database().await?;

    let select = SelectBuilder::<Agency>::new()
        .r#where(Filter::eq("agency_id", agency_id))
        .limit(1)
        .build()
        .context("building agency lookup")?;

    let rows = database
        .query_as::<Row>(&select.sql, &select.params)
        .await
        .context("executing agency lookup")?;

    if rows.is_empty() {
        return Err(anyhow!("agency not found").into());
    }

    let feed = Feed {
        feed_id: req.feed_id,
        agency_id,
        description: req.description,
        created_at: Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
    };

    let query = InsertBuilder::<Feed>::from_entity(&feed).build().context("building insert")?;

    database.exec(&query.sql, &query.params).await.context("inserting feed")?;

    Ok(Json(json!({ "feed": feed })))
}

/// List all feeds with their agency information (`entity!` JOIN).
#[omnia_sdk::otel::instrument]
async fn list_all_feeds() -> omnia_sdk::Result<Json<Value>> {
    let database = database().await?;

    let select = SelectBuilder::<FeedWithAgency>::new()
        .order_by_desc(Some("feed"), "created_at")
        .limit(100)
        .build()
        .context("building join query")?;

    let rows = database
        .query_as::<Row>(&select.sql, &select.params)
        .await
        .context("executing join query")?;

    let feeds = rows
        .iter()
        .map(FeedWithAgency::from_row)
        .collect::<omnia_sdk::Result<Vec<_>>>()
        .context("mapping rows")?;

    Ok(Json(json!({ "feeds": feeds })))
}

/// Delete a feed (`DeleteBuilder`).
#[omnia_sdk::otel::instrument]
async fn delete_feed(Path(id): Path<i64>) -> omnia_sdk::Result<Json<Value>> {
    let database = database().await?;

    let query = DeleteBuilder::<Feed>::new()
        .r#where(Filter::eq("feed_id", id))
        .build()
        .context("building delete")?;

    let rows_affected = database.exec(&query.sql, &query.params).await.context("deleting feed")?;

    if rows_affected == 0 {
        return Err(anyhow!("feed not found").into());
    }

    Ok(Json(json!({ "message": "feed deleted", "feed_id": id })))
}

/// Open the `default` database and create the schema with raw statements.
/// Each request is handled by a fresh guest instance, so this runs per
/// request — fine for an example.
async fn database() -> anyhow::Result<Database> {
    let database = store::open(DEFAULT.to_owned()).await.context("opening the database")?;

    let create_agency = "CREATE TABLE IF NOT EXISTS agency (
        agency_id BIGINT PRIMARY KEY,
        name TEXT NOT NULL,
        url TEXT,
        timezone TEXT,
        created_at TEXT NOT NULL
    )";

    database.exec(create_agency, &[]).await.context("creating agency table")?;

    let create_feed = "CREATE TABLE IF NOT EXISTS feed (
        feed_id BIGINT PRIMARY KEY,
        agency_id BIGINT NOT NULL,
        description TEXT NOT NULL,
        created_at TEXT NOT NULL
    )";

    database.exec(create_feed, &[]).await.context("creating feed table")?;

    Ok(database)
}

// Entity definitions

entity!(
    table = "agency",
    #[derive(Debug, Clone, Serialize)]
    pub struct Agency {
        pub agency_id: i64,
        pub name: String,
        pub url: Option<String>,
        pub timezone: Option<String>,
        pub created_at: String,
    }
);

entity!(
    table = "feed",
    #[derive(Debug, Clone, Serialize)]
    pub struct Feed {
        pub feed_id: i64,
        pub agency_id: i64,
        pub description: String,
        pub created_at: String,
    }
);

// JOIN entity: `columns` names fields sourced from the joined agency table;
// fields not listed are auto-qualified with the main table (feed).
entity!(
    table = "feed",
    columns = [("agency", "name", "agency_name"),],
    joins = [Join::left("agency", Filter::col_eq("feed", "agency_id", "agency", "agency_id")),],
    #[derive(Debug, Clone, Serialize)]
    pub struct FeedWithAgency {
        pub feed_id: i64,
        pub agency_id: i64,
        pub description: String,
        pub created_at: String,
        pub agency_name: String,
    }
);

// Request types

#[derive(Debug, Deserialize)]
struct CreateAgencyRequest {
    agency_id: i64,
    name: String,
    url: Option<String>,
    timezone: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateAgencyRequest {
    name: String,
    timezone: String,
}

#[derive(Debug, Deserialize)]
struct CreateFeedRequest {
    feed_id: i64,
    description: String,
}
