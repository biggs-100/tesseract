// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Tesseract Contributors

//! MCP server for Tesseract — lets AI agents search and insert vectors
//! via the Model Context Protocol.
//!
//! ## Usage
//!
//! ```bash
//! TESSERACT_API_URL=http://localhost:3000 cargo run -p tesseract-mcp
//! TESSERACT_API_KEY=sk-abc123 cargo run -p tesseract-mcp
//! ```

use std::sync::Arc;

use rmcp::{
    Error as McpError, RoleServer, ServerHandler,
    model::{
        CallToolRequestParam, CallToolResult, Content, Implementation, ListToolsResult,
        PaginatedRequestParam, ProtocolVersion, ServerCapabilities, ServerInfo, Tool,
    },
    service::{RequestContext, ServiceExt},
    transport::io,
};

// ---------------------------------------------------------------------------
// MCP Server
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct TesseractMcp {
    api_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl TesseractMcp {
    fn new(api_url: String, api_key: Option<String>) -> Self {
        Self {
            api_url,
            api_key,
            client: reqwest::Client::new(),
        }
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.api_url.trim_end_matches('/'), path);
        let mut req = self.client.request(method, &url);
        if let Some(ref key) = self.api_key {
            req = req.header("X-API-Key", key);
        }
        req
    }
}

// ---------------------------------------------------------------------------
// ServerHandler
// ---------------------------------------------------------------------------

impl ServerHandler for TesseractMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::default(),
            capabilities: ServerCapabilities {
                tools: Some(Default::default()),
                ..Default::default()
            },
            server_info: Implementation {
                name: "tesseract-mcp".into(),
                version: "0.1.0".into(),
            },
            instructions: Some(
                "Tesseract vector database MCP server. \
                 Use tesseract_query to search vectors with VQL, \
                 tesseract_insert to add vectors, \
                 and tesseract_status to check server health."
                    .into(),
            ),
        }
    }

    async fn list_tools(
        &self,
        _: PaginatedRequestParam,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let query_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "vql": {
                    "type": "string",
                    "description": "VQL query string, e.g. FIND SIMILARITY(emb, VECTOR(0.1, 0.2)) LIMIT 5"
                }
            },
            "required": ["vql"]
        });

        let insert_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Unique identifier for the vector"
                },
                "vector": {
                    "type": "array",
                    "items": { "type": "number" },
                    "description": "Array of float values"
                },
                "metadata": {
                    "type": "string",
                    "description": "Optional JSON metadata string, e.g. '{\"category\": \"science\"}'"
                }
            },
            "required": ["id", "vector"]
        });

        let status_schema = serde_json::json!({
            "type": "object",
            "properties": {}
        });

        Ok(ListToolsResult {
            tools: vec![
                Tool::new(
                    "tesseract_query",
                    "Search vectors using VQL (Vector Query Language). Supports FIND SIMILARITY, metadata filters, topological bias, and pagination.",
                    Arc::new(query_schema.as_object().unwrap().clone()),
                ),
                Tool::new(
                    "tesseract_insert",
                    "Insert a vector with optional JSON metadata into the Tesseract database. Vectors are immediately searchable via the hot buffer.",
                    Arc::new(insert_schema.as_object().unwrap().clone()),
                ),
                Tool::new(
                    "tesseract_status",
                    "Check the Tesseract server health. Returns liveness and component-level readiness.",
                    Arc::new(status_schema.as_object().unwrap().clone()),
                ),
            ],
            next_cursor: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParam,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match request.name.as_ref() {
            "tesseract_query" => self.exec_query(request.arguments).await,
            "tesseract_insert" => self.exec_insert(request.arguments).await,
            "tesseract_status" => self.exec_status().await,
            name => Err(McpError::invalid_request(format!("unknown tool: {name}"), None)),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

impl TesseractMcp {
    async fn exec_query(&self, args: Option<serde_json::Map<String, serde_json::Value>>) -> Result<CallToolResult, McpError> {
        let params = args.ok_or_else(|| McpError::invalid_params("missing arguments", None))?;

        let vql = params
            .get("vql")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_params("missing 'vql' string argument", None))?
            .to_string();

        let payload = serde_json::json!({ "vql": vql });

        let resp = self
            .request(reqwest::Method::POST, "/query")
            .json(&payload)
            .send()
            .await
            .map_err(|e| McpError::internal_error(format!("connection failed: {e}"), None))?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if status.is_success() {
            Ok(CallToolResult::success(vec![Content::text(body)]))
        } else {
            Ok(CallToolResult::error(vec![Content::text(format!("HTTP {status}: {body}"))]))
        }
    }

    async fn exec_insert(&self, args: Option<serde_json::Map<String, serde_json::Value>>) -> Result<CallToolResult, McpError> {
        let params = args.ok_or_else(|| McpError::invalid_params("missing arguments", None))?;

        let id = params
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McpError::invalid_params("missing 'id' string argument", None))?
            .to_string();

        let vector = params
            .get("vector")
            .and_then(|v| v.as_array())
            .ok_or_else(|| McpError::invalid_params("missing 'vector' array argument", None))?
            .iter()
            .filter_map(|v| v.as_f64())
            .collect::<Vec<_>>();

        let mut payload = serde_json::json!({ "id": id, "vector": vector });

        if let Some(meta_str) = params.get("metadata").and_then(|v| v.as_str()) {
            payload["metadata"] = match serde_json::from_str::<serde_json::Value>(meta_str) {
                Ok(parsed) => parsed,
                Err(_) => serde_json::json!({ "raw": meta_str }),
            };
        }

        let resp = self
            .request(reqwest::Method::POST, "/insert")
            .json(&payload)
            .send()
            .await
            .map_err(|e| McpError::internal_error(format!("connection failed: {e}"), None))?;

        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();

        if status.is_success() {
            Ok(CallToolResult::success(vec![Content::text(format!("ok: vector {id} inserted. response: {body}"))]))
        } else {
            Ok(CallToolResult::error(vec![Content::text(format!("HTTP {status}: {body}"))]))
        }
    }

    async fn exec_status(&self) -> Result<CallToolResult, McpError> {
        let resp = self
            .request(reqwest::Method::GET, "/health/liveness")
            .send()
            .await
            .map_err(|e| McpError::internal_error(format!("connection failed: {e}"), None))?;

        let body = resp.text().await.unwrap_or_default();

        Ok(CallToolResult::success(vec![Content::text(body)]))
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let api_url = std::env::var("TESSERACT_API_URL")
        .unwrap_or_else(|_| "http://localhost:3000".into());
    let api_key = std::env::var("TESSERACT_API_KEY").ok();

    tracing::info!("Starting Tesseract MCP server — API: {api_url}");

    let server = TesseractMcp::new(api_url, api_key);
    let ct = tokio_util::sync::CancellationToken::new();
    let ct_clone = ct.clone();

    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Shutting down MCP server...");
        ct_clone.cancel();
    });

    let (stdin, stdout) = io::stdio();
    server
        .serve_with_ct((stdin, stdout), ct)
        .await?
        .waiting()
        .await?;

    Ok(())
}
