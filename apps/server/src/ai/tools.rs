//! One tool registry for browser chat and the owner-managed MCP surface.
mod dialect;
mod predicate;
mod review;
mod select;
mod sql;
mod usage;

use crate::{AppState, auth, core::workspaces, error::ApiError};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::get,
};
use serde_json::{Value, json};
use uuid::Uuid;

pub(super) struct ToolActor {
    pub kind: String,
    pub key: String,
    pub platform: String,
    pub app_version: String,
}

pub(super) fn router() -> Router<AppState> {
    Router::new().route("/v1/me/ai-usage", get(usage_route))
}
async fn usage_route(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let identity = auth::authenticate(&state, &headers).await?;
    Ok(Json(usage(&state, identity.user_id).await?))
}

#[allow(
    clippy::too_many_lines,
    reason = "This compiled JSON inventory preserves the eight deployed tool schemas verbatim."
)]
pub(super) fn definitions() -> Value {
    json!([
      {
        "type": "function",
        "name": "sql_query",
        "description": "Read workspace-scoped workspace, cards, decks and review_events using SHOW TABLES, DESCRIBE, SHOW COLUMNS FROM or SELECT; writes require sql_execute. Decks are tag filters; cards have no deck_id. SELECT returns at most 100 rows; paginate with LIMIT, OFFSET and stable ORDER BY. Discover schema in a separate call before composing a batch. Read get_guide(sql_dialect) for grammar and examples; this is not full PostgreSQL.",
        "parameters": {
          "type": "object",
          "properties": {
            "sql": {
              "type": "string",
              "minLength": 1,
              "description": "One or more read statements in the published lingvichr SQL dialect (SHOW TABLES, DESCRIBE, SHOW COLUMNS, SELECT)."
            },
            "workspaceId": {
              "description": "Workspace UUID from list_workspaces; omit for the selected default.",
              "type": "string",
              "format": "uuid",
              "pattern": "^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$"
            }
          },
          "required": [
            "sql"
          ],
          "additionalProperties": false
        },
        "strict": false
      },
      {
        "type": "function",
        "name": "sql_execute",
        "description": "Write workspace cards/decks with INSERT, UPDATE or DELETE; reads require sql_query. front_text is only a question, back_text its answer. New cards need a tag; reuse existing tags and check duplicates with sql_query. Atomic batches allow 50 semicolon-separated statements, 100 affected rows each; never mix reads and writes. Arrays use ('a', 'b'), or () to clear. RETURNING * or columns returns affected rows. Filter by tag in UPDATE and DELETE with tags OVERLAP ('tag'), because UNNEST is only available in SELECT. Read get_guide(card_authoring) before authoring, sql_dialect for grammar/examples and bulk_authoring for large writes.",
        "parameters": {
          "type": "object",
          "properties": {
            "sql": {
              "type": "string",
              "minLength": 1,
              "description": "One or more write statements in the published lingvichr SQL dialect (INSERT, UPDATE, DELETE)."
            },
            "workspaceId": {
              "description": "Workspace UUID from list_workspaces; omit for the selected default.",
              "type": "string",
              "format": "uuid",
              "pattern": "^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$"
            }
          },
          "required": [
            "sql"
          ],
          "additionalProperties": false
        },
        "strict": false
      },
      {
        "type": "function",
        "name": "list_workspaces",
        "description": "Lists accessible workspaces with IDs, names, card counts, activity and isSelected. Pass a returned workspaceId to other tools; omission uses the selected default.",
        "parameters": {
          "type": "object",
          "properties": {},
          "additionalProperties": false
        },
        "strict": false
      },
      {
        "type": "function",
        "name": "get_guide",
        "description": "Static guides: sql_dialect (grammar/limits/examples), card_authoring (content/tags/duplicates/formatting/links), bulk_authoring (batches/recovery/verification), review_flow (review/rating). Read card_authoring before writes; sql_dialect after syntax errors. No workspace access or writes.",
        "parameters": {
          "type": "object",
          "properties": {
            "topic": {
              "type": "string",
              "enum": [
                "sql_dialect",
                "card_authoring",
                "bulk_authoring",
                "review_flow"
              ],
              "description": "Guide topic."
            }
          },
          "required": [
            "topic"
          ],
          "additionalProperties": false
        },
        "strict": false
      },
      {
        "type": "function",
        "name": "next_review_card",
        "description": "Read one eligible cardId/frontText or card:null, without answer, reservation, scheduling or grading. Server-time app queue order: due cards reviewed within an hour, other due cards, then new cards. Narrow with tags or deckId (mutually exclusive). Wait for the learner before reveal_answer.",
        "parameters": {
          "type": "object",
          "properties": {
            "workspaceId": {
              "description": "Workspace UUID; omit for selected. Keep fixed through review/retries.",
              "type": "string",
              "format": "uuid",
              "pattern": "^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$"
            },
            "tags": {
              "description": "Match any existing workspace tag, case-insensitively. Unknown tags return 400; [] matches nothing. Cannot combine with deckId.",
              "maxItems": 100,
              "type": "array",
              "items": {
                "type": "string",
                "minLength": 1
              }
            },
            "deckId": {
              "description": "Saved deck UUID (tag filter); no deck tags means all cards. Cannot combine with tags.",
              "type": "string",
              "format": "uuid",
              "pattern": "^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$"
            }
          },
          "additionalProperties": false
        },
        "strict": false
      },
      {
        "type": "function",
        "name": "reveal_answer",
        "description": "Read backText after the learner attempts the card. No review is submitted. Keep workspaceId/cardId fixed through submission.",
        "parameters": {
          "type": "object",
          "properties": {
            "workspaceId": {
              "description": "Workspace UUID; omit for selected. Keep fixed through review/retries.",
              "type": "string",
              "format": "uuid",
              "pattern": "^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$"
            },
            "cardId": {
              "type": "string",
              "format": "uuid",
              "pattern": "^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$",
              "description": "The cardId returned by next_review_card."
            }
          },
          "required": [
            "cardId"
          ],
          "additionalProperties": false
        },
        "strict": false
      },
      {
        "type": "function",
        "name": "submit_review",
        "description": "Atomically record a rating and advance the authoritative FSRS schedule. Agent assesses recall, explains gaps and announces rating before automatic submission; no rating confirmation. Server stamps online review time; no history import. Returns schedule, no card text/editable memory state. Retry same reviewId: 409 REVIEW_EVENT_CONFLICT with schedule, no duplicate. Same ID on another card: 409 REVIEW_ID_CARD_MISMATCH, no write.",
        "parameters": {
          "type": "object",
          "properties": {
            "workspaceId": {
              "description": "Workspace UUID; omit for selected. Keep fixed through review/retries.",
              "type": "string",
              "format": "uuid",
              "pattern": "^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$"
            },
            "cardId": {
              "type": "string",
              "format": "uuid",
              "pattern": "^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$",
              "description": "The cardId returned by next_review_card."
            },
            "reviewId": {
              "type": "string",
              "format": "uuid",
              "pattern": "^([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$",
              "description": "Persist this client-generated UUID before sending; reuse it on retries of this card's review to prevent duplicate reviews."
            },
            "rating": {
              "type": "string",
              "enum": [
                "Again",
                "Hard",
                "Good",
                "Easy"
              ],
              "description": "Agent-assessed or manual learner rating: Again=failed recall; Hard=correct but difficult; Good=correct; Easy=complete and effortless. Default to Good if effort is unclear. Spoken aliases require learner agreement."
            },
            "reviewedTimeZone": {
              "type": "string",
              "description": "Learner's IANA timezone, e.g. Europe/Sofia, for the local streak/progress day."
            }
          },
          "required": [
            "cardId",
            "reviewId",
            "rating",
            "reviewedTimeZone"
          ],
          "additionalProperties": false
        },
        "strict": false
      },
      {
        "type": "function",
        "name": "get_usage_limits",
        "description": "Reads the account's plan, monthly AI limits and usage. Use for plan/remaining-usage questions, before AI-heavy work or after AI_LIMIT_REACHED. No arguments, card access or writes. null aiMonthlyMessages means uncapped, not zero.",
        "parameters": {
          "type": "object",
          "properties": {},
          "additionalProperties": false
        },
        "strict": false
      }
    ])
}
pub(super) async fn tier(state: &AppState, user: Uuid) -> Result<String, ApiError> {
    usage::tier(state, user).await
}
pub(super) async fn entitlement(state: &AppState, user: Uuid) -> Result<Value, ApiError> {
    usage::refresh(state, user).await
}
pub(super) async fn usage(state: &AppState, user: Uuid) -> Result<Value, ApiError> {
    usage::usage(state, user).await
}
pub(super) async fn assert_allowance(state: &AppState, user: Uuid) -> Result<(), ApiError> {
    usage::assert_allowance(state, user).await
}

