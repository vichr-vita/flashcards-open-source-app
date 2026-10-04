//! Transactional product SQL. Only static, parameterized SQL reaches `PostgreSQL`.
use super::{
    ToolActor,
    dialect::{self, Kind, Object, Returning, Statement, invalid},
    predicate, select,
};
use crate::{
    AppState,
    core::{
        cards,
        model::{CardSnapshot, Mutation},
        sync, workspaces,
    },
    database::scoped,
    error::ApiError,
};
use chrono::{DateTime, SecondsFormat, SubsecRound, Utc};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

const READ_INSTRUCTIONS: &str = "data.rowsTruncated is false, so no row of this page was dropped for size while this result was built, and data.totalRowCount reports how many rows the statement produced before LIMIT and OFFSET, i.e. after WHERE, UNNEST, and any GROUP BY.";
const MUTATION_INSTRUCTIONS: &str = "The mutation succeeded. Read data.affectedCount for the summary. INSERT, UPDATE, and DELETE may affect at most 100 rows per statement. Without a RETURNING clause, INSERT and UPDATE return only the identifier column in data.rows and DELETE returns no rows. data.rowsOmitted reports whether the returned rows were dropped to fit the result-size budget; the write succeeded either way. This endpoint supports the published SQL dialect, not full PostgreSQL. Use docs.discoveryUrl for runtime routes and docs.source.agentRoutesUrl for implementation details.";
const DISCOVERY_INSTRUCTIONS: &str = "Read rows from data.rows. This endpoint supports the published SQL dialect, not full PostgreSQL. Use docs.discoveryUrl for runtime routes and docs.source.agentRoutesUrl for implementation details.";

pub(super) async fn rows(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    resource: &str,
) -> Result<Vec<Object>, ApiError> {
    let values=match resource {
        "cards"=>sqlx::query_scalar::<_,Value>("SELECT to_jsonb(c) FROM content.cards c WHERE workspace_id=$1 AND deleted_at IS NULL ORDER BY created_at DESC,card_id ASC").bind(workspace).fetch_all(&mut **tx).await?,
        "decks"=>sqlx::query_scalar::<_,Value>("SELECT to_jsonb(d)||jsonb_build_object('tags',filter_definition->'tags') FROM content.decks d WHERE workspace_id=$1 AND deleted_at IS NULL ORDER BY created_at DESC,deck_id DESC").bind(workspace).fetch_all(&mut **tx).await?,
        "review_events"=>sqlx::query_scalar::<_,Value>("SELECT to_jsonb(r) FROM content.review_events r WHERE workspace_id=$1 ORDER BY reviewed_at_server DESC,review_event_id DESC").bind(workspace).fetch_all(&mut **tx).await?,
        "workspace"=>sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('workspace_id',workspace_id,'name',name,'created_at',created_at,'algorithm',fsrs_algorithm,'desired_retention',fsrs_desired_retention,'learning_steps_minutes',fsrs_learning_steps_minutes,'relearning_steps_minutes',fsrs_relearning_steps_minutes,'maximum_interval_days',fsrs_maximum_interval_days,'enable_fuzz',fsrs_enable_fuzz) FROM org.workspaces WHERE workspace_id=$1").bind(workspace).fetch_all(&mut **tx).await?,
        _=>return Err(invalid("Unknown resource")),
    };
    let descriptor = dialect::resource(resource)?;
    let cols = descriptor
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(ApiError::internal)?;
    values
        .iter()
        .map(|value| {
            let mut object = Object::new();
            for col in cols {
                let name = col
                    .get("columnName")
                    .and_then(Value::as_str)
                    .ok_or_else(ApiError::internal)?;
                let mut field = value.get(name).cloned().unwrap_or(Value::Null);
                if col.get("type").and_then(Value::as_str) == Some("datetime") && !field.is_null() {
                    let date: DateTime<Utc> = field
                        .as_str()
                        .ok_or_else(ApiError::internal)?
                        .parse()
                        .map_err(|_| ApiError::internal())?;
                    field = json!(date.to_rfc3339_opts(SecondsFormat::Millis, true));
                }
                object.insert(name.into(), field);
            }
            Ok(object)
        })
        .collect()
}

