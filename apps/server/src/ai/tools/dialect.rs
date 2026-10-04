//! Parser for the published product SQL dialect. Its AST is never sent to `PostgreSQL`.
use crate::error::ApiError;
use axum::http::StatusCode;
use regex::Regex;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) type Object = serde_json::Map<String, Value>;

pub(super) fn invalid(message: impl Into<String>) -> ApiError {
    let message = message.into();
    ApiError::new(StatusCode::BAD_REQUEST, "QUERY_INVALID_SQL", &message).with_details(
        json!({"validationIssues":[{"path":"sql","code":"invalid_sql","message":message}]}),
    )
}

pub(super) fn captures(pattern: &str, input: &str) -> Result<Option<Vec<String>>, ApiError> {
    let expression = Regex::new(pattern).map_err(|_| ApiError::internal())?;
    Ok(expression.captures(input).map(|groups| {
        groups
            .iter()
            .skip(1)
            .map(|group| group.map_or_else(String::new, |m| m.as_str().to_owned()))
            .collect()
    }))
}
pub(super) fn field(groups: &[String], index: usize) -> &str {
    groups.get(index).map_or("", String::as_str)
}

/// Scan only at balanced, unquoted boundaries; SQL comments and quoted identifiers are unsupported.
pub(super) fn positions(input: &str, needle: &str, word: bool) -> Result<Vec<usize>, ApiError> {
    let mut result = Vec::new();
    let mut depth = 0_u16;
    let mut quoted = false;
    let mut double = false;
    let mut chars = input.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if quoted {
            if ch == '\'' {
                if chars.peek().is_some_and(|(_, next)| *next == '\'') {
                    chars.next();
                } else {
                    quoted = false;
                }
            }
            continue;
        }
        if double {
            if ch == '"' {
                double = false;
            } else if ch == '\\' {
                chars.next();
            }
            continue;
        }
        if depth == 0
            && input
                .get(index..)
                .and_then(|tail| tail.get(..needle.len()))
                .is_some_and(|part| part.eq_ignore_ascii_case(needle))
        {
            let end = index
                .checked_add(needle.len())
                .ok_or_else(ApiError::internal)?;
            let before = input.get(..index).and_then(|part| part.chars().next_back());
            let after = input.get(end..).and_then(|part| part.chars().next());
            if !word
                || (before.is_none_or(crate::core::cards::js_space)
                    && after.is_none_or(crate::core::cards::js_space))
            {
                result.push(index);
            }
        }
        match ch {
            '\'' => quoted = true,
            '"' => double = true,
            '(' | '[' | '{' => {
                depth = depth
                    .checked_add(1)
                    .ok_or_else(|| invalid("SQL nesting is too deep"))?;
            }
            ')' | ']' | '}' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| invalid("Unbalanced SQL delimiters"))?;
            }
            _ => {}
        }
    }
    if quoted || double || depth != 0 {
        return Err(invalid("Unbalanced SQL literals or delimiters"));
    }
    Ok(result)
}
pub(super) fn split(input: &str, needle: &str, word: bool) -> Result<Vec<String>, ApiError> {
    let points = positions(input, needle, word)?;
    let mut start = 0;
    let mut parts = Vec::new();
    for index in points {
        parts.push(
            input
                .get(start..index)
                .ok_or_else(ApiError::internal)?
                .trim_matches(crate::core::cards::js_space)
                .into(),
        );
        start = index
            .checked_add(needle.len())
            .ok_or_else(ApiError::internal)?;
    }
    parts.push(
        input
            .get(start..)
            .ok_or_else(ApiError::internal)?
            .trim_matches(crate::core::cards::js_space)
            .into(),
    );
    Ok(parts)
}
pub(super) fn normalize(input: &str) -> String {
    let mut out = String::new();
    let mut quote = false;
    let mut pending = false;
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if quote {
            out.push(ch);
            if ch == '\'' {
                if chars.peek() == Some(&'\'') {
                    out.push('\'');
                    chars.next();
                } else {
                    quote = false;
                }
            }
        } else if crate::core::cards::js_space(ch) {
            pending = true;
        } else {
            if pending && !out.is_empty() {
                out.push(' ');
            }
            pending = false;
            out.push(ch);
            if ch == '\'' {
                quote = true;
            }
        }
    }
    out
}
pub(super) fn clauses(
    input: &str,
    keys: &[&str],
) -> Result<(String, BTreeMap<String, String>), ApiError> {
    let mut matches = Vec::new();
    for (rank, key) in keys.iter().enumerate() {
        for index in positions(input, key, true)? {
            matches.push((index, rank, *key));
        }
    }
    matches.sort_by_key(|(index, _, _)| *index);
    let mut values = BTreeMap::new();
    let mut last = None;
    for (offset, &(index, rank, key)) in matches.iter().enumerate() {
        if last.is_some_and(|previous| previous >= rank) {
            return Err(invalid("SQL clauses are duplicated or out of order"));
        }
        last = Some(rank);
        let start = index
            .checked_add(key.len())
            .ok_or_else(ApiError::internal)?;
        let end = offset
            .checked_add(1)
            .and_then(|n| matches.get(n))
            .map_or(input.len(), |(n, _, _)| *n);
        let value = input
            .get(start..end)
            .ok_or_else(ApiError::internal)?
            .trim_matches(crate::core::cards::js_space);
        if value.is_empty() {
            return Err(invalid(format!("{key} must not be empty")));
        }
        values.insert(key.to_string(), value.to_owned());
    }
    Ok((
        input
            .get(..matches.first().map_or(input.len(), |(index, _, _)| *index))
            .ok_or_else(ApiError::internal)?
            .trim_matches(crate::core::cards::js_space)
            .into(),
        values,
    ))
}
pub(super) fn schema() -> Result<Value, ApiError> {
    serde_json::from_str(include_str!("schema.json")).map_err(|_| ApiError::internal())
}
pub(super) fn resource(name: &str) -> Result<Value, ApiError> {
    schema()?
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get("resourceName").and_then(Value::as_str) == Some(name))
        })
        .cloned()
        .ok_or_else(|| invalid(format!("Unknown resource: {name}")))
}
pub(super) fn column(resource: &str, alias: Option<&str>, name: &str) -> Result<Value, ApiError> {
    if alias == Some(name) {
        return Ok(json!({"type":"string","filterable":true,"sortable":true,"readOnly":true}));
    }
    self::resource(resource)?
        .get("columns")
        .and_then(Value::as_array)
        .and_then(|cols| {
            cols.iter()
                .find(|col| col.get("columnName").and_then(Value::as_str) == Some(name))
        })
        .cloned()
        .ok_or_else(|| invalid(format!("Unknown column: {name}")))
}
pub(super) fn writable(resource: &str, name: &str) -> Result<(), ApiError> {
    if (resource == "cards" && name == "effort_level")
        || (resource == "decks" && name == "effort_levels")
    {
        return Ok(());
    }
    if column(resource, None, name)?.get("readOnly") == Some(&Value::Bool(false)) {
        Ok(())
    } else {
        Err(invalid(format!("Column is read-only: {name}")))
    }
}
pub(super) fn string(input: &str) -> Result<String, ApiError> {
    let trimmed = input.trim_matches(crate::core::cards::js_space);
    if captures(r"(?s)^'((?:''|[^'])*)'$", trimmed)?.is_none() {
        return Err(invalid("Expected a quoted string literal"));
    }
    Ok(trimmed
        .get(
            1..trimmed
                .len()
                .checked_sub(1)
                .ok_or_else(ApiError::internal)?,
        )
        .ok_or_else(ApiError::internal)?
        .replace("''", "'"))
}
pub(super) fn literal(input: &str) -> Result<Value, ApiError> {
    let input = input.trim_matches(crate::core::cards::js_space);
    if input.eq_ignore_ascii_case("null") {
        return Ok(Value::Null);
    }
    if input.eq_ignore_ascii_case("true") {
        return Ok(json!(true));
    }
    if input.eq_ignore_ascii_case("false") {
        return Ok(json!(false));
    }
    if input.starts_with('\'') {
        return string(input).map(Value::String);
    }
    if captures(r"^-?\d+(?:\.\d+)?$", input)?.is_some() {
        return serde_json::from_str(input).map_err(|_| invalid("Invalid numeric literal"));
    }
    Err(invalid(format!("Unsupported literal: {input}")))
}
pub(super) fn string_array(input: &str) -> Result<Vec<String>, ApiError> {
    let mut text = input.trim_matches(crate::core::cards::js_space).to_owned();
    if text.starts_with('\'') {
        text = string(&text)?;
    }
    if text
        .get(..5)
        .is_some_and(|s| s.eq_ignore_ascii_case("ARRAY"))
    {
        text = text
            .get(5..)
            .ok_or_else(ApiError::internal)?
            .trim_matches(crate::core::cards::js_space)
            .into();
    }
    let first = text.chars().next();
    let last = text.chars().next_back();
    if !matches!(
        (first, last),
        (Some('['), Some(']')) | (Some('('), Some(')')) | (Some('{'), Some('}'))
    ) {
        return Err(invalid("Expected a string array literal"));
    }
    let inside = text
        .get(1..text.len().checked_sub(1).ok_or_else(ApiError::internal)?)
        .ok_or_else(ApiError::internal)?;
    if inside.trim_matches(crate::core::cards::js_space).is_empty() {
        return Ok(Vec::new());
    }
    split(inside, ",", false)?
        .iter()
        .map(|item| {
            if item.starts_with('\'') {
                string(item)
            } else if first == Some('{') {
                if item.starts_with('"') {
                    serde_json::from_str::<String>(item)
                        .map_err(|_| invalid("Invalid array string"))
                } else {
                    Ok(item.clone())
                }
            } else {
                Err(invalid("Array values must be quoted strings"))
            }
        })
        .collect()
}

