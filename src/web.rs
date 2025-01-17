use crate::cache::{Key, Layer};
use crate::highlight::{self, DATA};
use crate::id::Id;
use crate::{Entry, Error, Router};
use askama::Template;
use askama_axum::IntoResponse;
use axum::extract::{Form, Path};
use axum::headers::HeaderValue;
use axum::http::{header, StatusCode};
use axum::response::{Redirect, Response};
use axum::routing::get;
use axum::{headers, Extension, TypedHeader};
use bytes::Bytes;
use once_cell::sync::Lazy;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::env;

static TITLE: Lazy<String> =
    Lazy::new(|| env::var("WASTEBIN_TITLE").unwrap_or_else(|_| "wastebin".to_string()));

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Serialize, Deserialize)]
struct FormEntry {
    text: String,
    extension: Option<String>,
    expires: String,
}

impl From<FormEntry> for Entry {
    fn from(entry: FormEntry) -> Self {
        let burn_after_reading = Some(entry.expires == "burn");

        let expires = match entry.expires.parse::<u32>() {
            Ok(0) | Err(_) => None,
            Ok(secs) => Some(secs),
        };

        Self {
            text: entry.text,
            extension: entry.extension,
            expires,
            burn_after_reading,
            seconds_since_creation: 0,
        }
    }
}

#[derive(Template)]
#[template(path = "index.html")]
struct Index<'a> {
    title: &'a str,
    syntaxes: &'a [syntect::parsing::SyntaxReference],
    version: &'a str,
}

#[derive(Template)]
#[template(path = "paste.html")]
struct Paste<'a> {
    title: &'a str,
    id: String,
    formatted: String,
    extension: String,
    deletion_possible: bool,
    version: &'a str,
}

#[derive(Template)]
#[template(path = "burn.html")]
struct BurnPage<'a> {
    title: &'a str,
    id: String,
    version: &'a str,
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage<'a> {
    title: &'a str,
    error: String,
    version: &'a str,
}

type ErrorHtml<'a> = (StatusCode, ErrorPage<'a>);

impl From<Error> for ErrorHtml<'_> {
    fn from(err: Error) -> Self {
        let html = ErrorPage {
            title: &TITLE,
            error: err.to_string(),
            version: VERSION,
        };

        (err.into(), html)
    }
}

#[allow(clippy::unused_async)]
async fn index<'a>() -> Index<'a> {
    Index {
        title: &TITLE,
        syntaxes: DATA.syntax_set.syntaxes(),
        version: VERSION,
    }
}

async fn insert(
    Form(entry): Form<FormEntry>,
    layer: Extension<Layer>,
) -> Result<Redirect, ErrorHtml<'static>> {
    let id: Id = tokio::task::spawn_blocking(|| {
        let mut rng = rand::thread_rng();
        rng.gen::<u32>()
    })
    .await
    .map_err(Error::from)?
    .into();

    let entry: Entry = entry.into();
    let url = id.to_url_path(&entry);
    let burn_after_reading = entry.burn_after_reading.unwrap_or(false);

    layer.insert(id, entry).await?;

    if burn_after_reading {
        Ok(Redirect::to(&format!("/burn{url}")))
    } else {
        Ok(Redirect::to(&url))
    }
}

async fn show(
    id_with_opt_ext: Path<String>,
    layer: Extension<Layer>,
) -> Result<Paste<'static>, ErrorHtml<'static>> {
    let title = &TITLE;
    let key = Key::try_from(id_with_opt_ext)?;
    let id = key.id();
    let extension = key.extension();
    let entry = layer.get_formatted(key).await?;

    Ok(Paste {
        title,
        id,
        extension,
        formatted: entry.formatted,
        deletion_possible: entry.seconds_since_creation < 60,
        version: VERSION,
    })
}

#[allow(clippy::unused_async)]
async fn burn_link(Path(id): Path<String>) -> BurnPage<'static> {
    BurnPage {
        title: &TITLE,
        id,
        version: VERSION,
    }
}

async fn delete(
    Path(id): Path<String>,
    layer: Extension<Layer>,
) -> Result<Redirect, ErrorHtml<'static>> {
    let id = Id::try_from(id.as_str())?;
    let entry = layer.get(id).await?;

    if entry.seconds_since_creation > 60 {
        Err(Error::DeletionTimeExpired)?
    }

    layer.delete(id).await?;

    Ok(Redirect::to("/"))
}

async fn download(
    Path((id, extension)): Path<(String, String)>,
    layer: Extension<Layer>,
) -> Result<Response<String>, ErrorHtml<'static>> {
    // Validate extension.
    if !extension.is_ascii() {
        Err(Error::IllegalCharacters)?
    }

    let raw_string = layer.get(Id::try_from(id.as_str())?).await?.text;
    let content_type = "text; charset=utf-8";
    let content_disposition = format!(r#"attachment; filename="{id}.{extension}"#);

    Ok(Response::builder()
        .header(header::CONTENT_TYPE, HeaderValue::from_static(content_type))
        .header(header::CONTENT_DISPOSITION, content_disposition)
        .body(raw_string)
        .map_err(Error::from)?)
}

#[allow(clippy::unused_async)]
async fn favicon() -> impl IntoResponse {
    (
        TypedHeader(headers::ContentType::png()),
        Bytes::from_static(include_bytes!("../assets/favicon.png")),
    )
}

pub fn routes() -> Router {
    Router::new()
        .route("/", get(index).post(insert))
        .route("/:id", get(show))
        .route("/burn/:id", get(burn_link))
        .route("/delete/:id", get(delete))
        .route("/download/:id/:extension", get(download))
        .route("/favicon.png", get(favicon))
        .route("/style.css", get(|| async { highlight::main() }))
        .route("/dark.css", get(|| async { highlight::dark() }))
        .route("/light.css", get(|| async { highlight::light() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{make_app, Client};
    use http::StatusCode;

    #[tokio::test]
    async fn unknown_paste() -> Result<(), Box<dyn std::error::Error>> {
        let client = Client::new(make_app()?);

        let res = client.get("/000000").send().await?;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        Ok(())
    }

    #[tokio::test]
    async fn insert() -> Result<(), Box<dyn std::error::Error>> {
        let client = Client::new(make_app()?);

        let data = FormEntry {
            text: "FooBarBaz".to_string(),
            extension: None,
            expires: "0".to_string(),
        };

        let res = client.post("/").form(&data).send().await?;
        assert_eq!(res.status(), StatusCode::SEE_OTHER);

        let location = res.headers().get("location").unwrap().to_str()?;

        let res = client.get(location).send().await?;
        assert_eq!(res.status(), StatusCode::OK);

        let content = res.text().await?;
        assert!(content.contains("FooBarBaz"));

        Ok(())
    }

    #[tokio::test]
    async fn delete() -> Result<(), Box<dyn std::error::Error>> {
        let client = Client::new(make_app()?);

        let data = FormEntry {
            text: "FooBarBaz".to_string(),
            extension: None,
            expires: "0".to_string(),
        };

        let res = client.post("/").form(&data).send().await?;
        assert_eq!(res.status(), StatusCode::SEE_OTHER);

        let location = res.headers().get("location").unwrap().to_str()?;
        let res = client.get(&format!("/delete{location}")).send().await?;
        assert_eq!(res.status(), StatusCode::SEE_OTHER);

        let res = client.get(location).send().await?;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        Ok(())
    }
}
