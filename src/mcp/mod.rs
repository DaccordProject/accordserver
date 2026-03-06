mod tools;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::state::AppState;

const MCP_PROTOCOL_VERSION: &str = "2025-03-26";

/// JSON-RPC 2.0 request envelope.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<Value>,
}

/// JSON-RPC 2.0 response envelope.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Option<Value>, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

/// MCP endpoint handler. Validates the API key and dispatches JSON-RPC methods.
pub async fn handle_mcp(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<JsonRpcRequest>,
) -> Response {
    // Validate API key
    let mcp_key = match &state.mcp_api_key {
        Some(key) => key,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(JsonRpcResponse::error(
                    req.id,
                    -32000,
                    "MCP endpoint is not enabled (MCP_API_KEY not configured)",
                )),
            )
                .into_response();
        }
    };

    let provided_key = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "));

    match provided_key {
        Some(key) if constant_time_eq(key.as_bytes(), mcp_key.as_bytes()) => {}
        _ => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(JsonRpcResponse::error(
                    req.id,
                    -32001,
                    "Invalid or missing MCP API key",
                )),
            )
                .into_response();
        }
    }

    let response = dispatch(&state, &req).await;
    Json(response).into_response()
}

/// Constant-time comparison to prevent timing attacks on the API key.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

async fn dispatch(state: &AppState, req: &JsonRpcRequest) -> JsonRpcResponse {
    match req.method.as_str() {
        "initialize" => handle_initialize(req),
        "notifications/initialized" => {
            // Client acknowledgement — no response needed for notifications,
            // but since this arrives as a POST we return an empty success.
            JsonRpcResponse::success(req.id.clone(), serde_json::json!({}))
        }
        "ping" => JsonRpcResponse::success(req.id.clone(), serde_json::json!({})),
        "tools/list" => handle_tools_list(req),
        "tools/call" => handle_tools_call(state, req).await,
        _ => JsonRpcResponse::error(req.id.clone(), -32601, "Method not found"),
    }
}

fn handle_initialize(req: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(
        req.id.clone(),
        serde_json::json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {
                "tools": {
                    "listChanged": false
                }
            },
            "serverInfo": {
                "name": "accord-mcp",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    )
}

fn handle_tools_list(req: &JsonRpcRequest) -> JsonRpcResponse {
    JsonRpcResponse::success(
        req.id.clone(),
        serde_json::json!({
            "tools": tools::tool_definitions()
        }),
    )
}

async fn handle_tools_call(state: &AppState, req: &JsonRpcRequest) -> JsonRpcResponse {
    let params = match &req.params {
        Some(p) => p,
        None => {
            return JsonRpcResponse::error(req.id.clone(), -32602, "Missing params");
        }
    };

    let tool_name = params
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    match tools::call_tool(state, tool_name, arguments).await {
        Ok(result) => JsonRpcResponse::success(
            req.id.clone(),
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": result
                }]
            }),
        ),
        Err(err) => JsonRpcResponse::success(
            req.id.clone(),
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": err
                }],
                "isError": true
            }),
        ),
    }
}
