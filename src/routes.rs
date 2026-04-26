use std::path::Path;

use axum::{
    Json, Router,
    body::Body,
    extract::{Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use tower_http::trace::TraceLayer;

use crate::{
    AppState,
    git::{CommitResult, GitError, GitRepo},
    vault::{TreeNode, VaultError},
};

#[derive(RustEmbed)]
#[folder = "src/assets"]
struct Assets;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(serve_index))
        .route("/assets/{*path}", get(serve_asset))
        .route("/api/tree", get(api_tree))
        .route(
            "/api/file",
            get(api_get_file).put(api_put_file).delete(api_delete_file),
        )
        .route("/api/file/create", post(api_create_file))
        .route("/api/file/rename", post(api_rename_file))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

// ---------- Static asset serving ----------

async fn serve_index() -> Response {
    serve_embedded("index.html").await
}

async fn serve_asset(axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    serve_embedded(&path).await
}

async fn serve_embedded(path: &str) -> Response {
    match Assets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let mut resp = Response::new(Body::from(content.data.into_owned()));
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(mime.as_ref())
                    .unwrap_or(HeaderValue::from_static("application/octet-stream")),
            );
            resp
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

// ---------- API types ----------

#[derive(Deserialize)]
struct PathQuery {
    path: String,
}

#[derive(Deserialize)]
struct PutFileBody {
    path: String,
    content: String,
    message: Option<String>,
}

#[derive(Deserialize)]
struct CreateFileBody {
    path: String,
    #[serde(default)]
    content: String,
    message: Option<String>,
}

#[derive(Deserialize)]
struct DeleteFileBody {
    path: String,
    message: Option<String>,
}

#[derive(Deserialize)]
struct RenameFileBody {
    from: String,
    to: String,
    message: Option<String>,
}

#[derive(Serialize)]
struct CommitResponse {
    committed: bool,
    sha: Option<String>,
}

#[derive(Serialize)]
struct FileContent {
    path: String,
    content: String,
}

// ---------- API handlers ----------

async fn api_tree(State(state): State<AppState>) -> Json<TreeNode> {
    Json(state.vault.tree())
}

async fn api_get_file(
    State(state): State<AppState>,
    Query(q): Query<PathQuery>,
) -> Result<Json<FileContent>, ApiError> {
    let abs = state.vault.resolve(&q.path)?;
    if !abs.exists() {
        return Err(ApiError::NotFound(q.path.clone()));
    }
    if !abs.is_file() {
        return Err(ApiError::BadRequest(format!("not a file: {}", q.path)));
    }
    let content = tokio::fs::read_to_string(&abs)
        .await
        .map_err(|e| ApiError::Internal(format!("read {}: {e}", q.path)))?;
    Ok(Json(FileContent {
        path: q.path,
        content,
    }))
}

async fn api_put_file(
    State(state): State<AppState>,
    Json(body): Json<PutFileBody>,
) -> Result<Json<CommitResponse>, ApiError> {
    let abs = state.vault.resolve(&body.path)?;
    if let Some(parent) = abs.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| ApiError::Internal(format!("mkdir {}: {e}", parent.display())))?;
    }
    tokio::fs::write(&abs, &body.content)
        .await
        .map_err(|e| ApiError::Internal(format!("write {}: {e}", body.path)))?;

    let rel = state
        .vault
        .relative_str(&abs)
        .ok_or_else(|| ApiError::Internal("could not derive relative path".into()))?;

    let message = body
        .message
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("edit: {rel}"));

    commit(&state, &[&rel], &message).await
}

