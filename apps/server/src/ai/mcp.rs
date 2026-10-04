//! Stateless local MCP uses human-managed agent keys, never browser cookies or provider tokens.
use super::{
    admin::{self, AgentConnection},
    tools,
};
use crate::{AppState, error::ApiError};
use axum::{
    Json, Router,
    body::{Bytes, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse as _, Response},
    routing::any,
};
use serde_json::{Map, Value, json};
use std::time::Instant;
use url::Url;
use uuid::Uuid;

const VERSIONS: &[&str] = &[
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];
const INSTRUCTIONS: &str = "Call list_workspaces first to pick a workspaceId, or omit it for the selected default. Then use sql_query for reads and sql_execute for authoring writes. To review, call next_review_card, then reveal_answer, then submit_review. Call get_guide for detail, and get_usage_limits for the plan tier, its limits and this month's AI usage. Hard rules: front_text is a question and never the answer; every new card needs at least one tag; reuse existing workspace tags; check for duplicates with sql_query before creating; describe broad deletes or updates before running them. The dialect is not full PostgreSQL. Published resources, already workspace-scoped: workspace, cards, decks, review_events. A deck is a saved tag filter, so a card has no deck_id and belongs to a deck only by matching tags. get_guide topics: sql_dialect for the grammar, limits, and examples; card_authoring for the card contract, formatting, and a card's web link; bulk_authoring for splitting and verifying a large write job; review_flow for the review loop.";

pub(super) fn router() -> Router<AppState> {
    Router::new().route("/v1/mcp", any(handle))
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let mut words = header_text(headers, "authorization")?.split_whitespace();
    let scheme = words.next()?;
    let token = words.next()?;
    (scheme.eq_ignore_ascii_case("Bearer") && words.next().is_none()).then_some(token)
}

fn json_response(status: StatusCode, value: Value) -> Response {
    let characters = value.to_string().encode_utf16().count();
    let mut response = (status, Json(value)).into_response();
    response
        .extensions_mut()
        .insert(ResponseCharacters(characters));
    response
}

#[derive(Clone)]
struct ResponseCharacters(usize);

fn challenge() -> Response {
    let mut response = json_response(
        StatusCode::UNAUTHORIZED,
        json!({"error":"A valid local MCP agent key is required"}),
    );
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

fn rpc_error(id: &Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message.into()}})
}

fn transport_error(status: StatusCode, code: i64, message: impl Into<String>) -> Response {
    json_response(status, rpc_error(&Value::Null, code, message))
}

async fn authorize(state: &AppState, headers: &HeaderMap) -> Result<AgentConnection, Response> {
    let Some(owner) = state.config.local_mcp_user_id else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "NOT_FOUND", "Not found").into_response());
    };
    if header_text(headers, "origin").is_some_and(|origin| origin != state.config.backend_origin) {
        return Err(json_response(
            StatusCode::FORBIDDEN,
            json!({"error":"Invalid origin"}),
        ));
    }
    let expected_host = Url::parse(&state.config.backend_origin)
        .ok()
        .and_then(|url| {
            url.host_str().map(|host| {
                url.port()
                    .map_or_else(|| host.to_owned(), |port| format!("{host}:{port}"))
            })
        });
    if !header_text(headers, "host")
        .zip(expected_host.as_deref())
        .is_some_and(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
    {
        return Err(json_response(
            StatusCode::FORBIDDEN,
            json!({"error":"Invalid host"}),
        ));
    }
    let token = bearer(headers).ok_or_else(challenge)?;
    match admin::authenticate(&state.pool, token).await {
        Ok(connection) if connection.user == owner => Ok(connection),
        Ok(_) => Err(challenge()),
        Err(error) if error.status == StatusCode::UNAUTHORIZED => Err(challenge()),
        Err(error) => Err(error.into_response()),
    }
}