fn guides() -> Result<Value, ApiError> {
    serde_json::from_str(include_str!("tools/guides.json")).map_err(|_| ApiError::internal())
}
fn validate(name: &str, args: &Value) -> Result<(), ApiError> {
    let definition = definitions();
    let tool = definition
        .as_array()
        .and_then(|tools| {
            tools
                .iter()
                .find(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
        })
        .ok_or_else(|| ApiError::bad_request("Unknown tool"))?;
    let properties = tool
        .get("parameters")
        .and_then(|params| params.get("properties"))
        .and_then(Value::as_object)
        .ok_or_else(ApiError::internal)?;
    let object = args
        .as_object()
        .ok_or_else(|| ApiError::bad_request("Tool arguments must be an object"))?;
    if let Some(extra) = object.keys().find(|key| !properties.contains_key(*key)) {
        return Err(
            if name.ends_with("review") || matches!(name, "next_review_card" | "reveal_answer") {
                review::invalid(format!("Unrecognized key: {extra}"))
            } else {
                ApiError::bad_request(format!("Unrecognized key: {extra}"))
            },
        );
    }
    if let Some(required) = tool
        .get("parameters")
        .and_then(|params| params.get("required"))
        .and_then(Value::as_array)
    {
        for name in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(name) {
                return Err(
                    if matches!(name, "cardId" | "reviewId" | "rating" | "reviewedTimeZone") {
                        review::invalid(format!("{name}: Required"))
                    } else {
                        ApiError::bad_request(format!("{name}: Required"))
                    },
                );
            }
        }
    }
    Ok(())
}

