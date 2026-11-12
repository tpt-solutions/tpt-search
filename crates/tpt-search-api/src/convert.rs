//! Conversion between JSON and documents.
//!
//! JSON values are read according to the field's declared type, so a plain number is
//! an integer or a float as the schema says, a string is a timestamp when the field is a
//! `DateTime`, and an array of numbers is a vector when the field is a `Vector`. Output
//! uses the plain JSON forms.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde_json::{Map, Number, Value};
use tpt_search_core::{Document, FieldType, FieldValue, Schema};

/// A rejected value: the message says which field and why.
pub type ConvertError = String;

/// Builds a document from a JSON object, checking every field against the schema.
pub fn document_from_json(
    schema: &Schema,
    id: &str,
    body: &Value,
) -> Result<Document, ConvertError> {
    let object = body
        .as_object()
        .ok_or_else(|| "document body must be a JSON object".to_string())?;
    let mut doc = Document::new(id);
    for (name, value) in object {
        let field = schema
            .field(name)
            .ok_or_else(|| format!("unknown field `{name}`"))?;
        let converted = value_from_json(&field.field_type, value)
            .map_err(|e| format!("field `{name}`: {e}"))?;
        doc.fields.insert(name.clone(), converted);
    }
    Ok(doc)
}

/// Converts one JSON value for a field of type `ty`.
pub fn value_from_json(ty: &FieldType, value: &Value) -> Result<FieldValue, ConvertError> {
    match (ty, value) {
        (_, Value::Null) => Ok(FieldValue::Null),
        (FieldType::Text, Value::String(s)) => Ok(FieldValue::Text(s.clone())),
        (FieldType::Integer, Value::Number(n)) => n
            .as_i64()
            .map(FieldValue::Integer)
            .ok_or_else(|| "expected an integer".to_string()),
        (FieldType::Float, Value::Number(n)) => n
            .as_f64()
            .map(FieldValue::Float)
            .ok_or_else(|| "expected a number".to_string()),
        (FieldType::Boolean, Value::Bool(b)) => Ok(FieldValue::Boolean(*b)),
        (FieldType::DateTime, Value::String(s)) => DateTime::parse_from_rfc3339(s)
            .map(|t| FieldValue::DateTime(t.with_timezone(&Utc)))
            .map_err(|e| format!("expected an RFC 3339 timestamp: {e}")),
        (FieldType::Vector(dim), Value::Array(items)) => {
            if items.len() != *dim as usize {
                return Err(format!("expected {dim} components, got {}", items.len()));
            }
            items
                .iter()
                .map(|v| {
                    v.as_f64()
                        .map(|f| f as f32)
                        .ok_or_else(|| "vector components must be numbers".to_string())
                })
                .collect::<Result<Vec<f32>, _>>()
                .map(FieldValue::Vector)
        }
        (FieldType::Array(inner), Value::Array(items)) => items
            .iter()
            .map(|v| value_from_json(inner, v))
            .collect::<Result<Vec<_>, _>>()
            .map(FieldValue::Array),
        (FieldType::Object(schema), Value::Object(map)) => {
            let mut fields = HashMap::new();
            for (name, v) in map {
                let field = schema
                    .field(name)
                    .ok_or_else(|| format!("unknown nested field `{name}`"))?;
                fields.insert(name.clone(), value_from_json(&field.field_type, v)?);
            }
            Ok(FieldValue::Object(fields))
        }
        (ty, _) => Err(format!("expected {}", describe(ty))),
    }
}

fn describe(ty: &FieldType) -> &'static str {
    match ty {
        FieldType::Text => "a string",
        FieldType::Integer => "an integer",
        FieldType::Float => "a number",
        FieldType::Boolean => "a boolean",
        FieldType::DateTime => "a timestamp string",
        FieldType::Vector(_) => "an array of numbers",
        FieldType::Array(_) => "an array",
        FieldType::Object(_) => "an object",
    }
}

/// Converts a field value to plain JSON.
pub fn value_to_json(value: &FieldValue) -> Value {
    match value {
        FieldValue::Text(s) => Value::String(s.clone()),
        FieldValue::Integer(i) => Value::Number((*i).into()),
        FieldValue::Float(f) => Number::from_f64(*f).map_or(Value::Null, Value::Number),
        FieldValue::Boolean(b) => Value::Bool(*b),
        FieldValue::DateTime(t) => Value::String(t.to_rfc3339()),
        FieldValue::Vector(v) => Value::Array(
            v.iter()
                .map(|x| Number::from_f64(f64::from(*x)).map_or(Value::Null, Value::Number))
                .collect(),
        ),
        FieldValue::Array(items) => Value::Array(items.iter().map(value_to_json).collect()),
        FieldValue::Object(fields) => {
            let mut map = Map::new();
            for (k, v) in fields {
                map.insert(k.clone(), value_to_json(v));
            }
            Value::Object(map)
        }
        FieldValue::Null => Value::Null,
    }
}

/// Converts a document to `{"id": ..., "fields": {...}}`.
pub fn document_to_json(doc: &Document) -> Value {
    let mut fields = Map::new();
    for (name, value) in &doc.fields {
        fields.insert(name.clone(), value_to_json(value));
    }
    let mut out = Map::new();
    out.insert("id".into(), Value::String(doc.id.as_str().to_owned()));
    out.insert("fields".into(), Value::Object(fields));
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tpt_search_core::FieldSchema;

    fn schema() -> Schema {
        Schema::new("s", "id")
            .with_field(FieldSchema::new("id", FieldType::Text))
            .unwrap()
            .with_field(FieldSchema::new("title", FieldType::Text))
            .unwrap()
            .with_field(FieldSchema::new("year", FieldType::Integer))
            .unwrap()
            .with_field(FieldSchema::new("score", FieldType::Float))
            .unwrap()
            .with_field(FieldSchema::new("at", FieldType::DateTime))
            .unwrap()
            .with_field(FieldSchema::new("emb", FieldType::Vector(2)))
            .unwrap()
    }

    #[test]
    fn values_follow_the_declared_type() {
        let doc = document_from_json(
            &schema(),
            "d1",
            &json!({"title": "hello", "year": 2024, "score": 1.5, "at": "2024-05-01T10:00:00Z", "emb": [1.0, 0.5]}),
        )
        .unwrap();
        assert_eq!(doc.get("year"), Some(&FieldValue::Integer(2024)));
        assert_eq!(doc.get("emb"), Some(&FieldValue::Vector(vec![1.0, 0.5])));
        assert!(matches!(doc.get("at"), Some(FieldValue::DateTime(_))));
    }

    #[test]
    fn rejects_unknown_fields_and_wrong_types() {
        assert!(document_from_json(&schema(), "d", &json!({"nope": 1})).is_err());
        assert!(document_from_json(&schema(), "d", &json!({"year": "2024"})).is_err());
        assert!(document_from_json(&schema(), "d", &json!({"emb": [1.0]})).is_err());
        assert!(document_from_json(&schema(), "d", &json!([1, 2])).is_err());
    }

    #[test]
    fn json_round_trips_through_documents() {
        let body = json!({"title": "t", "year": 7});
        let doc = document_from_json(&schema(), "d", &body).unwrap();
        let back = document_to_json(&doc);
        assert_eq!(back["id"], "d");
        assert_eq!(back["fields"]["year"], 7);
    }
}
