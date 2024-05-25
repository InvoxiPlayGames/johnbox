use std::{borrow::Cow, collections::HashMap, fmt::Display, str::FromStr, sync::Arc};

use axum::{
    extract::{Path, Query, WebSocketUpgrade},
    response::IntoResponse,
    Json,
};
use dashmap::DashMap;
use rand::{rngs::OsRng, RngCore};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Notify;

use crate::{JBRoom, OpMode, State, Token};

pub mod acl;
pub mod entity;
pub mod ws;

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RoomRequest {
    pub app_id: String,
    pub app_tag: String,
    pub audience_enabled: bool,
    pub max_players: u8,
    pub platform: String,
    pub player_names: serde_json::Value,
    pub time: f32,
    pub twitch_locked: bool,
    pub user_id: uuid::Uuid,
    #[serde(default)]
    pub host: String,
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Host,
    #[default]
    Player,
    Audience,
    Moderator,
}

impl Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host => write!(f, "host"),
            Self::Player => write!(f, "player"),
            Self::Audience => write!(f, "audience"),
            Self::Moderator => write!(f, "moderator"),
        }
    }
}

impl FromStr for Role {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "host" => Ok(Self::Host),
            "player" => Ok(Self::Player),
            "audience" => Ok(Self::Audience),
            "moderator" => Ok(Self::Moderator),
            _ => Err(()),
        }
    }
}