async fn handle(State(state): State<AppState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let method = parts.method;
    let headers = parts.headers;
    let request_id = header_text(&headers, "x-request-id")
        .filter(|value| !value.is_empty())
        .map_or_else(|| Uuid::new_v4().to_string(), str::to_owned);
    let started = Instant::now();
    let mut invoked = None;
    let mut response = match authorize(&state, &headers).await {
        Err(response) => response,
        Ok(connection) => {
            let mut bytes = Bytes::new();
            let response = if method == Method::POST {
                if let Err(response) = validate_transport(&headers) {
                    *response
                } else {
                    match to_bytes(body, 4_194_304).await {
                        Ok(received) => {
                            bytes = received;
                            dispatch(
                                &state,
                                &headers,
                                &bytes,
                                &connection,
                                &request_id,
                                &mut invoked,
                            )
                            .await
                        }
                        Err(_) => transport_error(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            -32000,
                            "Request body is too large",
                        ),
                    }
                }
            } else {
                let mut response = json_response(
                    StatusCode::METHOD_NOT_ALLOWED,
                    json!({"error":"Only POST is supported"}),
                );
                response
                    .headers_mut()
                    .insert(header::ALLOW, HeaderValue::from_static("POST"));
                response
            };
            let body_tool = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|body| {
                    body.get("params")
                        .and_then(|params| params.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                });
            tracing::info!(
                event = "mcp_request", request_id, http_method = %method, user_id = %connection.user,
                workspace_id = ?connection.selected, connection_id = %connection.id,
                caller = ?telemetry_value(header_text(&headers, "user-agent")),
                protocol_version = ?telemetry_value(header_text(&headers, "mcp-protocol-version")),
                json_rpc_method = ?telemetry_value(header_text(&headers, "mcp-method")),
                tool_name = ?invoked.as_ref().or(body_tool.as_ref()),
                tool_executed = ?invoked.as_ref().or(body_tool.as_ref()).map(|_| invoked.is_some()),
                status_code = response.status().as_u16(), duration_ms = started.elapsed().as_millis(),
                response_chars = ?response.extensions().get::<ResponseCharacters>().map(|characters| characters.0),
                "Private MCP request completed"
            );
            response
        }
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}

fn telemetry_value(value: Option<&str>) -> Option<String> {
    value
        .map(|text| {
            text.chars()
                .filter(|character| !character.is_control())
                .take(256)
                .collect::<String>()
        })
        .filter(|text| !text.is_empty())
}

fn valid_id(value: &Value) -> bool {
    value.is_string() || value.as_i64().is_some() || value.as_u64().is_some()
}

fn valid_message(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return false;
    }
    if object.get("method").is_some_and(Value::is_string) {
        object
            .keys()
            .all(|key| matches!(key.as_str(), "jsonrpc" | "id" | "method" | "params"))
            && object.get("id").is_none_or(valid_id)
            && object.get("params").is_none_or(Value::is_object)
    } else {
        object.get("id").is_some_and(valid_id)
            && (object.get("result").is_some_and(Value::is_object)
                && object
                    .keys()
                    .all(|key| matches!(key.as_str(), "jsonrpc" | "id" | "result"))
                || object.get("error").is_some_and(|error| {
                    error.get("code").and_then(Value::as_i64).is_some()
                        && error.get("message").is_some_and(Value::is_string)
                }) && object
                    .keys()
                    .all(|key| matches!(key.as_str(), "jsonrpc" | "id" | "error")))
    }
}

fn parse_messages(bytes: &[u8]) -> Result<Vec<Value>, Box<Response>> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| {
        Box::new(transport_error(
            StatusCode::BAD_REQUEST,
            -32700,
            "Parse error: Invalid JSON",
        ))
    })?;
    let messages = match value {
        Value::Array(messages) => messages,
        message => vec![message],
    };
    if !messages.iter().all(valid_message) {
        return Err(Box::new(transport_error(
            StatusCode::BAD_REQUEST,
            -32700,
            "Parse error: Invalid JSON-RPC message",
        )));
    }
    if messages.len() > 1
        && messages
            .iter()
            .any(|message| message.get("method").and_then(Value::as_str) == Some("initialize"))
    {
        return Err(Box::new(transport_error(
            StatusCode::BAD_REQUEST,
            -32600,
            "Invalid Request: Only one initialization request is allowed",
        )));
    }
    Ok(messages)
}

