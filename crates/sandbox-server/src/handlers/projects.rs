//! Projects. Creation needs the operator's bootstrap token (D-021): this is a local/CI
//! sandbox, not a public sign-up.

use axum::{
    Json,
    extract::{FromRequest, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use sandbox_db::DbError;
use sandbox_db::keys::NewKey;
use sandbox_domain::keys::{self, Scope};
use sandbox_domain::project::{ProjectSlug, validate_name};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::auth::{self, Auth};
use crate::error::ApiError;
use crate::extract::SandboxJson;
use crate::state::AppState;

/// `POST /v1/projects` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateProject {
    slug: String,
    name: String,
}

/// Creates a project with its chart of accounts and a first admin key, shown once.
pub async fn create(State(state): State<AppState>, request: Request) -> Result<Response, ApiError> {
    let (parts, body) = request.into_parts();
    if !auth::is_bootstrap(&parts, &state) {
        return Err(ApiError::Unauthorized);
    }
    // The token is checked before the body is read, so an unauthenticated caller learns
    // nothing about the body rules.
    let SandboxJson(input): SandboxJson<CreateProject> =
        SandboxJson::from_request(Request::from_parts(parts, body), &state).await?;
    let slug = ProjectSlug::parse(&input.slug).map_err(|e| ApiError::InvalidBody(e.to_string()))?;
    let name = validate_name(&input.name).map_err(|e| ApiError::InvalidBody(e.to_owned()))?;

    let secret = auth::generate_key()?;
    let hash = keys::hash_presented(&secret).map_err(|_| {
        ApiError::Internal(anyhow::anyhow!("generated key failed its own shape check"))
    })?;
    let prefix = keys::display_prefix(&secret);
    let project_id = Uuid::now_v7();
    let key_id = Uuid::now_v7();

    let mut tx = state.db.begin().await.map_err(DbError::from)?;
    let project =
        match sandbox_db::projects::create_in(&mut tx, project_id, slug.as_str(), name).await {
            Ok(project) => project,
            Err(DbError::Constraint(c)) if c == "projects_slug_key" => {
                return Err(ApiError::Conflict(
                    "a project with this slug already exists".to_owned(),
                ));
            }
            Err(other) => return Err(other.into()),
        };
    let key = sandbox_db::keys::insert(
        &mut tx,
        project_id,
        &NewKey {
            id: key_id,
            hash,
            prefix: &prefix,
            scopes: &[Scope::Admin],
            label: "first admin key",
        },
        "bootstrap",
    )
    .await?;
    tx.commit().await.map_err(DbError::from)?;
    tracing::info!(project_id = %project.id, key_prefix = %key.prefix, "project created");

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "project": project_json(&project),
            "api_key": {"id": key.id, "secret": secret, "prefix": key.prefix, "scopes": key.scopes,
                        "note": "shown once; only its SHA-256 is stored"},
            "simulated": true,
        })),
    )
        .into_response())
}

/// `GET /v1/project`: the caller's project.
pub async fn current(State(state): State<AppState>, auth: Auth) -> Result<Response, ApiError> {
    auth.require(Scope::Read)?;
    let project = sandbox_db::projects::get(&state.db, auth.scope().project_id).await?;
    Ok(Json(json!({"project": project_json(&project), "simulated": true})).into_response())
}

fn project_json(project: &sandbox_db::projects::Project) -> serde_json::Value {
    json!({
        "id": project.id,
        "slug": project.slug,
        "name": project.name,
        "dataset_epoch": project.dataset_epoch,
    })
}