async fn api_create_file(
    State(state): State<AppState>,
    Json(body): Json<CreateFileBody>,
) -> Result<Json<CommitResponse>, ApiError> {
    let abs = state.vault.resolve(&body.path)?;
    if abs.exists() {
        return Err(ApiError::Conflict(format!(
            "file already exists: {}",
            body.path
        )));
    }
    if let Some(parent) = abs.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| ApiError::Internal(format!("mkdir {}: {e}", parent.display())))?;
    }
    tokio::fs::write(&abs, &body.content)
        .await
        .map_err(|e| ApiError::Internal(format!("write {}: {e}", body.path)))?;

    let rel = state
        .vault
        .relative_str(&abs)
        .ok_or_else(|| ApiError::Internal("could not derive relative path".into()))?;

    let message = body
        .message
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("create: {rel}"));

    commit(&state, &[&rel], &message).await
}

async fn api_delete_file(
    State(state): State<AppState>,
    Json(body): Json<DeleteFileBody>,
) -> Result<Json<CommitResponse>, ApiError> {
    let abs = state.vault.resolve(&body.path)?;
    if !abs.exists() {
        return Err(ApiError::NotFound(body.path.clone()));
    }
    let rel = state
        .vault
        .relative_str(&abs)
        .ok_or_else(|| ApiError::Internal("could not derive relative path".into()))?;

    let repo = GitRepo {
        root: state.vault.root(),
        user_name: &state.git_user_name,
        user_email: &state.git_user_email,
    };

    let message = body
        .message
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("delete: {rel}"));

    let result = repo.rm_and_commit(&rel, &message).await?;
    Ok(Json(commit_response(result)))
}

async fn api_rename_file(
    State(state): State<AppState>,
    Json(body): Json<RenameFileBody>,
) -> Result<Json<CommitResponse>, ApiError> {
    let abs_from = state.vault.resolve(&body.from)?;
    let abs_to = state.vault.resolve(&body.to)?;
    if !abs_from.exists() {
        return Err(ApiError::NotFound(body.from.clone()));
    }
    if abs_to.exists() {
        return Err(ApiError::Conflict(format!(
            "destination already exists: {}",
            body.to
        )));
    }
    if let Some(parent) = abs_to.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| ApiError::Internal(format!("mkdir {}: {e}", parent.display())))?;
    }

    let rel_from = state
        .vault
        .relative_str(&abs_from)
        .ok_or_else(|| ApiError::Internal("could not derive relative path".into()))?;
    let rel_to = state
        .vault
        .relative_str(&abs_to)
        .ok_or_else(|| ApiError::Internal("could not derive relative path".into()))?;

    let repo = GitRepo {
        root: state.vault.root(),
        user_name: &state.git_user_name,
        user_email: &state.git_user_email,
    };

    let message = body
        .message
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("rename: {rel_from} -> {rel_to}"));

    let result = repo.mv_and_commit(&rel_from, &rel_to, &message).await?;
    Ok(Json(commit_response(result)))
}

// ---------- Helpers ----------

async fn commit(
    state: &AppState,
    rel_paths: &[&str],
    message: &str,
) -> Result<Json<CommitResponse>, ApiError> {
    let repo = GitRepo {
        root: state.vault.root(),
        user_name: &state.git_user_name,
        user_email: &state.git_user_email,
    };
    let result = repo.add_and_commit(rel_paths, message).await?;
    Ok(Json(commit_response(result)))
}

fn commit_response(result: CommitResult) -> CommitResponse {
    match result {
        CommitResult::Committed { sha } => CommitResponse {
            committed: true,
            sha: Some(sha),
        },
        CommitResult::Nothing => CommitResponse {
            committed: false,
            sha: None,
        },
    }
}

#[allow(dead_code)]
fn debug_path(p: &Path) -> String {
    p.display().to_string()
}

// ---------- Errors ----------

#[derive(Debug, thiserror::Error)]
enum ApiError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("internal: {0}")]
    Internal(String),
    #[error("git error: {0}")]
    Git(#[from] GitError),
    #[error("vault error: {0}")]
    Vault(#[from] VaultError),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::BadRequest(_) | ApiError::Vault(_) => StatusCode::BAD_REQUEST,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::Internal(_) | ApiError::Git(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = serde_json::json!({ "error": self.to_string() });
        (status, Json(body)).into_response()
    }
}
