use crate::cache::Layer;
use crate::id::Id;
use crate::{Entry, Error, Router};
use axum::extract::Path;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Json};
use rand::Rng;
use serde::Serialize;

#[derive(Serialize)]
struct ErrorPayload {
    message: String,
}

#[derive(Serialize)]
struct RedirectResponse {
    path: String,
}

type ErrorResponse = (StatusCode, Json<ErrorPayload>);

impl From<Error> for ErrorResponse {
    fn from(err: Error) -> Self {
        let payload = Json::from(ErrorPayload {
            message: err.to_string(),
        });

        (err.into(), payload)
    }
}

#[allow(clippy::unused_async)]
async fn health() -> StatusCode {
    StatusCode::OK
}

async fn insert(
    Json(entry): Json<Entry>,
    layer: Extension<Layer>,
) -> Result<Json<RedirectResponse>, ErrorResponse> {
    let id: Id = tokio::task::spawn_blocking(|| {
        let mut rng = rand::thread_rng();
        rng.gen::<u32>()
    })
    .await
    .map_err(Error::from)?
    .into();

    let path = id.to_url_path(&entry);

    layer.insert(id, entry).await?;
    Ok(Json::from(RedirectResponse { path }))
}

async fn raw(Path(id): Path<String>, layer: Extension<Layer>) -> Result<String, ErrorResponse> {
    Ok(layer.get(Id::try_from(id.as_str())?).await?.text)
}

async fn delete(Path(id): Path<String>, layer: Extension<Layer>) -> Result<(), ErrorResponse> {
    let id = Id::try_from(id.as_str())?;
    let entry = layer.get(id).await?;

    if entry.seconds_since_creation > 60 {
        Err(Error::DeletionTimeExpired)?
    }

    layer.delete(id).await?;
    Ok(())
}

pub fn routes() -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/entries", post(insert))
        .route("/api/entries/:id", get(raw).delete(delete))
}
