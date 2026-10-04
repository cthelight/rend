//! The axum router: static assets, the JSON state snapshot, the SSE event
//! stream, and the drive/settings actions.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use rend_encode::Format;

use crate::assets;
use crate::state::{ApiError, AppState};
use crate::view::{ServerEvent, StateView};

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/style.css", get(style))
        .route("/app.js", get(app_js))
        .route("/api/state", get(state_view))
        .route("/api/events", get(events))
        .route("/api/settings", post(update_settings))
        .route("/api/drives/{i}/load", post(load))
        .route("/api/drives/{i}/rip", post(rip))
        .route("/api/drives/{i}/stop", post(stop))
        .route("/api/drives/{i}/eject", post(eject))
        .route("/api/drives/{i}/match", post(select_match))
        .route("/api/drives/{i}/tags", post(tags))
        .with_state(state)
}

async fn root(State(_state): State<Arc<AppState>>) -> Response {
    serve("index.html")
}

async fn style(State(_state): State<Arc<AppState>>) -> Response {
    serve("style.css")
}

async fn app_js(State(_state): State<Arc<AppState>>) -> Response {
    serve("app.js")
}

fn serve(file: &str) -> Response {
    match assets::file(file) {
        Some((bytes, mime)) => ([(axum::http::header::CONTENT_TYPE, mime)], bytes).into_response(),
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn state_view(State(state): State<Arc<AppState>>) -> Json<StateView> {
    Json(state.view())
}

async fn events(State(state): State<Arc<AppState>>) -> Response {
    let rx = state.events_receiver();
    let stream =
        BroadcastStream::new(rx).map(|result| -> Result<Event, std::convert::Infallible> {
            Ok(match result {
                Ok(event) => event.to_sse(),
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(skipped)) => {
                    ServerEvent::Resync { skipped }.to_sse()
                }
            })
        });
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsRequest {
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    force: Option<bool>,
}

async fn update_settings(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SettingsRequest>,
) -> Response {
    if let Some(raw) = req.format {
        match Format::parse(&raw) {
            Ok(format) => state.set_format(format),
            Err(message) => return error_response(ApiError::bad(message)),
        }
    }
    if let Some(force) = req.force {
        state.set_force(force);
    }
    (StatusCode::OK, Json(state.view())).into_response()
}

async fn load(State(state): State<Arc<AppState>>, Path(i): Path<usize>) -> Response {
    let worker = Arc::clone(&state);
    match tokio::task::spawn_blocking(move || worker.load_toc(i)).await {
        Ok(Ok(())) => (StatusCode::ACCEPTED, Json(state.view())).into_response(),
        Ok(Err(error)) => error_response(error),
        Err(join) => internal(join),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RipRequest {
    #[serde(default)]
    tracks: Vec<u8>,
}

async fn rip(
    State(state): State<Arc<AppState>>,
    Path(i): Path<usize>,
    Json(req): Json<RipRequest>,
) -> Response {
    let worker = Arc::clone(&state);
    let tracks = req.tracks;
    match tokio::task::spawn_blocking(move || worker.rip(i, tracks)).await {
        Ok(Ok(())) => (StatusCode::ACCEPTED, Json(state.view())).into_response(),
        Ok(Err(error)) => error_response(error),
        Err(join) => internal(join),
    }
}

async fn stop(State(state): State<Arc<AppState>>, Path(i): Path<usize>) -> Response {
    match state.stop_rip(i) {
        Ok(()) => (StatusCode::OK, Json(state.view())).into_response(),
        Err(error) => error_response(error),
    }
}

async fn eject(State(state): State<Arc<AppState>>, Path(i): Path<usize>) -> Response {
    let worker = Arc::clone(&state);
    match tokio::task::spawn_blocking(move || worker.eject(i)).await {
        Ok(Ok(())) => (StatusCode::OK, Json(state.view())).into_response(),
        Ok(Err(error)) => error_response(error),
        Err(join) => internal(join),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MatchRequest {
    #[serde(default)]
    index: usize,
}

async fn select_match(
    State(state): State<Arc<AppState>>,
    Path(i): Path<usize>,
    Json(req): Json<MatchRequest>,
) -> Response {
    match state.select_match(i, req.index) {
        Ok(()) => (StatusCode::OK, Json(state.view())).into_response(),
        Err(error) => error_response(error),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TagsRequest {
    #[serde(default)]
    album: Option<String>,
    #[serde(default)]
    artist: Option<String>,
    #[serde(default)]
    album_artist: Option<String>,
    #[serde(default)]
    year: Option<String>,
    #[serde(default)]
    tracks: Vec<TrackTagsRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrackTagsRequest {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    artist: Option<String>,
}

async fn tags(
    State(state): State<Arc<AppState>>,
    Path(i): Path<usize>,
    Json(req): Json<TagsRequest>,
) -> Response {
    let tracks = req
        .tracks
        .into_iter()
        .map(|track| (track.title, track.artist))
        .collect();
    match state.set_tags(i, req.album, req.artist, req.album_artist, req.year, tracks) {
        Ok(()) => (StatusCode::OK, Json(state.view())).into_response(),
        Err(error) => error_response(error),
    }
}

fn error_response(error: ApiError) -> Response {
    (
        error.status,
        Json(serde_json::json!({ "error": error.message })),
    )
        .into_response()
}

fn internal(join: tokio::task::JoinError) -> Response {
    error_response(ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: format!("drive task failed: {join}"),
    })
}
