//! Deliberately small, closed JSON contracts. Unsupported schema constructs fail.
use crate::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum Schema {
    Object {
        properties: BTreeMap<String, Schema>,
        required: BTreeSet<String>,
        #[serde(rename = "additionalProperties")]
        additional: bool,
    },
    Array {
        items: Box<Schema>,
        #[serde(rename = "maxItems")]
        max_items: usize,
    },
    String {
        #[serde(rename = "maxLength")]
        max_length: usize,
        #[serde(rename = "enum", default, skip_serializing_if = "BTreeSet::is_empty")]
        values: BTreeSet<String>,
    },
    Integer {
        minimum: i64,
        maximum: i64,
    },
    Number {
        minimum: f64,
        maximum: f64,
    },
    Boolean,
    Null,
}
impl Schema {
    pub fn validate_definition(&self) -> AppResult<()> {
        fn walk(s: &Schema, depth: usize, n: &mut usize) -> AppResult<()> {
            *n += 1;
            if depth > 8 || *n > 256 {
                return Err(AppError::invalid("contract_structure_limit"));
            }
            match s {
                Schema::Object {
                    properties,
                    required,
                    additional,
                } => {
                    if *additional
                        || properties.len() > 64
                        || !required.is_subset(&properties.keys().cloned().collect())
                        || properties.keys().any(|k| {
                            k.is_empty() || k.len() > 64 || k.chars().any(char::is_control)
                        })
                    {
                        return Err(AppError::invalid("contract_object_invalid"));
                    }
                    for s in properties.values() {
                        walk(s, depth + 1, n)?;
                    }
                }
                Schema::Array { items, max_items } => {
                    if !(1..=1000).contains(max_items) {
                        return Err(AppError::invalid("contract_array_invalid"));
                    }
                    walk(items, depth + 1, n)?;
                }
                Schema::String { max_length, values } => {
                    if !(1..=65536).contains(max_length)
                        || values.len() > 100
                        || values.iter().any(|v| v.len() > *max_length)
                    {
                        return Err(AppError::invalid("contract_string_invalid"));
                    }
                }
                Schema::Integer { minimum, maximum } => {
                    if minimum > maximum {
                        return Err(AppError::invalid("contract_range_invalid"));
                    }
                }
                Schema::Number { minimum, maximum }
                    if (!minimum.is_finite() || !maximum.is_finite() || minimum > maximum) =>
                {
                    return Err(AppError::invalid("contract_range_invalid"));
                }
                _ => {}
            }
            Ok(())
        }
        walk(self, 0, &mut 0)
    }
    pub fn validate(&self, value: &Value) -> AppResult<()> {
        self.validate_definition()?;
        crate::governance::validate_shape(value)?;
        fn walk(s: &Schema, v: &Value) -> bool {
            match (s, v) {
                (
                    Schema::Object {
                        properties,
                        required,
                        ..
                    },
                    Value::Object(m),
                ) => {
                    required.iter().all(|k| m.contains_key(k))
                        && m.iter()
                            .all(|(k, v)| properties.get(k).is_some_and(|s| walk(s, v)))
                }
                (Schema::Array { items, max_items }, Value::Array(a)) => {
                    a.len() <= *max_items && a.iter().all(|v| walk(items, v))
                }
                (Schema::String { max_length, values }, Value::String(s)) => {
                    s.chars().count() <= *max_length
                        && !s.contains('\0')
                        && (values.is_empty() || values.contains(s))
                }
                (Schema::Integer { minimum, maximum }, v) => {
                    v.as_i64().is_some_and(|n| n >= *minimum && n <= *maximum)
                }
                (Schema::Number { minimum, maximum }, v) => v
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && n >= *minimum && n <= *maximum),
                (Schema::Boolean, Value::Bool(_)) | (Schema::Null, Value::Null) => true,
                _ => false,
            }
        }
        if walk(self, value) {
            Ok(())
        } else {
            Err(AppError::invalid("contract_value_invalid"))
        }
    }
}