pub(super) async fn execute(
    state: &AppState,
    user: Uuid,
    workspace: Option<Uuid>,
    name: &str,
    args: &Value,
    timezone: &str,
) -> Result<Value, ApiError> {
    let actor = ToolActor {
        kind: "ai_chat".into(),
        key: "web:chat".into(),
        platform: "web".into(),
        app_version: "ai-chat:web:chat".into(),
    };
    let workspace = if workspace.is_some()
        || matches!(name, "list_workspaces" | "get_guide" | "get_usage_limits")
    {
        workspace
    } else {
        Some(workspaces::resolve_workspace(state, &user.to_string(), None).await?)
    };
    execute_with_actor(state, user, workspace, name, args, timezone, &actor).await
}

#[allow(
    clippy::too_many_arguments,
    reason = "The registry seam carries the authenticated actor and workspace independently from untrusted tool arguments."
)]
pub(super) async fn execute_with_actor(
    state: &AppState,
    user: Uuid,
    selected: Option<Uuid>,
    name: &str,
    args: &Value,
    _timezone: &str,
    actor: &ToolActor,
) -> Result<Value, ApiError> {
    validate(name, args)?;
    if actor.platform != "web"
        || !matches!(actor.kind.as_str(), "ai_chat" | "agent_connection")
        || actor.key.is_empty()
        || actor.app_version.is_empty()
    {
        return Err(ApiError::internal());
    }
    if name == "get_guide" {
        let topic = args
            .get("topic")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::bad_request("topic is required"))?;
        let guides = guides()?;
        let guide = guides
            .get("bodies")
            .and_then(|bodies| bodies.get(topic))
            .ok_or_else(|| ApiError::bad_request("Unknown guide topic"))?;
        return Ok(
            json!({"data":{"topic":topic,"guide":guide},"instructions":guides.get("instructions")}),
        );
    }
    if name == "get_usage_limits" {
        return Ok(
            json!({"data":usage(state,user).await?,"instructions":guides()?.get("usageInstructions")}),
        );
    }
    if name == "list_workspaces" {
        let mut data = workspaces::list_workspaces(state, &user.to_string()).await?;
        if let Some(rows) = data.get_mut("workspaces").and_then(Value::as_array_mut) {
            for row in rows {
                let is_selected = row
                    .get("workspaceId")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<Uuid>().ok())
                    .is_some_and(|id| Some(id) == selected);
                if let Some(object) = row.as_object_mut() {
                    object.insert("isSelected".into(), json!(is_selected));
                }
            }
        }
        return Ok(
            json!({"data":data,"instructions":"These are the workspaces you can access. Each workspace has a workspaceId, name, cardCount (active cards), lastActivityAt (most recent card edit or review, or null), and isSelected (your current default). To target a specific one, pass its workspaceId to any workspace-scoped tool; the isSelected workspace is used by default when you omit workspaceId. Prefer the most active workspace (highest cardCount or most recent lastActivityAt) when the user has not told you which to use."}),
        );
    }
    let explicit = args
        .get("workspaceId")
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| ApiError::bad_request("workspaceId must be a UUID"))?
                .trim()
                .parse::<Uuid>()
                .map_err(|_| ApiError::bad_request("workspaceId must be a UUID"))
        })
        .transpose()?;
    let workspace = explicit.or(selected).ok_or_else(|| {
        ApiError::new(
            StatusCode::CONFLICT,
            "WORKSPACE_SELECTION_REQUIRED",
            "Select a workspace before using this endpoint",
        )
    })?;
    let workspace =
        workspaces::resolve_workspace(state, &user.to_string(), Some(workspace)).await?;
    if matches!(name, "sql_query" | "sql_execute") {
        let input = args
            .get("sql")
            .and_then(Value::as_str)
            .map(|value| value.trim_matches(crate::core::cards::js_space))
            .filter(|value| !value.is_empty())
            .ok_or_else(|| dialect::invalid("sql must not be empty"))?;
        return sql::execute(state, user, workspace, input, name == "sql_execute", actor).await;
    }
    let data = match name {
        "next_review_card" => review::next(state, user, workspace, args).await?,
        "reveal_answer" => review::reveal(state, user, workspace, args).await?,
        "submit_review" => review::submit(state, user, workspace, args, actor).await?,
        _ => return Err(ApiError::bad_request("Unknown tool")),
    };
    Ok(json!({"data":data,"instructions":guides()?.get("reviewInstructions")}))
}
