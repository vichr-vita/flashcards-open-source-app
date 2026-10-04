//! Predicate evaluation over the published row projection, with no database SQL interpolation.
use super::dialect::{self, Object, captures, field, invalid, split};
use crate::error::ApiError;
use chrono::{SecondsFormat, Utc};
use icu_collator::{Collator, CollatorBorrowed, options::CollatorOptions};
use icu_locale::locale;
use regex::{Regex, RegexBuilder};
use serde_json::{Value, json};
use std::{cmp::Ordering, sync::OnceLock};

pub(super) fn collator() -> Result<&'static CollatorBorrowed<'static>, ApiError> {
    static ENGLISH: OnceLock<Option<CollatorBorrowed<'static>>> = OnceLock::new();
    ENGLISH
        .get_or_init(|| Collator::try_new(locale!("en-US").into(), CollatorOptions::default()).ok())
        .as_ref()
        .ok_or_else(ApiError::internal)
}

#[derive(Debug)]
pub(super) enum Predicate {
    And(Vec<Self>),
    Or(Vec<Self>),
    Match(String),
    Null(String, bool),
    Compare(String, String, Value),
    Like(String, String, bool, bool),
    In(String, Vec<Value>, bool, bool),
    EqualsArray(String, Vec<String>),
    Overlap(String, Vec<String>),
}

pub(super) fn like(pattern: &str, insensitive: bool) -> Result<Regex, ApiError> {
    let mut expression = String::from("^");
    for ch in pattern.chars() {
        match ch {
            '%' => expression.push_str(".*"),
            '_' => expression.push('.'),
            _ => expression.push_str(&regex::escape(&ch.to_string())),
        }
    }
    expression.push('$');
    RegexBuilder::new(&expression)
        .case_insensitive(insensitive)
        .build()
        .map_err(|_| invalid("Invalid LIKE pattern"))
}

pub(super) fn compare(a: &Value, b: &Value, collator: &CollatorBorrowed<'_>) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    if a.is_null() {
        return Ordering::Less;
    }
    if b.is_null() {
        return Ordering::Greater;
    }
    if let (Some(a), Some(b)) = (a.as_f64(), b.as_f64()) {
        return a.total_cmp(&b);
    }
    if let (Some(a), Some(b)) = (a.as_bool(), b.as_bool()) {
        return a.cmp(&b);
    }
    collator.compare(&text(a), &text(b))
}
fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(text).collect::<Vec<_>>().join("\0"),
        _ => value.to_string(),
    }
}
fn scalar_equal(a: &Value, b: &Value) -> bool {
    if a.is_array() || a.is_object() {
        false
    } else if let (Some(a), Some(b)) = (a.as_f64(), b.as_f64()) {
        a.total_cmp(&b) == Ordering::Equal
    } else {
        a == b
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "Keep the accepted predicate operators adjacent for contract review."
)]
pub(super) fn parse(
    resource: &str,
    alias: Option<&str>,
    input: &str,
    depth: u8,
) -> Result<Predicate, ApiError> {
    if depth > 16 {
        return Err(invalid("WHERE nesting exceeds 16 levels"));
    }
    let input = input.trim_matches(crate::core::cards::js_space);
    if input.is_empty() {
        return Err(invalid("WHERE must not be empty"));
    }
    for (separator, or) in [("OR", true), ("AND", false)] {
        let parts = split(input, separator, true)?;
        if parts.len() > 1 {
            let predicates = parts
                .iter()
                .map(|part| {
                    parse(
                        resource,
                        alias,
                        part,
                        depth.checked_add(1).ok_or_else(ApiError::internal)?,
                    )
                })
                .collect::<Result<_, _>>()?;
            return Ok(if or {
                Predicate::Or(predicates)
            } else {
                Predicate::And(predicates)
            });
        }
    }
    if input.starts_with('(') && input.ends_with(')') {
        let inside = input
            .get(1..input.len().checked_sub(1).ok_or_else(ApiError::internal)?)
            .ok_or_else(ApiError::internal)?;
        if dialect::positions(inside, "\0", false).is_ok() {
            return parse(
                resource,
                alias,
                inside,
                depth.checked_add(1).ok_or_else(ApiError::internal)?,
            );
        }
    }
    if let Some(groups) = captures(r"(?is)^MATCH\s*\(\s*('(?:''|[^'])*')\s*\)$", input)? {
        let query = dialect::string(field(&groups, 0))?;
        if query.trim_matches(crate::core::cards::js_space).is_empty() {
            return Err(invalid("MATCH must not be empty"));
        }
        return Ok(Predicate::Match(query.to_lowercase()));
    }
    let groups=captures(r"(?is)^(?:LOWER\s*\(\s*([a-z_][a-z0-9_]*)\s*\)|([a-z_][a-z0-9_]*))\s*(IS NOT NULL|IS NULL|NOT ILIKE|NOT LIKE|ILIKE|LIKE|NOT IN|IN|OVERLAP|<=|>=|=|<|>)\s*(.*)$",input)?.ok_or_else(||invalid(format!("Unsupported WHERE predicate: {input}")))?;
    let lowered = !field(&groups, 0).is_empty();
    let name = if lowered {
        field(&groups, 0)
    } else {
        field(&groups, 1)
    }
    .to_lowercase();
    let descriptor = dialect::column(resource, alias, &name)?;
    if descriptor.get("filterable") != Some(&json!(true)) {
        return Err(invalid(format!("Column is not filterable: {name}")));
    }
    let operator = field(&groups, 2).to_ascii_uppercase();
    let rhs = field(&groups, 3).trim_matches(crate::core::cards::js_space);
    match operator.as_str() {
        "IS NULL" | "IS NOT NULL" => {
            if lowered || !rhs.is_empty() {
                return Err(invalid("Unsupported NULL predicate"));
            }
            Ok(Predicate::Null(name, operator == "IS NOT NULL"))
        }
        "LIKE" | "NOT LIKE" | "ILIKE" | "NOT ILIKE" => {
            if !matches!(
                descriptor.get("type").and_then(Value::as_str),
                Some("string" | "uuid" | "datetime")
            ) {
                return Err(invalid("LIKE requires a text column"));
            }
            let pattern = dialect::string(rhs)?;
            like(&pattern, lowered || operator.contains("ILIKE"))?;
            Ok(Predicate::Like(
                name,
                pattern,
                lowered || operator.contains("ILIKE"),
                operator.starts_with("NOT"),
            ))
        }
        "IN" | "NOT IN" => {
            if operator == "NOT IN" && !lowered {
                return Err(invalid("NOT IN is supported only with LOWER(column)"));
            }
            let inside = rhs
                .strip_prefix('(')
                .and_then(|part| part.strip_suffix(')'))
                .ok_or_else(|| invalid("IN requires parenthesized scalar literals"))?;
            let values = split(inside, ",", false)?
                .iter()
                .map(|part| dialect::literal(part))
                .collect::<Result<Vec<_>, _>>()?;
            if lowered && values.iter().any(|value| !value.is_string()) {
                return Err(invalid("LOWER(column) IN requires string literals"));
            }
            Ok(Predicate::In(name, values, lowered, operator == "NOT IN"))
        }
        "OVERLAP" => {
            if lowered {
                return Err(invalid("LOWER does not support OVERLAP"));
            }
            Ok(Predicate::Overlap(name, dialect::string_array(rhs)?))
        }
        "=" if descriptor.get("type").and_then(Value::as_str) == Some("string[]") && !lowered => {
            Ok(Predicate::EqualsArray(name, dialect::string_array(rhs)?))
        }
        "=" if lowered => {
            let pattern = dialect::string(rhs)?;
            Ok(Predicate::Like(name, pattern, true, false))
        }
        _ => {
            if lowered {
                return Err(invalid("Unsupported LOWER comparison"));
            }
            let value = if captures(r"(?i)^NOW\s*\(\s*\)$", rhs)?.is_some() {
                json!(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true))
            } else {
                dialect::literal(rhs)?
            };
            Ok(Predicate::Compare(name, operator, value))
        }
    }
}