fn returning_row(
    resource: &str,
    row: &Object,
    returning: &Returning,
    delete: bool,
) -> Option<Object> {
    match returning {
        Some(columns) if columns.is_empty() => Some(row.clone()),
        Some(columns) => Some(
            columns
                .iter()
                .map(|name| (name.clone(), row.get(name).cloned().unwrap_or(Value::Null)))
                .collect(),
        ),
        None if delete => None,
        None => {
            let name = if resource == "cards" {
                "card_id"
            } else {
                "deck_id"
            };
            Some(
                std::iter::once((name.into(), row.get(name).cloned().unwrap_or(Value::Null)))
                    .collect(),
            )
        }
    }
}
fn text(fields: &Object, key: &str) -> Result<String, ApiError> {
    fields
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| invalid(format!("{key} must be a string")))
}
fn tags(fields: &Object, old: Vec<String>, deck: bool) -> Result<Vec<String>, ApiError> {
    let mut result = fields
        .get("tags")
        .map(|value| {
            value
                .as_array()
                .ok_or_else(|| invalid("tags must be a string array"))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| invalid("tags must be a string array"))
                })
                .collect::<Result<Vec<_>, ApiError>>()
        })
        .transpose()?
        .unwrap_or(old);
    let legacy = if deck {
        fields
            .get("effort_levels")
            .map(|value| {
                value
                    .as_array()
                    .ok_or_else(|| invalid("effort_levels must be a string array"))
                    .cloned()
            })
            .transpose()?
            .unwrap_or_default()
    } else {
        fields.get("effort_level").cloned().into_iter().collect()
    };
    for effort in legacy {
        let effort = effort
            .as_str()
            .filter(|value| matches!(*value, "fast" | "medium" | "long"))
            .ok_or_else(|| invalid("effort_level must be fast, medium, or long"))?;
        if effort != "fast" {
            let tag = effort.to_owned();
            if !result.contains(&tag) {
                result.push(tag);
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    result.retain(|tag| seen.insert(tag.clone()));
    Ok(result)
}
fn new_card(now: DateTime<Utc>) -> CardSnapshot {
    CardSnapshot {
        card_id: Uuid::new_v4(),
        front_text: String::new(),
        back_text: String::new(),
        card_type: "basic".into(),
        metadata: json!({"version":1,"source":{"label":null,"author":null,"comment":null,"createdAt":now.to_rfc3339_opts(SecondsFormat::Millis,true),"importedAt":null,"importId":null}}),
        tags: Vec::new(),
        due_at: None,
        created_at: now,
        reps: 0,
        lapses: 0,
        fsrs_card_state: "new".into(),
        fsrs_step_index: None,
        fsrs_stability: None,
        fsrs_difficulty: None,
        fsrs_last_reviewed_at: None,
        fsrs_scheduled_days: None,
        deleted_at: None,
    }
}

async fn write_card(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    id: Option<Uuid>,
    fields: &Object,
    mutation: &Mutation,
    delete: bool,
) -> Result<Uuid, ApiError> {
    let mut snapshot = if let Some(id) = id {
        cards::card_in_tx(tx, workspace, id)
            .await?
            .ok_or_else(|| invalid("Card not found"))?
            .snapshot
    } else {
        let mut card = new_card(mutation.client_updated_at);
        card.front_text = text(fields, "front_text")?;
        card.back_text = text(fields, "back_text")?;
        card
    };
    for name in ["front_text", "back_text", "card_type"] {
        if fields.contains_key(name) {
            let value = text(fields, name)?;
            match name {
                "front_text" => snapshot.front_text = value,
                "back_text" => snapshot.back_text = value,
                _ => snapshot.card_type = value,
            }
        }
    }
    snapshot.tags = tags(fields, snapshot.tags, false)?;
    if delete {
        snapshot.deleted_at = Some(mutation.client_updated_at);
    }
    let id = snapshot.card_id;
    cards::overwrite_card_in_tx(tx, workspace, snapshot, mutation).await?;
    Ok(id)
}

async fn write_deck(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    id: Option<Uuid>,
    fields: &Object,
    mutation: &Mutation,
    delete: bool,
) -> Result<Uuid, ApiError> {
    let old = if let Some(id) = id {
        sqlx::query_scalar::<_,Value>("SELECT to_jsonb(d) FROM content.decks d WHERE workspace_id=$1 AND deck_id=$2 AND deleted_at IS NULL FOR UPDATE").bind(workspace).bind(id).fetch_optional(&mut **tx).await?
    } else {
        None
    };
    let name = if fields.contains_key("name") {
        text(fields, "name")?
    } else {
        old.as_ref()
            .and_then(|row| row.get("name"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| invalid("name is required for INSERT INTO decks"))?
    };
    let name = name.trim();
    if name.is_empty() {
        return Err(invalid("name must not be empty"));
    }
    let old_tags = old
        .as_ref()
        .and_then(|row| row.get("filter_definition"))
        .and_then(|filter| filter.get("tags"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let filter = json!({"version":2,"tags":tags(fields,old_tags,true)?});
    let id = id.unwrap_or_else(Uuid::new_v4);
    let deleted = delete.then_some(mutation.client_updated_at);
    sqlx::query("INSERT INTO content.decks(deck_id,workspace_id,name,filter_definition,created_at,updated_at,deleted_at,client_updated_at,last_modified_by_replica_id,last_operation_id) VALUES($1,$2,$3,$4,$5,$5,$6,$5,$7,$8) ON CONFLICT(deck_id) DO UPDATE SET name=EXCLUDED.name,filter_definition=EXCLUDED.filter_definition,updated_at=now(),deleted_at=EXCLUDED.deleted_at,client_updated_at=EXCLUDED.client_updated_at,last_modified_by_replica_id=EXCLUDED.last_modified_by_replica_id,last_operation_id=EXCLUDED.last_operation_id WHERE content.decks.workspace_id=EXCLUDED.workspace_id")
        .bind(id).bind(workspace).bind(name).bind(filter).bind(mutation.client_updated_at).bind(deleted).bind(mutation.replica_id).bind(&mutation.operation_id).execute(&mut **tx).await?;
    sync::record_hot(tx, workspace, "deck", id, mutation).await?;
    Ok(id)
}

#[allow(
    clippy::too_many_lines,
    reason = "One statement validates its bounded rows, writes their tombstones or authored state, and collects matching committed facts."
)]
async fn mutate(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    statement: &Statement,
    replica: Uuid,
    facts: &mut crate::core::facts::Buffer,
) -> Result<Value, ApiError> {
    let resource = statement
        .resource
        .as_deref()
        .ok_or_else(ApiError::internal)?;
    if !matches!(resource, "cards" | "decks") {
        return Err(invalid("Resource is read-only"));
    }
    let (inputs, returning, delete) = match &statement.kind {
        Kind::Insert(inputs, returning) => (
            inputs
                .iter()
                .map(|fields| (None, fields.clone(), None))
                .collect::<Vec<_>>(),
            returning,
            false,
        ),
        Kind::Update(_, expression, returning) | Kind::Delete(expression, returning) => {
            let filter = predicate::parse(resource, None, expression, 0)?;
            let collator = predicate::collator()?;
            let updates = if let Kind::Update(fields, ..) = &statement.kind {
                fields.clone()
            } else {
                Object::new()
            };
            let identifier = if resource == "cards" {
                "card_id"
            } else {
                "deck_id"
            };
            let selected = rows(tx, workspace, resource)
                .await?
                .into_iter()
                .filter(|row| filter.matches(row, collator))
                .map(|row| {
                    let id = row
                        .get(identifier)
                        .and_then(Value::as_str)
                        .ok_or_else(ApiError::internal)?
                        .parse()
                        .map_err(|_| ApiError::internal())?;
                    Ok((Some(id), updates.clone(), Some(row)))
                })
                .collect::<Result<Vec<_>, ApiError>>()?;
            (
                selected,
                returning,
                matches!(statement.kind, Kind::Delete(..)),
            )
        }
        _ => {
            return Err(invalid(
                "sql_execute accepts only INSERT, UPDATE, or DELETE",
            ));
        }
    };
    if inputs.len() > 100 {
        return Err(invalid(format!(
            "{} may affect at most 100 records per statement",
            statement.kind.name().to_ascii_uppercase()
        )));
    }
    let affected = inputs.len();
    let mut result = Vec::new();
    for (id, fields, previous) in inputs {
        let mutation = Mutation {
            client_updated_at: Utc::now().trunc_subsecs(3),
            replica_id: replica,
            operation_id: Uuid::new_v4().to_string(),
        };
        let kind = if resource == "cards" { "card" } else { "deck" };
        let before = if let Some(id) = id {
            crate::core::facts::content(tx, workspace, kind, id).await?
        } else {
            None
        };
        let id = if resource == "cards" {
            write_card(tx, workspace, id, &fields, &mutation, delete).await?
        } else {
            write_deck(tx, workspace, id, &fields, &mutation, delete).await?
        };
        let after = crate::core::facts::content(tx, workspace, kind, id)
            .await?
            .ok_or_else(ApiError::internal)?;
        facts.content(kind, id, before.as_ref(), &after, &mutation);
        let identifier = if resource == "cards" {
            "card_id"
        } else {
            "deck_id"
        };
        let row = if delete {
            previous.ok_or_else(ApiError::internal)?
        } else {
            rows(tx, workspace, resource)
                .await?
                .into_iter()
                .find(|row| {
                    row.get(identifier).and_then(Value::as_str) == Some(id.to_string().as_str())
                })
                .ok_or_else(ApiError::internal)?
        };
        if let Some(row) = returning_row(resource, &row, returning, delete) {
            result.push(row);
        }
    }
    Ok(
        json!({"statementType":statement.kind.name(),"resource":resource,"rows":result,"affectedCount":affected,"rowsOmitted":false,"sqlOmitted":false}),
    )
}

async fn read(
    tx: &mut Transaction<'_, Postgres>,
    workspace: Uuid,
    statement: &Statement,
) -> Result<Value, ApiError> {
    let resource = statement.resource.as_deref();
    let mut payload = match &statement.kind {
        Kind::Show(pattern) => {
            let schema = dialect::schema()?;
            let expression = pattern
                .as_ref()
                .map(|pattern| predicate::like(pattern, true))
                .transpose()?;
            let values=schema.as_array().ok_or_else(ApiError::internal)?.iter().filter(|row|expression.as_ref().is_none_or(|expression|row.get("resourceName").and_then(Value::as_str).is_some_and(|name|expression.is_match(name)))).map(|row|json!({"table_name":row.get("resourceName"),"writable":row.get("writable"),"description":row.get("description")})).collect::<Vec<_>>();
            json!({"rowCount":values.len(),"totalRowCount":values.len(),"rows":values,"rowsTruncated":false,"limit":null,"offset":null,"hasMore":false})
        }
        Kind::Describe => {
            let descriptor = dialect::resource(resource.ok_or_else(ApiError::internal)?)?;
            let values=descriptor.get("columns").and_then(Value::as_array).ok_or_else(ApiError::internal)?.iter().map(|col|json!({"column_name":col.get("columnName"),"type":col.get("type"),"nullable":col.get("nullable"),"read_only":col.get("readOnly"),"filterable":col.get("filterable"),"sortable":col.get("sortable"),"description":col.get("description")})).collect::<Vec<_>>();
            json!({"rowCount":values.len(),"totalRowCount":values.len(),"rows":values,"rowsTruncated":false,"limit":null,"offset":null,"hasMore":false})
        }
        Kind::Select(select) => {
            let resource = resource.ok_or_else(ApiError::internal)?;
            select::execute(resource, select, rows(tx, workspace, resource).await?)?
        }
        _ => {
            return Err(invalid(
                "sql_query accepts only SHOW TABLES, DESCRIBE, or SELECT",
            ));
        }
    };
    let object = payload.as_object_mut().ok_or_else(ApiError::internal)?;
    object.insert("statementType".into(), json!(statement.kind.name()));
    object.insert("resource".into(), json!(resource));
    Ok(payload)
}

#[allow(
    clippy::too_many_lines,
    reason = "One ordered transaction owns the complete SQL batch and commits before applying output budgets."
)]
pub(super) async fn execute(
    state: &AppState,
    user: Uuid,
    workspace: Uuid,
    input: &str,
    write: bool,
    actor: &ToolActor,
) -> Result<Value, ApiError> {
    use sha2::{Digest as _, Sha256};
    let started = std::time::Instant::now();
    let fingerprint = format!("{:x}", Sha256::digest(input.as_bytes()));
    let outcome=tokio::time::timeout(std::time::Duration::from_secs(15),execute_inner(state,user,workspace,input,write,actor)).await.unwrap_or_else(|_|Err(ApiError::new(axum::http::StatusCode::BAD_REQUEST,"QUERY_TIME_LIMIT_EXCEEDED","The SQL execution exceeded its 15-second database time budget. Narrow the query or split the write into smaller batches.")));
    // The SQL deadline must end at commit. Cancelling a later analytics drain would report a
    // committed write as failed and invite a duplicate retry.
    let outcome = match outcome {
        Ok((result, facts)) => {
            facts.emit(state, user, workspace, None).await;
            Ok(result)
        }
        Err(error) => Err(error),
    };
    tracing::info!(event="agent_sql",user_id=%user,workspace_id=%workspace,actor_kind=actor.kind,sql_fingerprint=fingerprint,sql_length=input.encode_utf16().count(),duration_ms=started.elapsed().as_millis(),success=outcome.is_ok(),error_code=outcome.as_ref().err().map(|error|error.code.as_str()),"Product SQL execution");
    outcome
}

#[allow(
    clippy::too_many_lines,
    reason = "One ordered transaction owns the complete SQL batch and commits before applying output budgets."
)]
async fn execute_inner(
    state: &AppState,
    user: Uuid,
    workspace: Uuid,
    input: &str,
    write: bool,
    actor: &ToolActor,
) -> Result<(Value, crate::core::facts::Buffer), ApiError> {
    let statement_sqls = dialect::split(input, ";", false)?;
    let mut statement_sqls = statement_sqls;
    if statement_sqls.last().is_some_and(String::is_empty) {
        statement_sqls.pop();
    }
    if statement_sqls.is_empty() || statement_sqls.iter().any(String::is_empty) {
        return Err(invalid("sql must not be empty"));
    }
    if statement_sqls.len() > 50 {
        return Err(invalid("SQL batch must contain at most 50 statements"));
    }
    let statements = statement_sqls
        .iter()
        .enumerate()
        .map(|(index, sql)| {
            dialect::parse(sql)
                .map_err(|error| batch_error(error, index, sql, statement_sqls.len()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if statements
        .iter()
        .any(|statement| statement.kind.mutation() != write)
    {
        return Err(invalid(if write {
            "sql_execute accepts only INSERT, UPDATE, or DELETE"
        } else {
            "sql_query accepts only SHOW TABLES, DESCRIBE, or SELECT"
        }));
    }
    let mut tx = scoped(&state.pool, &user.to_string(), Some(&workspace.to_string())).await?;
    workspaces::assert_access(&mut tx, workspace).await?;
    // The deployed tool's database budget is enforced inside the scoped transaction.
    sqlx::query("SET LOCAL statement_timeout = '15s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SET LOCAL lock_timeout = '15s'")
        .execute(&mut *tx)
        .await?;
    let replica = if write {
        let replica = sync::ensure_system_replica(
            &mut tx,
            &user.to_string(),
            workspace,
            &actor.kind,
            &actor.key,
        )
        .await?;
        sqlx::query("UPDATE sync.workspace_replicas SET app_version=$2 WHERE replica_id=$1")
            .bind(replica)
            .bind(&actor.app_version)
            .execute(&mut *tx)
            .await?;
        sync::lock_hot(&mut tx, workspace).await?;
        Some(replica)
    } else {
        None
    };
    let mut facts = crate::core::facts::Buffer::default();
    let mut results = Vec::new();
    let mut total = 0_u64;
    for (index, statement) in statements.iter().enumerate() {
        let result = if let Some(replica) = replica {
            mutate(&mut tx, workspace, statement, replica, &mut facts)
                .await
                .map_err(|error| {
                    batch_error(error, index, &statement.normalized, statements.len())
                })?
        } else {
            read(&mut tx, workspace, statement).await.map_err(|error| {
                batch_error(error, index, &statement.normalized, statements.len())
            })?
        };
        total = total
            .checked_add(
                result
                    .get("affectedCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            )
            .ok_or_else(ApiError::internal)?;
        results.push(result);
    }
    tx.commit().await?;
    let normalized = statements
        .iter()
        .map(|statement| statement.normalized.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    let mut data = if results.len() == 1 {
        results.pop().ok_or_else(ApiError::internal)?
    } else {
        json!({"statementType":"batch","resource":null,"statements":results,"statementCount":statements.len(),"affectedCountTotal":if write{json!(total)}else{Value::Null},"rowsOmitted":false,"sqlOmitted":false})
    };
    let object = data.as_object_mut().ok_or_else(ApiError::internal)?;
    object.insert("sql".into(), json!(input));
    object.insert("normalizedSql".into(), json!(normalized));
    object.insert("workspaceId".into(), json!(workspace));
    let instructions = if statements.len() > 1 {
        if write {
            "The batch mutation succeeded. Read data.statements for per-statement results and data.affectedCountTotal for the summary. The batch committed atomically. Returned rows may be omitted to fit the result-size budget.".into()
        } else {
            "Read rows from data.statements. This endpoint supports the published SQL dialect, not full PostgreSQL.".into()
        }
    } else if write {
        MUTATION_INSTRUCTIONS.into()
    } else if matches!(
        statements.first().map(|statement| &statement.kind),
        Some(Kind::Show(_) | Kind::Describe)
    ) {
        DISCOVERY_INSTRUCTIONS.into()
    } else {
        format!(
            "{READ_INSTRUCTIONS} {} LIMIT defaults to 100 and is capped at 100. SELECT returns at most 100 rows per statement. Prefer a stable ORDER BY clause when paginating. This endpoint supports the published SQL dialect, not full PostgreSQL. Use docs.discoveryUrl for runtime routes and docs.source.agentRoutesUrl for implementation details.",
            if data.get("hasMore") == Some(&json!(true)) {
                "Repeat the same query with a larger OFFSET to continue pagination."
            } else {
                "No further rows are available for this query."
            }
        )
    };
    budget(json!({"data":data,"instructions":instructions}), write).map(|result| (result, facts))
}

fn chars(value: &Value) -> usize {
    value.to_string().encode_utf16().count()
}
fn budget(mut envelope: Value, write: bool) -> Result<Value, ApiError> {
    if chars(&envelope) <= 48_000 {
        return Ok(envelope);
    }
    if write {
        return mutation_budget(envelope);
    }
    let data = envelope
        .get_mut("data")
        .and_then(Value::as_object_mut)
        .ok_or_else(ApiError::internal)?;
    if data.get("statementType").and_then(Value::as_str) != Some("select") {
        return Err(ApiError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "QUERY_RESULT_TOO_LARGE",
            "SQL result is too large. Narrow the query.",
        ));
    }
    loop {
        let data = envelope
            .get_mut("data")
            .and_then(Value::as_object_mut)
            .ok_or_else(ApiError::internal)?;
        let rows = data
            .get_mut("rows")
            .and_then(Value::as_array_mut)
            .ok_or_else(ApiError::internal)?;
        if rows.len() <= 1 {
            return Err(ApiError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "QUERY_RESULT_TOO_LARGE",
                "SQL result is too large. Narrow the query.",
            ));
        }
        rows.pop();
        let count = rows.len();
        data.insert("rowCount".into(), json!(count));
        data.insert("rowsTruncated".into(), json!(true));
        data.insert("hasMore".into(), json!(true));
        if let Some(object) = envelope.as_object_mut() {
            object.insert("instructions".into(),json!("This answer is partial: data.rowsTruncated is true. Continue with the same ORDER BY at OFFSET = data.offset + data.rowCount; do not advance by LIMIT, because that would skip dropped rows. Narrow the query to reduce its size."));
        }
        if chars(&envelope) <= 48_000 {
            return Ok(envelope);
        }
    }
}

fn preview(sql: &str) -> String {
    if sql.encode_utf16().count() <= 120 {
        return sql.into();
    }
    let prefix = sql.encode_utf16().take(117).collect::<Vec<_>>();
    format!("{}...", String::from_utf16_lossy(&prefix))
}
fn mutation_budget(mut envelope: Value) -> Result<Value, ApiError> {
    let mut shortened = envelope.clone();
    let data = shortened
        .get_mut("data")
        .and_then(Value::as_object_mut)
        .ok_or_else(ApiError::internal)?;
    for name in ["sql", "normalizedSql"] {
        if let Some(sql) = data.get(name).and_then(Value::as_str) {
            let preview = preview(sql);
            data.insert(name.into(), json!(preview));
        }
    }
    data.insert("sqlOmitted".into(), json!(true));
    let instructions = shortened
        .get("instructions")
        .and_then(Value::as_str)
        .ok_or_else(ApiError::internal)?;
    let instructions = format!(
        "{instructions} The echoed statement text in data.sql and data.normalizedSql was shortened where it exceeded the preview length, because the payload exceeded the result-size budget. The write itself succeeded and the statement that ran is unchanged, so do not repeat it."
    );
    shortened
        .as_object_mut()
        .ok_or_else(ApiError::internal)?
        .insert("instructions".into(), json!(instructions));
    if chars(&shortened) < chars(&envelope) {
        envelope = shortened;
    }
    if chars(&envelope) <= 48_000 {
        return Ok(envelope);
    }
    let data = envelope
        .get_mut("data")
        .and_then(Value::as_object_mut)
        .ok_or_else(ApiError::internal)?;
    let mut omitted = false;
    if let Some(statements) = data.get_mut("statements").and_then(Value::as_array_mut) {
        for statement in statements {
            if let Some(object) = statement.as_object_mut() {
                omitted |= object
                    .get("rows")
                    .and_then(Value::as_array)
                    .is_some_and(|rows| !rows.is_empty());
                object.insert("rows".into(), json!([]));
            }
        }
    } else if data
        .get("rows")
        .and_then(Value::as_array)
        .is_some_and(|rows| !rows.is_empty())
    {
        data.insert("rows".into(), json!([]));
        omitted = true;
    }
    if omitted {
        data.insert("rowsOmitted".into(), json!(true));
        let instructions = envelope
            .get("instructions")
            .and_then(Value::as_str)
            .ok_or_else(ApiError::internal)?;
        let instructions = format!(
            "{instructions} The returned rows were omitted because the payload exceeded the result-size budget. The write itself succeeded, so do not repeat it."
        );
        envelope
            .as_object_mut()
            .ok_or_else(ApiError::internal)?
            .insert("instructions".into(), json!(instructions));
    }
    Ok(envelope)
}

fn batch_error(mut error: ApiError, index: usize, sql: &str, count: usize) -> ApiError {
    if count > 1 {
        let Some(number) = index.checked_add(1) else {
            return ApiError::internal();
        };
        error.message = format!(
            "SQL batch statement {number} failed: {}. Statement: {}",
            error.message,
            preview(sql)
        );
    }
    error
}