fn validate_transport(headers: &HeaderMap) -> Result<(), Box<Response>> {
    if !header_text(headers, "accept").is_some_and(|accept| {
        accept.contains("application/json") && accept.contains("text/event-stream")
    }) {
        return Err(Box::new(transport_error(
            StatusCode::NOT_ACCEPTABLE,
            -32000,
            "Not Acceptable: Client must accept both application/json and text/event-stream",
        )));
    }
    if !header_text(headers, "content-type").is_some_and(|value| {
        value
            .split(';')
            .next()
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
    }) {
        return Err(Box::new(transport_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            -32000,
            "Unsupported Media Type: Content-Type must be application/json",
        )));
    }
    Ok(())
}

async fn dispatch(
    state: &AppState,
    headers: &HeaderMap,
    bytes: &[u8],
    connection: &AgentConnection,
    request_id: &str,
    invoked: &mut Option<String>,
) -> Response {
    if let Err(response) = validate_transport(headers) {
        return *response;
    }
    let messages = match parse_messages(bytes) {
        Ok(messages) => messages,
        Err(response) => return *response,
    };
    let initializing = messages
        .iter()
        .any(|message| message.get("method").and_then(Value::as_str) == Some("initialize"));
    if !initializing
        && let Some(version) = header_text(headers, "mcp-protocol-version")
        && !VERSIONS.contains(&version)
    {
        return transport_error(
            StatusCode::BAD_REQUEST,
            -32000,
            format!(
                "Bad Request: Unsupported protocol version: {version} (supported versions: {})",
                VERSIONS.join(", ")
            ),
        );
    }
    let mut responses = Vec::new();
    for message in messages {
        if message.get("method").is_none() || message.get("id").is_none() {
            continue;
        }
        responses.push(call(state, connection, &message, request_id, invoked).await);
    }
    match responses.len() {
        0 => StatusCode::ACCEPTED.into_response(),
        1 => json_response(
            StatusCode::OK,
            responses.into_iter().next().unwrap_or(Value::Null),
        ),
        _ => json_response(StatusCode::OK, Value::Array(responses)),
    }
}

fn initialize(state: &AppState, params: &Value) -> Result<Value, &'static str> {
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or("Invalid initialize protocolVersion")?;
    if !params.get("capabilities").is_some_and(Value::is_object)
        || !params.get("clientInfo").is_some_and(|info| {
            info.get("name").is_some_and(Value::is_string)
                && info.get("version").is_some_and(Value::is_string)
        })
    {
        return Err("Invalid initialize parameters");
    }
    Ok(
        json!({"protocolVersion":if VERSIONS.contains(&version) {version} else {"2025-11-25"}, "capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"flashcards-open-source-app","version":"v1","title":"lingvichr","websiteUrl":state.config.backend_origin,"icons":[{"src":format!("{}/icon.svg",state.config.backend_origin),"mimeType":"image/svg+xml","sizes":["any"]}]},"instructions":INSTRUCTIONS}),
    )
}

async fn call(
    state: &AppState,
    connection: &AgentConnection,
    request: &Value,
    request_id: &str,
    invoked: &mut Option<String>,
) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let result = match request.get("method").and_then(Value::as_str) {
        Some("initialize") => match initialize(state, &params) {
            Ok(result) => result,
            Err(message) => return rpc_error(&id, -32602, message),
        },
        Some("ping") => json!({}),
        Some("tools/list") => json!({"tools":tool_list()}),
        Some("tools/call") => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return rpc_error(&id, -32602, "Invalid tools/call parameters");
            };
            if !tool_list()
                .iter()
                .any(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
            {
                return json!({"jsonrpc":"2.0","id":id,"result":{"isError":true,"content":[{"type":"text","text":format!("Tool {name} not found")}]}});
            }
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !arguments.is_object() {
                return rpc_error(&id, -32602, "Tool arguments must be an object");
            }
            *invoked = Some(name.to_owned());
            let actor = tools::ToolActor {
                kind: "agent_connection".to_owned(),
                key: connection.id.to_string(),
                platform: "web".to_owned(),
                app_version: format!("agent:{}", connection.id),
            };
            match tools::execute_with_actor(
                state,
                connection.user,
                connection.selected,
                name,
                &arguments,
                "UTC",
                &actor,
            )
            .await
            {
                Ok(value) => tool_result(state, &value),
                Err(error) => tool_error(state, connection, name, error, request_id).await,
            }
        }
        _ => return rpc_error(&id, -32601, "Method not found"),
    };
    json!({"jsonrpc":"2.0","id":id,"result":result})
}