impl Predicate {
    pub fn matches(&self, row: &Object, collator: &CollatorBorrowed<'_>) -> bool {
        let get = |name: &str| row.get(name).unwrap_or(&Value::Null);
        match self {
            Self::And(operands) => operands
                .iter()
                .all(|predicate| predicate.matches(row, collator)),
            Self::Or(operands) => operands
                .iter()
                .any(|predicate| predicate.matches(row, collator)),
            Self::Match(query) => row.values().any(|value| match value {
                Value::Null => false,
                Value::Array(items) => items
                    .iter()
                    .map(text)
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase()
                    .contains(query),
                _ => text(value).to_lowercase().contains(query),
            }),
            Self::Null(name, not) => get(name).is_null() != *not,
            Self::Compare(name, operator, value) => {
                let left = get(name);
                if operator == "=" {
                    return scalar_equal(left, value);
                }
                if left.is_null() || left.is_array() || left.is_object() || value.is_null() {
                    return false;
                }
                let order = compare(left, value, collator);
                match operator.as_str() {
                    "<" => order == Ordering::Less,
                    "<=" => order != Ordering::Greater,
                    ">" => order == Ordering::Greater,
                    ">=" => order != Ordering::Less,
                    _ => false,
                }
            }
            Self::Like(name, pattern, insensitive, not) => {
                let left = get(name);
                if left.is_null() || left.is_array() || left.is_object() {
                    return false;
                }
                like(pattern, *insensitive)
                    .is_ok_and(|expression| expression.is_match(&text(left)) != *not)
            }
            Self::In(name, values, lowered, not) => {
                let left = get(name);
                if left.is_array() || left.is_object() {
                    return false;
                }
                let matched = values.iter().any(|value| {
                    if *lowered {
                        left.as_str()
                            .zip(value.as_str())
                            .is_some_and(|(a, b)| a.to_lowercase() == b.to_lowercase())
                    } else {
                        scalar_equal(left, value)
                    }
                });
                matched != *not
            }
            Self::Overlap(name, values) => get(name).as_array().is_some_and(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|item| values.iter().any(|candidate| candidate == item))
            }),
            Self::EqualsArray(name, values) => {
                let Some(items) = get(name).as_array() else {
                    return false;
                };
                let mut items = items.iter().filter_map(Value::as_str).collect::<Vec<_>>();
                let mut expected = values.iter().map(String::as_str).collect::<Vec<_>>();
                items.sort_unstable_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
                expected.sort_unstable_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
                items == expected
            }
        }
    }
}