#[derive(Deserialize, Serialize, Debug)]
pub struct WSQuery {
    #[serde(rename = "user-id")]
    pub user_id: String,
    pub format: String,
    pub name: String,
    pub role: Role,
    #[serde(rename = "host-token")]
    pub host_token: Option<Token>,
    pub secret: Option<Token>,
    #[serde(default)]
    // Id will never be 0 (this works)
    id: i64,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct JBResponse<T: Serialize + std::fmt::Debug> {
    ok: bool,
    body: T,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct RoomResponse {
    host: String,
    code: String,
    token: String,
}

pub async fn play_handler(
    ws: WebSocketUpgrade,
    axum::extract::State(state): axum::extract::State<State>,
    code: Path<String>,
    url_query: Query<WSQuery>,
) -> impl IntoResponse {
    let Some(config) = state.room_map.get(&code.0) else {
        return (StatusCode::NOT_FOUND, "Room not found").into_response();
    };
    let mut host = match url_query.role {
        Role::Audience => "https://ecast.jackboxgames.com".to_owned(),
        _ => format!("wss://{}", config.value().room_config.host),
    };

    host = host.replace("https://", "wss://");
    host = host.replace("http://", "ws://");

    if matches!(state.config.ecast.op_mode, OpMode::Proxy) {
        ws.protocols(["ecast-v0"])
            .on_upgrade(move |socket| {
                let ecast_req = format!(
                    "{}/api/v2/{}/{}/play?{}",
                    host,
                    match url_query.role {
                        Role::Audience => "audience",
                        _ => "rooms",
                    },
                    code.0,
                    serde_urlencoded::to_string(&url_query.0).unwrap()
                );
                ws::handle_socket_proxy(host, socket, ecast_req, url_query)
            })
            .into_response()
    } else {
        let room_map = Arc::clone(&state.room_map);
        let room = Arc::clone(config.value());
        let config = Arc::clone(&state.config);
        ws.protocols(["ecast-v0"])
            .on_upgrade(move |socket| async move {
                if let Err(e) = ws::handle_socket(socket, code, url_query, room, &config.doodles, room_map).await {
                    tracing::error!(id = e.0.profile.id, role = ?e.0.profile.role, error = %e.1, "Error in WebSocket");
                    e.0.disconnect().await;
                }
            })
            .into_response()
    }
}

pub async fn rooms_handler(
    axum::extract::State(state): axum::extract::State<State>,
    Json(room_req): Json<RoomRequest>,
) -> Json<JBResponse<RoomResponse>> {
    let mut code;
    let token;
    let host;
    match state.config.ecast.op_mode {
        OpMode::Proxy => {
            let url = format!(
                "{}/api/v2/rooms",
                state
                    .config
                    .ecast
                    .server_url
                    .as_ref()
                    .map(|s| s.as_str())
                    .unwrap_or("https://ecast.jackboxgames.com")
            );
            let response: JBResponse<RoomResponse> = state
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
                url = url,
                response = ?response,
                "ecast request"
            );

            code = response.body.code;
            token = response.body.token.parse().unwrap();
            host = response.body.host;
        }
        OpMode::Native => {
            fn random(size: usize) -> Vec<u8> {
                let mut bytes: Vec<u8> = vec![0; size];

                OsRng.fill_bytes(&mut bytes);

                bytes
            }
            const ALPHA_CAPITAL: [char; 26] = [
                'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P',
                'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
            ];
            code = nanoid::nanoid!(4, &ALPHA_CAPITAL, random);
            code.make_ascii_uppercase();

            token = format!("{:02x}", Token::random());
            host = state.config.accessible_host.to_owned();
        }
    }

    state.room_map.insert(
        code.clone(),
        Arc::new(ws::Room {
            entities: DashMap::new(),
            connections: DashMap::new(),
            room_serial: 1.into(),
            room_config: JBRoom {
                app_id: room_req.app_id,
                app_tag: room_req.app_tag.clone(),
                audience_enabled: room_req.audience_enabled,
                code: code.clone(),
                host,
                audience_host: state.config.accessible_host.clone(),
                locked: false,
                full: false,
                moderation_enabled: false,
                password_required: false,
                twitch_locked: false, // unimplemented
                locale: Cow::Borrowed("en"),
                keepalive: false,
            },
            exit: Notify::new(),
        }),
    );

    Json(JBResponse {
        ok: true,
        body: RoomResponse {
            host: state.config.accessible_host.clone(),
            code,
            token,
        },
    })
}

pub async fn rooms_get_handler(
    axum::extract::State(state): axum::extract::State<State>,
    Path(code): Path<String>,
) -> Result<Json<JBResponse<JBRoom>>, (StatusCode, &'static str)> {
    match state.config.ecast.op_mode {
        OpMode::Native => {
            let room = state.room_map.get(&code);

            if let Some(room) = room {
                return Ok(Json(JBResponse {
                    ok: true,
                    body: room.value().room_config.clone(),
                }));
            } else {
                return Err((StatusCode::NOT_FOUND, "Room not found"));
            }
        }
        OpMode::Proxy => {
            let url = format!(
                "{}/api/v2/rooms/{}",
                state
                    .config
                    .ecast
                    .server_url
                    .as_ref()
                    .map(|s| s.as_str())
                    .unwrap_or("https://ecast.jackboxgames.com"),
                code
            );
            let mut response: JBResponse<JBRoom> = state
                .http_cache
                .client
                .get(&url)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();

            tracing::debug!(
                url = url,
                response = ?response,
                "ecast request"
            );

            response.body.host = state.config.accessible_host.clone();
            response.body.audience_host = state.config.accessible_host.clone();

            Ok(Json(response))
        }
    }
}

pub async fn app_config_handler(
    Path(code): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    axum::extract::State(state): axum::extract::State<State>,
) -> Json<JBResponse<serde_json::Value>> {
    match state.config.ecast.op_mode {
        OpMode::Native => {
            return Json(JBResponse {
                ok: true,
                body: json!({
                    "settings": {
                        "serverUrl": state.config.accessible_host.clone()
                    }
                }),
            });
        }
        OpMode::Proxy => {
            let url = format!(
                "{}/api/v2/app-configs/{}?{}",
                state
                    .config
                    .ecast
                    .server_url
                    .as_ref()
                    .map(|s| s.as_str())
                    .unwrap_or("https://ecast.jackboxgames.com"),
                code,
                serde_urlencoded::to_string(query).unwrap()
            );
            let response: JBResponse<serde_json::Value> = state
                .http_cache
                .client
                .get(&url)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();

            tracing::debug!(
                url = url,
                response = ?response,
                "ecast request"
            );

            Json(response)
        }
    }
}