fn docs(state: &AppState) -> Value {
    let repository = "https://github.com/kirill-markin/flashcards-open-source-app";
    json!({"discoveryUrl":format!("{}/v1/",state.config.backend_origin),"source":{"repositoryUrl":repository,"agentRoutesUrl":format!("{repository}/tree/main/apps/backend/src/routes"),"authRoutesUrl":format!("{repository}/tree/main/apps/auth/src/routes/agent")}})
}

fn tool_result(state: &AppState, value: &Value) -> Value {
    let data = value.get("data").cloned().unwrap_or(Value::Null);
    let envelope = json!({"ok":true,"data":data,"instructions":value.get("instructions").cloned().unwrap_or(Value::Null),"docs":docs(state)});
    json!({"content":[{"type":"text","text":envelope.to_string()}],"structuredContent":{"data":data}})
}

async fn tool_error(
    state: &AppState,
    connection: &AgentConnection,
    name: &str,
    mut error: ApiError,
    request_id: &str,
) -> Value {
    if error.code == "WORKSPACE_SELECTION_REQUIRED" {
        let actor = tools::ToolActor {
            kind: "agent_connection".to_owned(),
            key: connection.id.to_string(),
            platform: "web".to_owned(),
            app_version: format!("agent:{}", connection.id),
        };
        match tools::execute_with_actor(
            state,
            connection.user,
            connection.selected,
            "list_workspaces",
            &json!({}),
            "UTC",
            &actor,
        )
        .await
        {
            Ok(value) => {
                let mut details = error
                    .details
                    .take()
                    .and_then(|value| value.as_object().cloned())
                    .unwrap_or_default();
                if let Some(workspaces) = value.get("data").and_then(|data| data.get("workspaces"))
                {
                    details.insert("workspaces".to_owned(), workspaces.clone());
                }
                error.details = Some(Value::Object(details));
            }
            Err(enrichment) => tracing::warn!(
                code = enrichment.code,
                request_id,
                "MCP workspace selection enrichment failed"
            ),
        }
    }
    let mut envelope = json!({"ok":false,"data":{},"instructions":remediation(&error,name),"docs":docs(state),"error":{"code":error.code,"message":error.message},"requestId":request_id});
    if let Some(details) = error.details
        && let Some(body) = envelope.get_mut("error").and_then(Value::as_object_mut)
    {
        body.insert("details".to_owned(), details);
    }
    json!({"isError":true,"content":[{"type":"text","text":envelope.to_string()}]})
}

fn remediation(error: &ApiError, name: &str) -> String {
    match error.code.as_str() {
        "QUERY_INVALID_SQL" | "QUERY_UNSUPPORTED_SYNTAX" => format!("Fix the sql string using error.message and any error.details.validationIssues, then if the sql string mixes reads and writes, split it into separate calls with reads to sql_query and writes to sql_execute; otherwise call the tool error.message says to use instead if it names one, or else call the {name} tool again. If the dialect itself is unclear, call get_guide with topic sql_dialect first instead of guessing."),
        "WORKSPACE_SELECTION_REQUIRED" => format!("This connection has no selected workspace. Call the list_workspaces tool to see the workspaces you can access (also embedded under error.details.workspaces when available), then call the {name} tool again with the workspaceId argument set to the one you want."),
        "REVIEW_STALE" => "The card's stored review time is at or after the current server time, so the scheduler cannot move forward from it. Reloading the card does not clear that; explain the conflict and review another card instead of submitting a rating for this one.".to_owned(),
        "REVIEW_EVENT_CONFLICT" => "This review was already recorded, so nothing was stored again. Read the card's current schedule from error.details.reviewSchedule and move on; use a new reviewId only for a new learner review.".to_owned(),
        "REVIEW_ID_CARD_MISMATCH" => "This reviewId already identifies a recorded review of a different card, so nothing was stored for the card you submitted and no schedule advanced. Generate a new reviewId for this review and submit it again; reuse a reviewId only to retry the same card's submission.".to_owned(),
        "DATABASE_COMMIT_OUTCOME_UNKNOWN" if name == "submit_review" => "Retry submit_review with the identical workspaceId, reviewId, rating, and cardId. Do not advance until the result is confirmed.".to_owned(),
        "DATABASE_COMMIT_OUTCOME_UNKNOWN" => format!("The previous mutation's outcome could not be confirmed. Do not blindly re-run it: first call sql_query with a SELECT to check whether the change already applied, and only call the {name} tool again if the change is confirmed absent."),
        "SERVICE_UNAVAILABLE" => format!("The service is temporarily unavailable. Retry the same {name} tool call after a short delay without changing the request."),
        _ if error.status.is_server_error() => format!("Retry the {name} tool once; if it fails again treat it as a server-side error and stop changing the request."),
        _ => format!("Fix the request using error.message and any error.details.validationIssues, then call the {name} tool again."),
    }
}

