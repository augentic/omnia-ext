use anyhow::bail;
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use omnia_sdk::capability::{self, Error, ErrorKind};
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
    /// Fails with [`ErrorKind::Invalid`] if the column is missing or the value cannot be
    /// converted to the target type.
    fn fetch(row: &Row, col: &str) -> capability::Result<Self>;
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
    /// Fails with [`ErrorKind::Invalid`] if any required column is missing or cannot be
    /// converted to the expected type.
    fn from_row(row: &Row) -> capability::Result<Self>;
}

/// Internal trait for extracting entity values. Automatically implemented by the `entity!` macro.
#[doc(hidden)]
pub trait EntityValues {
    fn __to_values(&self) -> Vec<(&'static str, SeaValue)>;
}

pub fn to_wasi_params(values: Values) -> anyhow::Result<Vec<Value>> {
    values.into_iter().map(to_wasi_param).collect()
}

// `omnia:sql` marks its unsigned types non-portable, so unsigned values bind
// as `Int64`.
fn to_wasi_param(value: SeaValue) -> anyhow::Result<Value> {
    let value = match value {
        SeaValue::Bool(v) => Value::Boolean(v),
        SeaValue::TinyInt(v) => Value::Int32(v.map(i32::from)),
        SeaValue::SmallInt(v) => Value::Int32(v.map(i32::from)),
        SeaValue::Int(v) => Value::Int32(v),
        SeaValue::BigInt(v) => Value::Int64(v),
        SeaValue::TinyUnsigned(v) => Value::Int64(v.map(i64::from)),
        SeaValue::SmallUnsigned(v) => Value::Int64(v.map(i64::from)),
        SeaValue::Unsigned(v) => Value::Int64(v.map(i64::from)),
        SeaValue::BigUnsigned(v) => Value::Int64(v.map(signed).transpose()?),
        SeaValue::Float(v) => Value::Float(v),
        SeaValue::Double(v) => Value::Double(v),
        SeaValue::String(v) => Value::Str(v),
        SeaValue::ChronoDate(v) => Value::Date(v.map(|value| value.to_string())),
        SeaValue::ChronoTime(v) => Value::Time(v.map(|value| value.to_string())),
        SeaValue::ChronoDateTime(v) => Value::Timestamp(v.map(|value| value.to_string())),
        SeaValue::ChronoDateTimeUtc(v) => Value::Timestamp(v.map(|value| value.to_rfc3339())),
        SeaValue::Char(v) => Value::Str(v.map(|ch| ch.to_string())),
        SeaValue::Bytes(v) => Value::from(v),
        _ => {
            bail!("unsupported values require explicit conversion before building the query")
        }
    };
    Ok(value)
}

fn signed(value: u64) -> capability::Result<i64> {
    i64::try_from(value).map_err(|e| {
        let message = format!("{value} exceeds `i64::MAX`, the widest portable SQL integer");
        Error::new(ErrorKind::Invalid, message).with_source(e)
    })
}

impl FetchValue for DateTime<Utc> {
    fn fetch(row: &Row, col: &str) -> capability::Result<Self> {
        parse_timestamp(col, row.get(col)?)
    }
}

impl FetchValue for NaiveDate {
    fn fetch(row: &Row, col: &str) -> capability::Result<Self> {
        parse_date(col, row.get(col)?)
    }
}

impl FetchValue for serde_json::Value {
    fn fetch(row: &Row, col: &str) -> capability::Result<Self> {
        parse_json(col, row_field(row, col)?)
    }
}

impl<T: FetchValue> FetchValue for Option<T> {
    fn fetch(row: &Row, col: &str) -> capability::Result<Self> {
        // Only SQL NULL maps to `None`; a missing column is a projection bug
        // and propagates as an error rather than masquerading as NULL.
        if row_field(row, col)?.is_null() { Ok(None) } else { T::fetch(row, col).map(Some) }
    }
}

macro_rules! fetch {
    ($($ty:ty),* $(,)?) => {$(
        impl FetchValue for $ty {
            fn fetch(row: &Row, col: &str) -> capability::Result<Self> {
                Ok(row.get(col)?)
            }
        }
    )*};
}

fetch!(bool, i32, i64, u32, u64, f32, f64, String, Vec<u8>);

fn row_field<'a>(row: &'a Row, col: &str) -> capability::Result<&'a Value> {
    row.fields
        .iter()
        .find(|field| field.name == col)
        .map(|field| &field.value)
        .ok_or_else(|| Error::new(ErrorKind::Invalid, format!("row lacks column `{col}`")))
}

fn parse_timestamp(col: &str, raw: &str) -> capability::Result<DateTime<Utc>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(raw) {
        return Ok(parsed.with_timezone(&Utc));
    }

    if let Ok(parsed) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f") {
        return Ok(DateTime::<Utc>::from_naive_utc_and_offset(parsed, Utc));
    }

    let message = format!(
        "column `{col}` holds unsupported timestamp {raw}; expected RFC3339 or \"%Y-%m-%d %H:%M:%S%.f\" format"
    );
    Err(Error::new(ErrorKind::Invalid, message))
}

fn parse_date(col: &str, raw: &str) -> capability::Result<NaiveDate> {
    NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|e| {
        let message =
            format!("column `{col}` holds unsupported date {raw}; expected \"%Y-%m-%d\" format");
        Error::new(ErrorKind::Invalid, message).with_source(e)
    })
}

fn parse_json(col: &str, value: &Value) -> capability::Result<serde_json::Value> {
    let parsed = match value {
        Value::Str(Some(raw)) => serde_json::from_str(raw),
        Value::Binary(Some(bytes)) => serde_json::from_slice(bytes),
        other => {
            let message = format!("column `{col}` is not JSON text or bytes: {other:?}");
            return Err(Error::new(ErrorKind::Invalid, message));
        }
    };
    parsed.map_err(|e| {
        Error::new(ErrorKind::Invalid, format!("column `{col}` is not valid JSON")).with_source(e)
    })
}
