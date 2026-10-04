//! In-memory SELECT semantics shared by reads and mutation target selection.
use super::{
    dialect::{self, Object, Select, invalid},
    predicate,
};
use crate::error::ApiError;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn project(row: &Object, select: &Select) -> Object {
    select
        .projection
        .iter()
        .map(|item| {
            (
                item.alias.clone(),
                row.get(&item.column).cloned().unwrap_or(Value::Null),
            )
        })
        .collect()
}

#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::as_conversions,
    reason = "Published SQL SUM and AVG use JavaScript double arithmetic over bounded projected values."
)]
fn aggregate(
    rows: &[Object],
    select: &Select,
    collator: &icu_collator::CollatorBorrowed<'_>,
) -> Result<Object, ApiError> {
    let mut output = Object::new();
    for item in &select.projection {
        let values = rows
            .iter()
            .filter_map(|row| row.get(&item.column))
            .filter(|value| !value.is_null() && !value.is_array() && !value.is_object())
            .collect::<Vec<_>>();
        let value = match item.aggregate.as_deref() {
            None => rows
                .first()
                .and_then(|row| row.get(&item.column))
                .cloned()
                .unwrap_or(Value::Null),
            Some("count") => json!(rows.len()),
            Some("sum") => json!(
                values
                    .iter()
                    .filter_map(|value| value.as_f64())
                    .sum::<f64>()
            ),
            Some("avg") => {
                let numbers = values
                    .iter()
                    .filter_map(|value| value.as_f64())
                    .collect::<Vec<_>>();
                if numbers.is_empty() {
                    Value::Null
                } else {
                    json!(numbers.iter().sum::<f64>() / numbers.len() as f64)
                }
            }
            Some(function @ ("min" | "max")) => {
                let mut ordered = values;
                ordered.sort_by(|a, b| predicate::compare(a, b, collator));
                if function == "min" {
                    ordered.first()
                } else {
                    ordered.last()
                }
                .copied()
                .cloned()
                .unwrap_or(Value::Null)
            }
            Some(_) => return Err(invalid("Unknown aggregate")),
        };
        output.insert(item.alias.clone(), value);
    }
    Ok(output)
}

#[allow(
    clippy::too_many_lines,
    reason = "SELECT expansion, grouping, ordering and paging share one bounded row pipeline."
)]
pub(super) fn execute(
    resource: &str,
    select: &Select,
    rows: Vec<Object>,
) -> Result<Value, ApiError> {
    let collator = predicate::collator()?;
    let mut expanded = Vec::new();
    for row in rows {
        if let Some(alias) = &select.unnest {
            if let Some(tags) = row.get("tags").and_then(Value::as_array) {
                for tag in tags {
                    let mut tagged = row.clone();
                    tagged.insert(alias.clone(), tag.clone());
                    expanded.push(tagged);
                }
            }
        } else {
            expanded.push(row);
        }
    }
    if let Some(expression) = &select.predicate {
        let filter = predicate::parse(resource, select.unnest.as_deref(), expression, 0)?;
        expanded.retain(|row| filter.matches(row, collator));
    }
    let grouped = !select.groups.is_empty()
        || select
            .projection
            .iter()
            .any(|item| item.aggregate.is_some());
    let mut rows = if grouped {
        for item in &select.projection {
            if matches!(item.aggregate.as_deref(), Some("sum" | "avg"))
                && !matches!(
                    dialect::column(resource, select.unnest.as_deref(), &item.column)?
                        .get("type")
                        .and_then(Value::as_str),
                    Some("number" | "integer")
                )
            {
                return Err(invalid("SUM and AVG only support numeric columns"));
            }
        }
        if select.groups.is_empty() {
            vec![aggregate(&expanded, select, collator)?]
        } else {
            let mut indices = BTreeMap::<String, usize>::new();
            let mut groups: Vec<Vec<Object>> = Vec::new();
            for row in expanded {
                let key = json!(
                    select
                        .groups
                        .iter()
                        .map(|name| row.get(name).cloned().unwrap_or(Value::Null))
                        .collect::<Vec<_>>()
                )
                .to_string();
                if let Some(&index) = indices.get(&key) {
                    groups
                        .get_mut(index)
                        .ok_or_else(ApiError::internal)?
                        .push(row);
                } else {
                    indices.insert(key, groups.len());
                    groups.push(vec![row]);
                }
            }
            groups
                .iter()
                .map(|rows| aggregate(rows, select, collator))
                .collect::<Result<Vec<_>, _>>()?
        }
    } else {
        expanded
    };
    if select
        .order
        .first()
        .is_some_and(|(name, _)| name == "random()")
    {
        // Random UUID keys provide the same uniformly shuffled contract without index arithmetic.
        rows.sort_by_cached_key(|_| uuid::Uuid::new_v4());
    } else {
        rows.sort_by(|a, b| {
            for (name, descending) in &select.order {
                let order = predicate::compare(
                    a.get(name).unwrap_or(&Value::Null),
                    b.get(name).unwrap_or(&Value::Null),
                    collator,
                );
                if !order.is_eq() {
                    return if *descending { order.reverse() } else { order };
                }
            }
            std::cmp::Ordering::Equal
        });
    }
    let total = rows.len();
    let paged = rows
        .into_iter()
        .skip(select.offset)
        .take(select.limit)
        .map(|row| {
            if !grouped
                && select
                    .projection
                    .first()
                    .is_some_and(|item| item.column != "*")
            {
                project(&row, select)
            } else {
                row
            }
        })
        .collect::<Vec<_>>();
    let delivered = paged.len();
    Ok(
        json!({"rows":paged,"rowCount":delivered,"totalRowCount":total,"rowsTruncated":false,"limit":select.limit,"offset":select.offset,"hasMore":select.offset.checked_add(delivered).is_some_and(|end|end<total)}),
    )
}
