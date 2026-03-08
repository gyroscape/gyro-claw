//! # Web API Server
//!
//! Simple Axum-based HTTP API server for Gyro-Claw.
//! Provides endpoints for running commands and chatting with the agent.
//! This allows a future web UI to interact with the agent.

use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;
// SECURITY FIX: Replaced wildcard CORS with restricted localhost-only origins.
use tower_http::cors::CorsLayer;

use crate::agent::executor::Executor;
use crate::agent::memory::Memory;
use crate::agent::planner::Planner;
use crate::llm::client::LlmClient;
use crate::tools::ToolRegistry;

/// Maximum allowed input length for API request bodies (characters).
/// SECURITY FIX: Prevents memory exhaustion from oversized payloads.
const MAX_INPUT_LENGTH: usize = 10_000;

/// Shared application state for the API server.
pub struct AppState {
    pub planner: Mutex<Planner>,
    pub tool_registry: ToolRegistry,
}

/// Request body for the /run endpoint.
#[derive(Deserialize)]
pub struct RunRequest {
    pub command: String,
}

/// Response body for the /run endpoint.
#[derive(Serialize)]
pub struct RunResponse {
    pub success: bool,
    pub output: String,
}

/// Request body for the /chat endpoint.
#[derive(Deserialize)]
pub struct ChatRequest {
    pub message: String,
}

/// Response body for the /chat endpoint.
#[derive(Serialize)]
pub struct ChatResponse {
    pub reply: String,
}

/// Health check response.
#[derive(Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

/// Start the Axum API server on the given port.
pub async fn start_server(
    llm: LlmClient,
    executor: Executor,
    memory: Memory,
    config: crate::config::Config,
    tool_registry: ToolRegistry,
    port: u16,
) -> anyhow::Result<()> {
    let planner = Planner::new(llm, executor, memory, config);

    let state = Arc::new(AppState {
        planner: Mutex::new(planner),
        tool_registry,
    });

    // SECURITY FIX: Restrict CORS to localhost origins only instead of wildcard `Any`.
    // This prevents any external website from making cross-origin API calls to the agent.
    // Configurable via GYRO_CLAW_CORS_ORIGIN environment variable.
    let allowed_origins = std::env::var("GYRO_CLAW_CORS_ORIGIN")
        .unwrap_or_else(|_| format!("http://localhost:{}", port));
    let cors = CorsLayer::new()
        .allow_origin(
            allowed_origins
                .parse::<axum::http::HeaderValue>()
                .unwrap_or_else(|_| {
                    format!("http://localhost:{}", port)
                        .parse()
                        .expect("static localhost origin must parse")
                }),
        )
        .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
        .allow_headers(tower_http::cors::Any);

    let app = Router::new()
        .route("/run", post(handle_run))
        .route("/chat", post(handle_chat))
        .route("/health", axum::routing::get(handle_health))
        .layer(cors)
        .with_state(state);

    // SECURITY FIX: Bind to 127.0.0.1 (localhost) by default instead of 0.0.0.0.
    // This prevents exposing the agent API to the entire network: critical for a
    // privacy-first tool. Configurable via GYRO_CLAW_HOST environment variable.
    let host = std::env::var("GYRO_CLAW_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let addr = format!("{}:{}", host, port);
    tracing::info!("🚀 Gyro-Claw API server listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Handle POST /run — execute a single command through the agent.
async fn handle_run(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RunRequest>,
) -> Result<Json<RunResponse>, StatusCode> {
    // SECURITY FIX: Reject oversized input to prevent memory exhaustion attacks.
    if req.command.len() > MAX_INPUT_LENGTH {
        return Ok(Json(RunResponse {
            success: false,
            output: format!(
                "Error: Input too large ({} chars). Maximum allowed: {} chars.",
                req.command.len(),
                MAX_INPUT_LENGTH
            ),
        }));
    }

    let mut planner = state.planner.lock().await;

    match planner.run(&req.command, &state.tool_registry).await {
        Ok(output) => Ok(Json(RunResponse {
            success: true,
            output,
        })),
        Err(e) => Ok(Json(RunResponse {
            success: false,
            output: format!("Error: {}", e),
        })),
    }
}

/// Handle POST /chat — chat with the agent.
async fn handle_chat(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ChatRequest>,
) -> Result<Json<ChatResponse>, StatusCode> {
    // SECURITY FIX: Reject oversized input to prevent memory exhaustion attacks.
    if req.message.len() > MAX_INPUT_LENGTH {
        return Ok(Json(ChatResponse {
            reply: format!(
                "Error: Input too large ({} chars). Maximum allowed: {} chars.",
                req.message.len(),
                MAX_INPUT_LENGTH
            ),
        }));
    }

    let mut planner = state.planner.lock().await;

    match planner.run(&req.message, &state.tool_registry).await {
        Ok(reply) => Ok(Json(ChatResponse { reply })),
        Err(e) => Ok(Json(ChatResponse {
            reply: format!("Error: {}", e),
        })),
    }
}

/// Handle GET /health — health check endpoint.
async fn handle_health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}
