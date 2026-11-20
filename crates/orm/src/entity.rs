use anyhow::bail;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use omnia_sdk::{Error, Result};
use sea_query::{Value as SeaValue, Values};

use super::join::Join;
use super::{Row, Value};

/// Trait for types that can be extracted from database rows.
///
/// This trait is implemented for all standard Rust types that can be
/// fetched from a database row (`i32`, `String`, `DateTime`, etc.).
pub trait FetchValue: Sized {
    /// Fetch a value from a row by column name.
    ///
    /// # Errors
    ///
    /// A [`ServerError`](Error::ServerError) with the code `invalid` if the
    /// column is missing or the value cannot be converted to the target type.
    fn fetch(row: &Row, col: &str) -> Result<Self>;
}

/// Trait for database entities with metadata for query building.
///
/// Typically implemented via the `entity!` macro rather than manually.
pub trait Entity: Sized {
    /// The database table name for this entity.
    const TABLE: &'static str;

    /// Column names to select when fetching this entity.
    fn projection() -> &'static [&'static str];

    /// Default joins to include when querying this entity.
    #[must_use]
    fn joins() -> Vec<Join> {
        Vec::new()
    }

    /// Column specifications for fields from joined tables.
    /// Returns tuples of (``struct_field``, ``source_table``, ``source_column``).
    /// Fields not listed here will be auto-qualified with the main table.
    #[must_use]
    fn column_specs() -> Vec<(&'static str, &'static str, &'static str)> {
        Vec::new()
    }

    /// Construct an entity instance from a database row.
    ///
    /// # Errors
    ///
    /// A [`ServerError`](Error::ServerError) with the code `invalid` if any
    /// required column is missing or cannot be converted to the expected type.
    fn from_row(row: &Row) -> Result<Self>;
}

/// Internal trait for extracting entity values. Automatically implemented by the `entity!` macro.
#[doc(hidden)]
pub trait EntityValues {
    fn __to_values(&self) -> Vec<(&'static str, SeaValue)>;
}

pub fn to_wasi_params(values: Values) -> anyhow::Result<Vec<Value>> {
    values.into_iter().map(to_wasi_param).collect()
}

// `omnia:sql` has no unsigned integer, so unsigned values bind as `Int64`.
fn to_wasi_param(value: SeaValue) -> anyhow::Result<Value> {
    let value = match value {
        SeaValue::Bool(v) => Value::from(v),
        SeaValue::TinyInt(v) => Value::from(v.map(i32::from)),
        SeaValue::SmallInt(v) => Value::from(v.map(i32::from)),
        SeaValue::Int(v) => Value::from(v),
        SeaValue::BigInt(v) => Value::from(v),
        SeaValue::TinyUnsigned(v) => Value::from(v.map(i64::from)),
        SeaValue::SmallUnsigned(v) => Value::from(v.map(i64::from)),
        SeaValue::Unsigned(v) => Value::from(v.map(i64::from)),
        SeaValue::BigUnsigned(v) => Value::from(v.map(signed).transpose()?),
        SeaValue::Float(v) => Value::from(v),
        SeaValue::Double(v) => Value::from(v),
        SeaValue::String(v) => Value::from(v),
        SeaValue::ChronoDate(v) => v.map_or(Value::Null, |value| Value::Date(value.to_string())),
        SeaValue::ChronoTime(v) => v.map_or(Value::Null, |value| Value::Time(value.to_string())),
        SeaValue::ChronoDateTime(v) => {
            v.map_or(Value::Null, |value| Value::Timestamp(value.to_string()))
        }
        SeaValue::ChronoDateTimeUtc(v) => {
            v.map_or(Value::Null, |value| Value::TimestampTz(value.to_rfc3339()))
        }
        SeaValue::Char(v) => Value::from(v.map(String::from)),
        SeaValue::Bytes(v) => Value::from(v),
        _ => {
            bail!("unsupported values require explicit conversion before building the query")
        }
    };
    Ok(value)
}

fn signed(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|e| {
        let message = format!("{value} exceeds `i64::MAX`, the widest portable SQL integer: {e}");
        Error::server_error("invalid", message)
    })
}

impl FetchValue for DateTime<Utc> {
    fn fetch(row: &Row, col: &str) -> Result<Self> {
        parse_timestamp(col, row.get(col)?)
    }
}

impl FetchValue for NaiveDate {
    fn fetch(row: &Row, col: &str) -> Result<Self> {
        parse_date(col, row.get(col)?)
    }
}

impl FetchValue for serde_json::Value {
    fn fetch(row: &Row, col: &str) -> Result<Self> {
        parse_json(col, row_field(row, col)?)
    }
}

impl<T: FetchValue> FetchValue for Option<T> {
    fn fetch(row: &Row, col: &str) -> Result<Self> {
        // Only SQL NULL maps to `None`; a missing column is a projection bug
        // and propagates as an error rather than masquerading as NULL.
        if row_field(row, col)?.is_null() { Ok(None) } else { T::fetch(row, col).map(Some) }
    }
}

macro_rules! fetch {
    ($($ty:ty),* $(,)?) => {$(
        impl FetchValue for $ty {
            fn fetch(row: &Row, col: &str) -> Result<Self> {
                row.get(col)
            }
        }
    )*};
}

fetch!(bool, i32, i64, u32, u64, f32, f64, String, Vec<u8>);

fn row_field<'a>(row: &'a Row, col: &str) -> Result<&'a Value> {
    row.fields
        .iter()
        .find(|field| field.name == col)
        .map(|field| &field.value)
        .ok_or_else(|| Error::server_error("invalid", format!("row lacks column `{col}`")))
}

fn parse_timestamp(col: &str, raw: &str) -> Result<DateTime<Utc>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(raw) {
        return Ok(parsed.with_timezone(&Utc));
    }

    if let Ok(parsed) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f") {
        return Ok(DateTime::<Utc>::from_naive_utc_and_offset(parsed, Utc));
    }

    let message = format!(
        "column `{col}` holds unsupported timestamp {raw}; expected RFC3339 or \"%Y-%m-%d %H:%M:%S%.f\" format"
    );
    Err(Error::server_error("invalid", message))
}

fn parse_date(col: &str, raw: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|e| {
        let message = format!(
            "column `{col}` holds unsupported date {raw}; expected \"%Y-%m-%d\" format: {e}"
        );
        Error::server_error("invalid", message)
    })
}

fn parse_json(col: &str, value: &Value) -> Result<serde_json::Value> {
    let parsed = match value {
        Value::Text(raw) => serde_json::from_str(raw),
        Value::Binary(bytes) => serde_json::from_slice(bytes),
        other => {
            let message = format!("column `{col}` is not JSON text or bytes: {other:?}");
            return Err(Error::server_error("invalid", message));
        }
    };
    parsed.map_err(|e| {
        Error::server_error("invalid", format!("column `{col}` is not valid JSON: {e}"))
    })
}