#[derive(Clone, Debug)]
pub(super) struct Statement {
    pub normalized: String,
    pub resource: Option<String>,
    pub kind: Kind,
}
#[derive(Clone, Debug)]
pub(super) enum Kind {
    Show(Option<String>),
    Describe,
    Select(Select),
    Insert(Vec<Object>, Returning),
    Update(Object, String, Returning),
    Delete(String, Returning),
}
impl Kind {
    pub const fn mutation(&self) -> bool {
        matches!(self, Self::Insert(..) | Self::Update(..) | Self::Delete(..))
    }
    pub const fn name(&self) -> &str {
        match self {
            Self::Show(_) => "show_tables",
            Self::Describe => "describe",
            Self::Select(_) => "select",
            Self::Insert(..) => "insert",
            Self::Update(..) => "update",
            Self::Delete(..) => "delete",
        }
    }
}
pub(super) type Returning = Option<Vec<String>>;
#[derive(Clone, Debug)]
pub(super) struct Projection {
    pub column: String,
    pub alias: String,
    pub aggregate: Option<String>,
}
#[derive(Clone, Debug)]
pub(super) struct Select {
    pub projection: Vec<Projection>,
    pub predicate: Option<String>,
    pub groups: Vec<String>,
    pub order: Vec<(String, bool)>,
    pub unnest: Option<String>,
    pub limit: usize,
    pub offset: usize,
}
fn returning(resource: &str, part: Option<&String>) -> Result<Returning, ApiError> {
    part.map(|value| {
        if value == "*" {
            Ok(Vec::new())
        } else {
            split(value, ",", false)?
                .iter()
                .map(|name| {
                    let name = name.to_lowercase();
                    column(resource, None, &name)?;
                    Ok(name)
                })
                .collect()
        }
    })
    .transpose()
}
fn assignments(resource: &str, input: &str) -> Result<Object, ApiError> {
    let mut fields = Object::new();
    for part in split(input, ",", false)? {
        let groups = captures(r"(?is)^([a-z_][a-z0-9_]*)\s*=\s*(.+)$", &part)?
            .ok_or_else(|| invalid("Unsupported UPDATE assignment"))?;
        let name = field(&groups, 0).to_lowercase();
        writable(resource, &name)?;
        if fields.contains_key(&name) {
            return Err(invalid("Duplicate assignment column"));
        }
        fields.insert(name.clone(), mutation_literal(&name, field(&groups, 1))?);
    }
    Ok(fields)
}
fn mutation_literal(name: &str, input: &str) -> Result<Value, ApiError> {
    if matches!(name, "tags" | "effort_levels") {
        Ok(json!(string_array(input)?))
    } else {
        literal(input)
    }
}

