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
use futures_util::stream;
use serde::Deserialize;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::assets;
use crate::shutdown_signal;
use crate::state::{ApiError, AppState};
use crate::view::{ServerEvent, StateView, decode_image_data};

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/style.css", get(style))
        .route("/app.js", get(app_js))
        .route("/icon.svg", get(icon_svg))
        .route("/icon-16.png", get(icon_16))
        .route("/icon-32.png", get(icon_32))
        .route("/api/state", get(state_view))
        .route("/api/events", get(events))
        .route("/api/settings", post(update_settings))
        .route("/api/drives/{i}/load", post(load))
        .route("/api/drives/{i}/rip", post(rip))
        .route("/api/drives/{i}/stop", post(stop))
        .route("/api/drives/{i}/eject", post(eject))
        .route("/api/drives/{i}/match", post(select_match))
        .route("/api/drives/{i}/tags", post(tags))
        .route("/api/drives/{i}/cover", post(set_cover))
        .route("/api/drives/{i}/cover/reset", post(reset_cover))
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

async fn icon_svg(State(_state): State<Arc<AppState>>) -> Response {
    serve("icon.svg")
}

async fn icon_16(State(_state): State<Arc<AppState>>) -> Response {
    serve("icon-16.png")
}

async fn icon_32(State(_state): State<Arc<AppState>>) -> Response {
    serve("icon-32.png")
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

/// Error emitted by the event stream when the process is shutting down.
/// Ending the stream with an error lets the in-flight SSE response
/// complete, so the graceful shutdown can finish instead of waiting on
/// an endless feed.
#[derive(Debug)]
struct Shutdown;

impl std::fmt::Display for Shutdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("rend-wui is shutting down")
    }
}

impl std::error::Error for Shutdown {}

async fn events(State(state): State<Arc<AppState>>) -> Response {
    let rx = state.events_receiver();
    let events = BroadcastStream::new(rx).map(|result| -> Result<Event, Shutdown> {
        Ok(match result {
            Ok(event) => event.to_sse(),
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(skipped)) => {
                ServerEvent::Resync { skipped }.to_sse()
            }
        })
    });
    let shutdown =
        stream::once(shutdown_signal()).map(|_| -> Result<Event, Shutdown> { Err(Shutdown) });
    Sse::new(stream::select(events, shutdown))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsRequest {
    #[serde(default)]
    formats: Option<Vec<String>>,
    #[serde(default)]
    force: Option<bool>,
}

async fn update_settings(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SettingsRequest>,
) -> Response {
    if let Some(formats) = req.formats {
        if let Err(error) = state.set_formats(formats) {
            return error_response(error);
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
    /// Overrides the global force setting for this rip only.
    #[serde(default)]
    force: Option<bool>,
}

async fn rip(
    State(state): State<Arc<AppState>>,
    Path(i): Path<usize>,
    Json(req): Json<RipRequest>,
) -> Response {
    let worker = Arc::clone(&state);
    let tracks = req.tracks;
    let force = req.force;
    match tokio::task::spawn_blocking(move || worker.rip(i, tracks, force)).await {
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CoverRequest {
    /// Base64 image data, optionally as a `data:` URL.
    #[serde(default)]
    data: Option<String>,
    /// URL to fetch the image from.
    #[serde(default)]
    url: Option<String>,
}

async fn set_cover(
    State(state): State<Arc<AppState>>,
    Path(i): Path<usize>,
    Json(req): Json<CoverRequest>,
) -> Response {
    match (req.data, req.url) {
        (Some(data), None) => match decode_image_data(&data) {
            Ok(bytes) => match state.set_custom_cover(i, bytes) {
                Ok(()) => (StatusCode::OK, Json(state.view())).into_response(),
                Err(error) => error_response(error),
            },
            Err(message) => error_response(ApiError::bad(message)),
        },
        (None, Some(url)) => {
            let worker = Arc::clone(&state);
            match tokio::task::spawn_blocking(move || worker.set_custom_cover_url(i, url)).await {
                Ok(Ok(())) => (StatusCode::OK, Json(state.view())).into_response(),
                Ok(Err(error)) => error_response(error),
                Err(join) => internal(join),
            }
        }
        // Both or neither: the client must supply exactly one source.
        _ => error_response(ApiError::bad("provide exactly one of 'data' or 'url'")),
    }
}

async fn reset_cover(State(state): State<Arc<AppState>>, Path(i): Path<usize>) -> Response {
    match state.reset_custom_cover(i) {
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