fn presentation(name: &str) -> (&str, bool, bool) {
    match name {
        "sql_query" => ("lingvichr SQL query (read-only)", true, true),
        "sql_execute" => ("lingvichr SQL execute (write)", false, false),
        "list_workspaces" => ("List flashcards workspaces", true, true),
        "get_guide" => ("Get flashcards usage guide", true, true),
        "next_review_card" => ("Next flashcard question", true, true),
        "reveal_answer" => ("Reveal flashcard answer", true, true),
        "submit_review" => ("Submit flashcard review", false, true),
        "get_usage_limits" => ("Get AI usage and limits", true, true),
        _ => (name, true, true),
    }
}

fn tool_list() -> Vec<Value> {
    tools::definitions().as_array().into_iter().flatten().filter_map(|definition| {
        let name = definition.get("name").and_then(Value::as_str)?;
        let (title, readonly, idempotent) = presentation(name);
        let mut tool = json!({"name":name,"title":title,"description":definition.get("description"),"inputSchema":definition.get("parameters"),"outputSchema":output_schema(name),"annotations":{"title":title,"readOnlyHint":readonly,"destructiveHint":!readonly,"openWorldHint":false}});
        if idempotent && let Some(hints) = tool.get_mut("annotations").and_then(Value::as_object_mut) { hints.insert("idempotentHint".to_owned(),Value::Bool(true)); }
        if matches!(name,"sql_query"|"sql_execute") && let Some(object) = tool.as_object_mut() { object.insert("_meta".to_owned(),json!({"anthropic/maxResultSizeChars":48000})); }
        Some(tool)
    }).collect()
}

fn object_schema(fields: &[(&str, Value)]) -> Value {
    let properties: Map<String, Value> = fields
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect();
    json!({"type":"object","properties":properties,"required":fields.iter().map(|(key,_)| *key).collect::<Vec<_>>(),"additionalProperties":false})
}

