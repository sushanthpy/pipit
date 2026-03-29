//! Axum REST API for task submission, status, cancellation, and steering.
//!
//! Endpoints:
//!   POST   /api/tasks              — submit task
//!   GET    /api/tasks              — list queue status
//!   DELETE /api/tasks/:id          — cancel task
//!   GET    /api/projects           — list projects
//!   POST   /api/projects/:name/steer — inject steering message
//!   GET    /api/health             — health check

use crate::config::{AuthConfig, AuthPermission, AuthToken, ServerConfig};
use crate::pool::AgentPool;
use crate::queue::TaskQueue;
use crate::reporter::Reporter;
use crate::store::DaemonStore;

use anyhow::Result;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{delete, get, post},
    Json, Router,
};
use chrono::Utc;
use futures::stream::Stream;
use pipit_channel::{MessageOrigin, NormalizedTask, TaskPriority};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct AppState {
    queue: Arc<TaskQueue>,
    pool: Arc<AgentPool>,
    store: Arc<DaemonStore>,
    reporter: Arc<Reporter>,
    auth_tokens: HashMap<String, AuthToken>,
    started_at: Instant,
}

// ---------------------------------------------------------------------------
// Server startup
// ---------------------------------------------------------------------------

pub async fn spawn_server(
    config: &ServerConfig,
    auth: &AuthConfig,
    queue: Arc<TaskQueue>,
    pool: Arc<AgentPool>,
    store: Arc<DaemonStore>,
    reporter: Arc<Reporter>,
    cancel: CancellationToken,
) -> Result<tokio::task::JoinHandle<()>> {
    // Build reverse lookup: secret → token config
    let mut auth_tokens = HashMap::new();
    for (name, token) in &auth.tokens {
        auth_tokens.insert(token.secret.clone(), token.clone());
    }

    let state = AppState {
        queue,
        pool,
        store,
        reporter,
        auth_tokens,
        started_at: Instant::now(),
    };

    let app = Router::new()
        .route("/api/tasks", post(submit_task))
        .route("/api/tasks", get(list_tasks))
        .route("/api/tasks/{id}", delete(cancel_task))
        .route("/api/tasks/{id}/stream", get(stream_task))
        .route("/api/projects", get(list_projects))
        .route("/api/projects/{name}/steer", post(steer_project))
        .route("/api/health", get(health_check))
        .with_state(state);

    let bind_addr = format!("{}:{}", config.bind, config.port);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;

    tracing::info!(addr = %bind_addr, "HTTP API listening");

    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                cancel.cancelled().await;
            })
            .await
            .unwrap_or_else(|e| tracing::error!(error = %e, "HTTP server error"));
    });

    Ok(handle)
}

// ---------------------------------------------------------------------------
// Auth middleware helper
// ---------------------------------------------------------------------------

fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

