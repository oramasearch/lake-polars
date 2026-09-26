// Copyright 2026 OramaSearch.
// Licensed under the Apache License, Version 2.0.

//! How a Polars value becomes JSON in a query answer.
//!
//! | Polars | JSON |
//! |---|---|
//! | null | `null` |
//! | Boolean | `true` / `false` |
//! | Int8–Int64, UInt8–UInt64 | a JSON integer (exact to 64 bits; JavaScript loses precision above 2^53) |
//! | Int128, UInt128, Decimal | a string, so no digit is lost |
//! | Float16–Float64 | a JSON number; NaN and infinities as the strings `"NaN"`, `"Infinity"`, `"-Infinity"` |
//! | String, Categorical, Enum | a string |
//! | Date | `"2026-09-26"` |
//! | Datetime | ISO 8601 `"2026-09-26T10:11:12.345"`, with the offset when the column has a time zone |
//! | Time | `"10:11:12"` (fractional seconds when there are any) |
//! | Duration, List, Array, Struct | Polars' own text form, as a string |
//! | Binary | `{"$base64": "..."}` |

use polars::prelude::*;
use serde_json::{json, Number, Value};

/// Converts every row of `frame` (at most `limit` of them) to JSON arrays.
pub fn rows(frame: &DataFrame, limit: usize) -> Vec<Vec<Value>> {
    let height = frame.height().min(limit);
    let columns = frame.columns();
    (0..height)
        .map(|i| {
            columns
                .iter()
                .map(|column| match column.get(i) {
                    Ok(value) => to_json(&value),
                    Err(_) => Value::Null,
                })
                .collect()
        })
        .collect()
}

fn float(v: f64) -> Value {
    if v.is_nan() {
        Value::String("NaN".into())
    } else if v.is_infinite() {
        Value::String(if v > 0.0 { "Infinity" } else { "-Infinity" }.into())
    } else {
        Number::from_f64(v)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

/// Converts one value (see the module table).
pub fn to_json(value: &AnyValue) -> Value {
    match value {
        AnyValue::Null => Value::Null,
        AnyValue::Boolean(v) => Value::Bool(*v),
        AnyValue::Int8(v) => json!(v),
        AnyValue::Int16(v) => json!(v),
        AnyValue::Int32(v) => json!(v),
        AnyValue::Int64(v) => json!(v),
        AnyValue::UInt8(v) => json!(v),
        AnyValue::UInt16(v) => json!(v),
        AnyValue::UInt32(v) => json!(v),
        AnyValue::UInt64(v) => json!(v),
        AnyValue::Int128(v) => Value::String(v.to_string()),
        AnyValue::UInt128(v) => Value::String(v.to_string()),
        AnyValue::Float16(v) => float(f64::from(f32::from(*v))),
        AnyValue::Float32(v) => float(f64::from(*v)),
        AnyValue::Float64(v) => float(*v),
        AnyValue::String(v) => Value::String((*v).to_string()),
        AnyValue::StringOwned(v) => Value::String(v.to_string()),
        AnyValue::Binary(v) => base64(v),
        AnyValue::BinaryOwned(v) => base64(v),
        AnyValue::Datetime(..) | AnyValue::DatetimeOwned(..) => {
            // Polars writes "2026-09-26 10:11:12.345"; ISO 8601 wants the T.
            Value::String(value.to_string().replacen(' ', "T", 1))
        }
        other => {
            if let Some(text) = other.get_str() {
                return Value::String(text.to_string());
            }
            let text = other.to_string();
            Value::String(text.trim_matches('"').to_string())
        }
    }
}

fn base64(bytes: &[u8]) -> Value {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> shift) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    json!({ "$base64": out })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(base64(b""), json!({"$base64": ""}));
        assert_eq!(base64(b"f"), json!({"$base64": "Zg=="}));
        assert_eq!(base64(b"fo"), json!({"$base64": "Zm8="}));
        assert_eq!(base64(b"foo"), json!({"$base64": "Zm9v"}));
        assert_eq!(base64(b"foobar"), json!({"$base64": "Zm9vYmFy"}));
    }

    #[test]
    fn scalars() {
        assert_eq!(to_json(&AnyValue::Int64(-7)), json!(-7));
        assert_eq!(to_json(&AnyValue::UInt64(u64::MAX)), json!(u64::MAX));
        assert_eq!(to_json(&AnyValue::Float64(2.5)), json!(2.5));
        assert_eq!(to_json(&AnyValue::Float64(f64::NAN)), json!("NaN"));
        assert_eq!(
            to_json(&AnyValue::Float64(f64::NEG_INFINITY)),
            json!("-Infinity")
        );
        assert_eq!(to_json(&AnyValue::Boolean(true)), json!(true));
        assert_eq!(to_json(&AnyValue::String("x")), json!("x"));
        assert_eq!(to_json(&AnyValue::Null), Value::Null);
        assert_eq!(
            to_json(&AnyValue::Int128(1 << 100)),
            json!((1i128 << 100).to_string())
        );
        // 2026-09-26 is day 20722 since the epoch.
        assert_eq!(to_json(&AnyValue::Date(20722)), json!("2026-09-26"));
    }
}
