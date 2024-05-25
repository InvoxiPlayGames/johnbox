use std::{borrow::Cow, ops::Deref};

use axum::{
    extract::{Path, Query, WebSocketUpgrade},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{OpMode, State};

pub mod ws;

#[derive(Deserialize, Serialize)]
pub struct BlobcastWSQuery {
    t: u32,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct BlobcastRoomResponse {
    create: bool,
    server: String,
}

pub async fn play_handler(
    ws: WebSocketUpgrade,
    axum::extract::State(state): axum::extract::State<State>,
    id: Path<String>,
) -> impl IntoResponse {
    let host = format!(
        "wss://{}:38203/socket.io/1/websocket/{}",
        state.blobcast_host.read().await.deref(),
        id.0
    );

    if matches!(state.config.ecast.op_mode, OpMode::Proxy) {
        ws.on_upgrade(move |socket| {
            let ecast_req = format!("{}/socket.io/websocket/{}", host, id.0);
            ws::handle_socket_proxy(host, socket, ecast_req)
        })
        .into_response()
    } else {
        // let room_map = Arc::clone(&state.room_map);
        // let room = Arc::clone(config.value());
        // let config = Arc::clone(&state.config);
        // ws.protocols(["ecast-v0"])
        //     .on_upgrade(move |socket| async move {
        //         if let Err(e) = ws::handle_socket(socket, code, url_query, room, &config.doodles, room_map).await {
        //             tracing::error!(id = e.0.profile.id, role = ?e.0.profile.role, error = %e.1, "Error in WebSocket");
        //             e.0.disconnect().await;
        //         }
        //     })
        //     .into_response()
        unimplemented!()
    }
}

pub async fn load_handler(
    axum::extract::State(state): axum::extract::State<State>,
    url_query: Query<BlobcastWSQuery>,
) -> impl IntoResponse {
    match state.config.blobcast.op_mode {
        OpMode::Proxy => {
            let url = format!(
                "https://{}:38203/socket.io/1?{}",
                state.blobcast_host.read().await.deref(),
                serde_urlencoded::to_string(&url_query.0).unwrap()
            );

            let response: String = state
                .http_cache
                .client
                .get(&url)
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap();

            tracing::debug!(
                url = url,
                response = ?response,
                "blobcast request"
            );

            response.into_response()
        }
        OpMode::Native => unimplemented!(),
    }
}

pub async fn rooms_handler(
    axum::extract::State(state): axum::extract::State<State>,
) -> Json<BlobcastRoomResponse> {
    let f_url;
    let response = match state.config.blobcast.op_mode {
        OpMode::Proxy => {
            let url = format!(
                "{}/room",
                state
                    .config
                    .blobcast
                    .server_url
                    .as_ref()
                    .map(|s| s.as_str())
                    .unwrap_or("http://blobcast.jackboxgames.com")
            );

            let mut response: BlobcastRoomResponse = state
                .http_cache
                .client
                .get(&url)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();

            *state.blobcast_host.write().await = response.server.clone();

            response.server = state.config.accessible_host.to_owned();

            tracing::debug!(
                url = url,
                response = ?response,
                "blobcast request"
            );

            f_url = Cow::Owned(url);

            response
        }
        OpMode::Native => {
            f_url = Cow::Borrowed("/room");
            BlobcastRoomResponse {
                create: true,
                server: state.config.accessible_host.to_owned(),
            }
        }
    };

    tracing::debug!(
        url = f_url.as_ref(),
        response = ?response,
        "blobcast request"
    );

    Json(response)
}

pub async fn access_token_handler(
    axum::extract::State(state): axum::extract::State<State>,
    Json(room_req): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    let url = format!(
        "{}/accessToken",
        state
            .config
            .blobcast
            .server_url
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or("http://blobcast.jackboxgames.com")
    );

    let response: serde_json::Value = state
        .http_cache
        .client
        .post(&url)
        .json(&room_req)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    tracing::debug!(
        req = ?room_req,
        url = url,
        response = ?response,
        "blobcast request"
    );

    return Json(response);
}
