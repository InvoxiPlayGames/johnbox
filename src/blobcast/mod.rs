use std::{borrow::Cow, ops::Deref, sync::Arc};

use axum::{
    extract::{Path, Query, WebSocketUpgrade},
    response::IntoResponse,
    Json,
};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::{OpMode, State, Token};

pub mod ws;

pub const APP_TAGS: phf::Map<&'static str, &'static str> = phf::phf_map! {
    "130f9f92-6fc4-4cdb-815e-0f65fdd2904b" => "survivetheinternet",
    "75a6de72-ea54-e1cb-28e1-aab354704d45" => "fibbage3"
};

#[derive(Deserialize, Serialize)]
pub struct BlobcastWSQuery {
    t: u64,
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

    if matches!(state.config.blobcast.op_mode, OpMode::Proxy) {
        ws.on_upgrade(move |socket| {
            let ecast_req = format!("{}/socket.io/websocket/{}", host, id.0);
            ws::handle_socket_proxy(host, socket, ecast_req)
        })
        .into_response()
    } else {
        let room_map = Arc::clone(&state.room_map);
        ws
            .on_upgrade(move |socket| async move {
                if let Err(e) = ws::handle_socket(socket, room_map, state.config.accessible_host.clone()).await {
                    tracing::error!(id = e.0.profile.id, role = ?e.0.profile.role, error = %e.1, "Error in WebSocket");
                    e.0.disconnect().await;
                }
            })
            .into_response()
    }
}

pub async fn load_handler(
    axum::extract::State(state): axum::extract::State<State>,
    url_query: Query<BlobcastWSQuery>,
) -> String {
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

            response
        }
        OpMode::Native => format!(
            "{:x}:60:60:websocket", // We are not compatible with flashsocket, excluding it just to be safe
            Token::from_seed(url_query.t)
        ),
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

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AccessTokenRequest {
    app_id: uuid::Uuid,
    room_id: String,
    user_id: String,
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AccessTokenResponse {
    access_token: Token,
    success: bool,
}

pub async fn access_token_handler(
    axum::extract::State(state): axum::extract::State<State>,
    Json(token_req): Json<AccessTokenRequest>,
) -> Json<AccessTokenResponse> {
    let response = match state.config.blobcast.op_mode {
        OpMode::Proxy => {
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

            let response: AccessTokenResponse = state
                .http_cache
                .client
                .post(&url)
                .json(&token_req)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();

            tracing::debug!(
                req = ?token_req,
                url = url,
                response = ?response,
                "blobcast request"
            );

            // state.room_map.insert(
            //     token_req.room_id.clone(),
            //     Arc::new(crate::Room {
            //         entities: DashMap::new(),
            //         connections: DashMap::new(),
            //         room_serial: 1.into(),
            //         room_config: crate::JBRoom {
            //             app_id: token_req.app_id.to_string(),
            //             app_tag: String::new(),
            //             audience_enabled: false,
            //             code: token_req.room_id.clone(),
            //             host: state.blobcast_host.read().await.clone(),
            //             audience_host: state.config.ecast.server_url.clone().unwrap_or_default(),
            //             locked: false,
            //             full: false,
            //             moderation_enabled: false,
            //             password_required: false,
            //             twitch_locked: false,
            //             locale: Cow::Borrowed("en"),
            //             keepalive: false,
            //         },
            //         exit: Notify::new(),
            //     }),
            // );

            response
        }
        OpMode::Native => AccessTokenResponse {
            access_token: Token::random(),
            success: true,
        },
    };

    return Json(response);
}