#[allow(
    clippy::too_many_lines,
    clippy::option_if_let_else,
    reason = "One grammar dispatcher keeps the accepted statement vocabulary explicit."
)]
pub(super) fn parse(input: &str) -> Result<Statement, ApiError> {
    let normalized = normalize(input);
    let upper = normalized.to_ascii_uppercase();
    if let Some(groups) = captures(r"(?is)^SHOW TABLES(?: LIKE (.+))?$", &normalized)? {
        let pattern = field(&groups, 0);
        return Ok(Statement {
            normalized,
            resource: None,
            kind: Kind::Show(if pattern.is_empty() {
                None
            } else {
                Some(string(pattern)?)
            }),
        });
    }
    if let Some(groups) = captures(
        r"(?i)^(?:DESCRIBE|SHOW COLUMNS FROM) ([a-z_][a-z0-9_]*)$",
        &normalized,
    )? {
        let name = field(&groups, 0).to_lowercase();
        resource(&name)?;
        return Ok(Statement {
            normalized,
            resource: Some(name),
            kind: Kind::Describe,
        });
    }
    if upper.starts_with("SELECT ") {
        let body = normalized.get(7..).ok_or_else(ApiError::internal)?;
        let (projection, clauses) = clauses(
            body,
            &["FROM", "WHERE", "GROUP BY", "ORDER BY", "LIMIT", "OFFSET"],
        )?;
        let from = clauses
            .get("FROM")
            .ok_or_else(|| invalid("SELECT requires FROM"))?;
        let groups = captures(
            r"(?i)^([a-z_][a-z0-9_]*)(?: UNNEST ([a-z_][a-z0-9_]*) AS ([a-z_][a-z0-9_]*))?$",
            from,
        )?
        .ok_or_else(|| invalid("Unsupported FROM source"))?;
        let name = field(&groups, 0).to_lowercase();
        resource(&name)?;
        let unnest = if field(&groups, 1).is_empty() {
            None
        } else {
            if name != "cards" || !field(&groups, 1).eq_ignore_ascii_case("tags") {
                return Err(invalid("UNNEST is only supported for cards.tags"));
            }
            Some(field(&groups, 2).to_lowercase())
        };
        let projections = split(&projection, ",", false)?
            .iter()
            .map(|item| {
                let (expression, alias) =
                    if let Some(groups) = captures(r"(?is)^(.+?) AS ([a-z_][a-z0-9_]*)$", item)? {
                        (
                            field(&groups, 0).to_owned(),
                            Some(field(&groups, 1).to_lowercase()),
                        )
                    } else {
                        (item.clone(), None)
                    };
                if expression == "*" {
                    return Ok(Projection {
                        column: "*".into(),
                        alias: "*".into(),
                        aggregate: None,
                    });
                }
                let (column, aggregate) = if let Some(groups) = captures(
                    r"(?i)^(COUNT|SUM|AVG|MIN|MAX)\s*\(\s*(\*|[a-z_][a-z0-9_]*)\s*\)$",
                    &expression,
                )? {
                    let aggregate = field(&groups, 0).to_lowercase();
                    let column = field(&groups, 1).to_lowercase();
                    if (aggregate == "count") != (column == "*") {
                        return Err(invalid("COUNT supports only COUNT(*)"));
                    }
                    (column, Some(aggregate))
                } else {
                    if captures(r"(?i)^[a-z_][a-z0-9_]*$", &expression)?.is_none() {
                        return Err(invalid("Unsupported SELECT expression"));
                    }
                    (expression.to_lowercase(), None)
                };
                if column != "*" {
                    self::column(&name, unnest.as_deref(), &column)?;
                }
                let default = aggregate.as_ref().map_or_else(
                    || column.clone(),
                    |function| {
                        if column == "*" {
                            function.clone()
                        } else {
                            format!("{function}_{column}")
                        }
                    },
                );
                Ok(Projection {
                    column,
                    alias: alias.unwrap_or(default),
                    aggregate,
                })
            })
            .collect::<Result<Vec<_>, ApiError>>()?;
        if projections.len() > 1
            && projections
                .iter()
                .any(|item| item.column == "*" && item.aggregate.is_none())
        {
            return Err(invalid("SELECT * cannot be mixed with other projections"));
        }
        let group_by = clauses
            .get("GROUP BY")
            .map(|value| split(value, ",", false))
            .transpose()?
            .unwrap_or_default()
            .into_iter()
            .map(|item| {
                let item = item.to_lowercase();
                column(&name, unnest.as_deref(), &item)?;
                Ok(item)
            })
            .collect::<Result<Vec<_>, ApiError>>()?;
        let grouped = !group_by.is_empty() || projections.iter().any(|p| p.aggregate.is_some());
        if grouped
            && projections
                .iter()
                .any(|p| p.aggregate.is_none() && !group_by.contains(&p.column))
        {
            return Err(invalid(
                "Grouped SELECT must list projected columns in GROUP BY",
            ));
        }
        let order = clauses
            .get("ORDER BY")
            .map(|value| split(value, ",", false))
            .transpose()?
            .unwrap_or_default()
            .iter()
            .map(|part| {
                if captures(r"(?i)^RANDOM\s*\(\s*\)$", part)?.is_some() {
                    return Ok(("random()".into(), false));
                }
                let groups = captures(r"(?i)^([a-z_][a-z0-9_]*)(?: (ASC|DESC))?$", part)?
                    .ok_or_else(|| invalid("Unsupported ORDER BY item"))?;
                let target = field(&groups, 0).to_lowercase();
                if grouped {
                    if !group_by.contains(&target) && !projections.iter().any(|p| p.alias == target)
                    {
                        return Err(invalid("Unknown ORDER BY target"));
                    }
                } else if column(&name, unnest.as_deref(), &target)?.get("sortable")
                    != Some(&json!(true))
                {
                    return Err(invalid("Column is not sortable"));
                }
                Ok((target, field(&groups, 1).eq_ignore_ascii_case("DESC")))
            })
            .collect::<Result<Vec<_>, ApiError>>()?;
        if order.len() > 1 && order.iter().any(|(name, _)| name == "random()") {
            return Err(invalid("RANDOM() must be the only ORDER BY item"));
        }
        let number = |key: &str, default: usize| -> Result<usize, ApiError> {
            clauses.get(key).map_or(Ok(default), |value| {
                if value.chars().all(|ch| ch.is_ascii_digit()) {
                    value
                        .parse()
                        .map_err(|_| invalid(format!("{key} is too large")))
                } else {
                    Err(invalid(format!("{key} must be a non-negative integer")))
                }
            })
        };
        let limit = number("LIMIT", 100)?.min(100);
        if limit == 0 {
            return Err(invalid("LIMIT must be positive"));
        }
        let offset = number("OFFSET", 0)?;
        return Ok(Statement {
            normalized,
            resource: Some(name),
            kind: Kind::Select(Select {
                projection: projections,
                predicate: clauses.get("WHERE").cloned(),
                groups: group_by,
                order,
                unnest,
                limit,
                offset,
            }),
        });
    }
    if upper.starts_with("INSERT ") {
        let (leading, parts) = clauses(&normalized, &["RETURNING"])?;
        let groups = captures(
            r"(?is)^INSERT INTO ([a-z_][a-z0-9_]*)\s*\((.+)\)\s*VALUES\s*(.+)$",
            &leading,
        )?
        .ok_or_else(|| invalid("Unsupported INSERT statement"))?;
        let name = field(&groups, 0).to_lowercase();
        resource(&name)?;
        let cols = split(field(&groups, 1), ",", false)?
            .iter()
            .map(|name| {
                let name = name.to_lowercase();
                writable(&name_resource(&groups), &name)?;
                Ok(name)
            })
            .collect::<Result<Vec<_>, ApiError>>()?;
        let mut seen = std::collections::HashSet::new();
        if cols.iter().any(|col| !seen.insert(col)) {
            return Err(invalid("Duplicate INSERT column"));
        }
        let mut rows = Vec::new();
        for row in split(field(&groups, 2), ",", false)? {
            let inner = row
                .strip_prefix('(')
                .and_then(|text| text.strip_suffix(')'))
                .ok_or_else(|| invalid("VALUES requires parenthesized rows"))?;
            let values = split(inner, ",", false)?;
            if values.len() != cols.len() {
                return Err(invalid("INSERT column and value counts differ"));
            }
            let mut object = Object::new();
            for (col, value) in cols.iter().zip(values) {
                object.insert(col.clone(), mutation_literal(col, &value)?);
            }
            rows.push(object);
        }
        return Ok(Statement {
            normalized,
            resource: Some(name.clone()),
            kind: Kind::Insert(rows, returning(&name, parts.get("RETURNING"))?),
        });
    }
    if upper.starts_with("UPDATE ") {
        let groups = captures(r"(?is)^UPDATE ([a-z_][a-z0-9_]*) SET (.+)$", &normalized)?
            .ok_or_else(|| invalid("Unsupported UPDATE statement"))?;
        let name = field(&groups, 0).to_lowercase();
        resource(&name)?;
        let (leading, parts) = clauses(field(&groups, 1), &["WHERE", "RETURNING"])?;
        let predicate = parts
            .get("WHERE")
            .ok_or_else(|| invalid("UPDATE requires WHERE"))?
            .clone();
        return Ok(Statement {
            normalized,
            resource: Some(name.clone()),
            kind: Kind::Update(
                assignments(&name, &leading)?,
                predicate,
                returning(&name, parts.get("RETURNING"))?,
            ),
        });
    }
    if upper.starts_with("DELETE ") {
        let groups = captures(r"(?is)^DELETE FROM (.+)$", &normalized)?
            .ok_or_else(|| invalid("Unsupported DELETE statement"))?;
        let (name, parts) = clauses(field(&groups, 0), &["WHERE", "RETURNING"])?;
        let name = name.to_lowercase();
        if resource(&name)?.get("writable") != Some(&json!(true)) {
            return Err(invalid("Resource is read-only"));
        }
        let predicate = parts
            .get("WHERE")
            .ok_or_else(|| invalid("DELETE requires WHERE"))?
            .clone();
        return Ok(Statement {
            normalized,
            resource: Some(name.clone()),
            kind: Kind::Delete(predicate, returning(&name, parts.get("RETURNING"))?),
        });
    }
    Err(invalid("Unsupported SQL statement"))
}
fn name_resource(groups: &[String]) -> String {
    field(groups, 0).to_lowercase()
}