fn check_auth(
    headers: &HeaderMap,
    auth_tokens: &HashMap<String, AuthToken>,
    required_perm: AuthPermission,
) -> Result<(), (StatusCode, String)> {
    // If no tokens configured, allow all (development mode)
    if auth_tokens.is_empty() {
        return Ok(());
    }

    let token = extract_bearer_token(headers)
        .ok_or((StatusCode::UNAUTHORIZED, "missing bearer token".to_string()))?;

    let auth_token = auth_tokens
        .get(token)
        .ok_or((StatusCode::UNAUTHORIZED, "invalid token".to_string()))?;

    if !auth_token.has_permission(required_perm) {
        return Err((
            StatusCode::FORBIDDEN,
            format!("token lacks {:?} permission", required_perm),
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SubmitTaskRequest {
    project: String,
    prompt: String,
    priority: Option<String>,
}

#[derive(Serialize)]
struct SubmitTaskResponse {
    task_id: String,
    status: String,
}

async fn submit_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SubmitTaskRequest>,
) -> impl IntoResponse {
    if let Err((status, msg)) = check_auth(&headers, &state.auth_tokens, AuthPermission::Submit) {
        return (status, Json(serde_json::json!({"error": msg}))).into_response();
    }

    let priority = match body.priority.as_deref() {
        Some("high") => TaskPriority::High,
        Some("low") => TaskPriority::Low,
        _ => TaskPriority::Normal,
    };

    let client_id = extract_bearer_token(&headers)
        .map(|t| t[..8.min(t.len())].to_string()); // Truncated for privacy

    let origin = MessageOrigin::Api { client_id };
    let task = NormalizedTask::new(body.project, body.prompt, origin).with_priority(priority);
    let task_id = task.task_id.clone();

    match state.queue.submit(task).await {
        Ok(record) => (
            StatusCode::CREATED,
            Json(serde_json::json!({
                "task_id": record.task_id,
                "status": "queued"
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn list_tasks(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err((status, msg)) = check_auth(&headers, &state.auth_tokens, AuthPermission::Status) {
        return (status, Json(serde_json::json!({"error": msg}))).into_response();
    }

    let queue_status = state.queue.status().await;
    (StatusCode::OK, Json(serde_json::json!(queue_status))).into_response()
}

async fn cancel_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
) -> impl IntoResponse {
    if let Err((status, msg)) = check_auth(&headers, &state.auth_tokens, AuthPermission::Cancel) {
        return (status, Json(serde_json::json!({"error": msg}))).into_response();
    }

    // Try to cancel from pending queue first
    if state.queue.cancel_pending(&task_id).await.is_ok() {
        return (
            StatusCode::OK,
            Json(serde_json::json!({"status": "cancelled", "was": "pending"})),
        )
            .into_response();
    }

    // Try to cancel running task
    match state.queue.cancel_running(&task_id).await {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "cancelled", "was": "running"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn list_projects(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err((status, msg)) = check_auth(&headers, &state.auth_tokens, AuthPermission::Status) {
        return (status, Json(serde_json::json!({"error": msg}))).into_response();
    }

    let statuses = state.pool.project_statuses().await;
    (StatusCode::OK, Json(serde_json::json!(statuses))).into_response()
}

#[derive(Deserialize)]
struct SteerRequest {
    message: String,
}

async fn steer_project(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(project_name): Path<String>,
    Json(body): Json<SteerRequest>,
) -> impl IntoResponse {
    if let Err((status, msg)) = check_auth(&headers, &state.auth_tokens, AuthPermission::Steer) {
        return (status, Json(serde_json::json!({"error": msg}))).into_response();
    }

    match state.pool.steer(&project_name, body.message).await {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "steering message injected"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn health_check(State(state): State<AppState>) -> impl IntoResponse {
    let uptime_secs = state.started_at.elapsed().as_secs();
    let queue_status = state.queue.status().await;
    let projects = state.pool.project_statuses().await;
    let store_keys = state.store.key_count();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ok",
            "uptime_secs": uptime_secs,
            "queue": queue_status,
            "projects": projects,
            "store_keys": store_keys,
        })),
    )
        .into_response()
}

/// SSE streaming endpoint: GET /api/tasks/:id/stream
///
/// Streams TaskUpdate events for a specific task in real-time.
/// Closes when the task reaches a terminal state (Completed, Error, Cancelled).
async fn stream_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
) -> impl IntoResponse {
    if let Err((status, msg)) = check_auth(&headers, &state.auth_tokens, AuthPermission::Status) {
        return Err((status, Json(serde_json::json!({"error": msg}))));
    }

    let mut rx = state.reporter.subscribe();
    let tid = task_id.clone();

    let stream = async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(update) => {
                    if update.task_id != tid {
                        continue;
                    }
                    let is_terminal = matches!(
                        &update.kind,
                        pipit_channel::TaskUpdateKind::Completed { .. }
                            | pipit_channel::TaskUpdateKind::Error { .. }
                            | pipit_channel::TaskUpdateKind::Cancelled
                    );

                    if let Ok(json) = serde_json::to_string(&update) {
                        yield Ok::<_, Infallible>(Event::default().data(json));
                    }

                    if is_terminal {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(task_id = %tid, lagged = n, "SSE subscriber lagged");
                    let msg = serde_json::json!({"warning": format!("missed {} events", n)});
                    yield Ok(Event::default().event("lagged").data(msg.to_string()));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