fn output_schema(name: &str) -> Value {
    let string = json!({"type":"string"});
    let number = json!({"type":"number"});
    let boolean = json!({"type":"boolean"});
    let optional_number = json!({"type":["number","null"]});
    let optional_string = json!({"type":["string","null"]});
    let workspace = ("workspaceId", string.clone());
    let card = ("cardId", string.clone());
    let data = match name {
        "list_workspaces" => object_schema(&[(
            "workspaces",
            json!({"type":"array","items":object_schema(&[workspace,("name",string.clone()),("createdAt",string),("isSelected",boolean),("cardCount",number),("lastActivityAt",optional_string)])}),
        )]),
        "get_guide" => object_schema(&[
            (
                "topic",
                json!({"enum":["sql_dialect","card_authoring","bulk_authoring","review_flow"]}),
            ),
            ("guide", string),
        ]),
        "next_review_card" => object_schema(&[
            workspace,
            (
                "card",
                json!({"anyOf":[object_schema(&[card,("frontText",string)]),{"type":"null"}]}),
            ),
        ]),
        "reveal_answer" => object_schema(&[workspace, card, ("backText", string)]),
        "submit_review" => object_schema(&[
            workspace,
            card,
            ("reviewId", string.clone()),
            ("reviewEventId", string.clone()),
            ("rating", json!({"enum":["Again","Hard","Good","Easy"]})),
            ("reviewedAt", string.clone()),
            ("dueAt", string),
            ("intervalSeconds", number.clone()),
            ("scheduledDays", number.clone()),
            (
                "state",
                json!({"enum":["new","learning","review","relearning"]}),
            ),
            ("reps", number.clone()),
            ("lapses", number),
        ]),
        "get_usage_limits" => object_schema(&[
            ("accountKind", json!({"enum":["account","guest"]})),
            (
                "entitlement",
                object_schema(&[
                    ("tier", json!({"enum":["free","premium","lifetime"]})),
                    ("tierRank", number.clone()),
                    ("tierDisplayName", string.clone()),
                    ("status", json!({"enum":["none","active","in_grace"]})),
                    ("until", optional_string),
                    ("isTrial", boolean.clone()),
                    ("willRenew", boolean),
                    (
                        "limits",
                        object_schema(&[
                            ("aiMonthlyMessages", optional_number.clone()),
                            ("aiMonthlyWeightedTokens", json!({"type":"null"})),
                        ]),
                    ),
                ]),
            ),
            (
                "usage",
                object_schema(&[
                    ("monthStartsAt", string.clone()),
                    ("monthEndsAt", string),
                    ("usedMessages", number.clone()),
                    ("remainingMessages", optional_number),
                    ("ownKeyMessages", number.clone()),
                    ("usedWeightedTokens", number.clone()),
                    ("remainingWeightedTokens", json!({"type":"null"})),
                    ("weightedOutputTokenMultiplier", number),
                ]),
            ),
        ]),
        "sql_query" | "sql_execute" => sql_output_schema(name == "sql_query"),
        _ => json!({"type":"object"}),
    };
    object_schema(&[("data", data)])
}

fn sql_output_schema(readonly: bool) -> Value {
    let string = json!({"type":"string"});
    let number = json!({"type":"number"});
    let boolean = json!({"type":"boolean"});
    let resource = json!({"enum":if readonly {json!(["workspace","cards","decks","review_events",null])} else {json!(["cards","decks"])} });
    let rows = json!({"type":"array","items":{"type":"object","additionalProperties":true}});
    let mut fields = vec![
        (
            "statementType",
            json!({"enum":if readonly {json!(["show_tables","describe","select"])} else {json!(["insert","update","delete"])} }),
        ),
        ("resource", resource),
        ("rows", rows),
    ];
    if readonly {
        fields.extend([
            ("rowCount", number.clone()),
            ("totalRowCount", number.clone()),
            ("rowsTruncated", boolean.clone()),
            ("limit", json!({"type":["number","null"]})),
            ("offset", json!({"type":["number","null"]})),
            ("hasMore", boolean.clone()),
        ]);
    } else {
        fields.push(("affectedCount", number.clone()));
    }
    let mut statement = object_schema(&fields);
    if let Some(object) = statement.as_object_mut() {
        object.insert("additionalProperties".to_owned(), Value::Bool(true));
    }
    let mut batch = object_schema(&[
        ("statementType", json!({"const":"batch"})),
        ("resource", json!({"type":"null"})),
        ("statementCount", number.clone()),
        ("statements", json!({"type":"array","items":statement})),
        (
            "affectedCountTotal",
            if readonly {
                json!({"type":"null"})
            } else {
                number
            },
        ),
        ("rowsOmitted", boolean.clone()),
        ("sqlOmitted", boolean.clone()),
    ]);
    if let Some(object) = batch.as_object_mut() {
        object.insert("additionalProperties".to_owned(), Value::Bool(true));
    }
    if !readonly {
        fields.extend([("rowsOmitted", boolean.clone()), ("sqlOmitted", boolean)]);
        statement = object_schema(&fields);
        if let Some(object) = statement.as_object_mut() {
            object.insert("additionalProperties".to_owned(), Value::Bool(true));
        }
    }
    let submitted = object_schema(&[
        ("sql", string.clone()),
        ("normalizedSql", string),
        ("workspaceId", json!({"type":"string"})),
    ]);
    let mut submitted = submitted;
    if let Some(object) = submitted.as_object_mut() {
        object.insert("additionalProperties".to_owned(), Value::Bool(true));
    }
    // Each SQL result is one statement or a batch, plus the submitted SQL context.
    json!({"allOf":[submitted,{"anyOf":[statement,batch]}]})
}
