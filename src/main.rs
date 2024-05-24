use std::{
    borrow::Cow,
    collections::HashMap,
    fmt::{Debug, Display, LowerHex},
    net::SocketAddr,
    path::PathBuf,
    str::FromStr,
    sync::Arc,
};

use axum::{
    extract::{Host, OriginalUri, Path, Query, WebSocketUpgrade},
    handler::HandlerWithoutStateExt,
    http::{uri::PathAndQuery, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    BoxError, Json, Router,
};
use axum_server::tls_rustls::RustlsConfig;
use dashmap::DashMap;
use helix_stdx::rope::RegexBuilder;
use rand::{rngs::OsRng, RngCore};
use reqwest::header::USER_AGENT;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex, Notify};
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use tree_sitter::{Parser, QueryCursor};
use ws::Room;

mod acl;
mod entity;
mod http_cache;
mod ws;

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

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Host,
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
struct JBResponse<T: Serialize + std::fmt::Debug> {
    ok: bool,
    body: T,
}

#[derive(Deserialize, Serialize, Debug)]
struct RoomResponse {
    host: String,
    code: String,
    token: String,
}

#[derive(Debug)]
pub struct Token([u8; 12]);

impl From<[u8; 12]> for Token {
    fn from(value: [u8; 12]) -> Self {
        Self(value)
    }
}

impl Serialize for Token {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        Serialize::serialize(&format!("{:02x}", self), serializer)
    }
}

impl<'de> Deserialize<'de> for Token {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let token: Cow<'de, str> = Deserialize::deserialize(deserializer)?;
        let mut bytes = [0u8; 12];
        let len = token
            .as_bytes()
            .chunks(2)
            .zip(bytes.iter_mut())
            .map(|(bi, bo)| {
                *bo = u8::from_str_radix(unsafe { std::str::from_utf8_unchecked(bi) }, 16).unwrap()
            })
            .count();
        assert_eq!(len, 12);
        Ok(Token(bytes))
    }
}

impl LowerHex for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for t in self.0 {
            write!(f, "{:02x}", t)?;
        }
        Ok(())
    }
}

impl Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        <Token as LowerHex>::fmt(self, f)
    }
}

impl Token {
    fn random() -> Token {
        let mut t = Token([0u8; 12]);
        OsRng.fill_bytes(&mut t.0);
        t
    }
}

#[derive(Clone)]
struct State {
    room_map: Arc<DashMap<String, Arc<Room>>>,
    http_cache: http_cache::HttpCache,
    config: Arc<Config>,
    offline: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum EcastOpMode {
    Native,
    Proxy,
}

#[derive(Deserialize)]
struct Config {
    doodles: DoodleConfig,
    ecast: Ecast,
    tls: Tls,
    ports: Ports,
    accessible_host: String,
    cache_path: PathBuf,
}

#[derive(Deserialize)]
struct DoodleConfig {
    render: bool,
    path: PathBuf,
}

#[derive(Deserialize)]
struct Ecast {
    op_mode: EcastOpMode,
    server_url: Option<String>,
}

#[derive(Deserialize)]
struct Tls {
    cert: PathBuf,
    key: PathBuf,
}

#[derive(Deserialize, Clone, Copy)]
struct Ports {
    https: u16,
    http: Option<u16>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct JBRoom {
    pub app_id: String,
    pub app_tag: String,
    pub audience_enabled: bool,
    pub code: String,
    pub host: String,
    pub audience_host: String,
    pub locked: bool,
    pub full: bool,
    pub moderation_enabled: bool,
    pub password_required: bool,
    pub twitch_locked: bool,
    pub locale: Cow<'static, str>,
    pub keepalive: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_default())
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config: Config = toml::from_str(&std::fs::read_to_string("config.toml").unwrap()).unwrap();

    let tls_config = RustlsConfig::from_pem_file(&config.tls.cert, &config.tls.key)
        .await
        .unwrap();

    let fragment_regex = RegexBuilder::new()
        .build("ecast.jackboxgames.com|bundles.jackbox.tv|jackbox.tv|cdn.jackboxgames.com|s3.amazonaws.com")
        .unwrap();
    let content_to_compress = RegexBuilder::new().build("text/html|text/css|text/xml|text/javascript|application/javascript|application/x-javascript|application/json").unwrap();

    let js_lang = tree_sitter_javascript::language();
    let fragment_query = tree_sitter::Query::new(&js_lang, "(string_fragment) @frag").unwrap();
    let css_lang = tree_sitter_css::language();
    let css_query = tree_sitter::Query::new(&css_lang, "(plain_value) @frag").unwrap();
    let state = State {
        room_map: Arc::new(DashMap::new()),
        http_cache: http_cache::HttpCache {
            client: reqwest::Client::new(),
            ts_parser: Arc::new(Mutex::new((Parser::new(), QueryCursor::new()))),
            js_lang: Arc::new((js_lang, fragment_query)),
            css_lang: Arc::new((css_lang, css_query)),
            regexes: Arc::new(http_cache::Regexes {
                content_to_compress,
                jackbox_urls: fragment_regex,
            }),
        },
        config: Arc::new(config),
        offline: std::env::args().nth(1).is_some(),
    };

    tokio::fs::create_dir_all(&state.config.doodles.path)
        .await
        .unwrap();

    let ports = state.config.ports;

    let app = Router::new()
        .route("/api/v2/rooms/:code/play", get(play_handler))
        .route("/api/v2/audience/:code/play", get(play_handler))
        .route("/api/v2/rooms", post(rooms_handler))
        .route(
            "/@ecast.jackboxgames.com/api/v2/rooms/:code",
            get(rooms_get_handler),
        )
        .route("/room", get(rooms_blobcast_handler))
        .route("/api/v2/app-configs/:app_tag", get(app_config_handler))
        .fallback(serve_jb_tv)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], ports.https));
    tracing::debug!("listening on {}", addr);
    tokio::try_join!(
        axum_server::bind_rustls(addr, tls_config).serve(app.into_make_service()),
        redirect_http_to_https(ports),
    )
    .unwrap();
}

async fn play_handler(
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

    if matches!(state.config.ecast.op_mode, EcastOpMode::Proxy) {
        ws.protocols(["ecast-v0"])
            .on_upgrade(move |socket| ws::handle_socket_proxy(host, socket, code, url_query))
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

async fn serve_jb_tv(
    axum::extract::State(state): axum::extract::State<State>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
) -> Result<Response, (StatusCode, String)> {
    if uri.query().is_some_and(|q| q.contains("&s=")) {
        let mut new_query: String = format!("{}?", uri.path());
        new_query.extend(
            uri.query()
                .unwrap()
                .split('&')
                .filter(|q| !q.starts_with("s="))
                .flat_map(|q| [q, "&"]),
        );
        new_query.pop();
        let mut parts = uri.into_parts();
        parts.path_and_query = Some(PathAndQuery::try_from(new_query).unwrap());

        return Ok(Redirect::to(&Uri::from_parts(parts).unwrap().to_string()).into_response());
    }
    if headers
        .get(USER_AGENT)
        .map(|a| a.to_str().unwrap().starts_with("JackboxGames"))
        .unwrap_or_default()
    {
        tracing::error!(uri = ?uri, "unknown endpoint");
        Ok(Json(json! ({ "ok": true, "body": {} })).into_response())
    } else {
        return state
            .http_cache
            .get_cached(
                uri,
                headers,
                state.offline,
                &state.config.accessible_host,
                &state.config.cache_path,
            )
            .await;
        // Ok(().into_response())
    }
}

async fn rooms_handler(
    axum::extract::State(state): axum::extract::State<State>,
    Json(room_req): Json<RoomRequest>,
) -> Json<JBResponse<RoomResponse>> {
    let mut code;
    let token;
    let host;
    match state.config.ecast.op_mode {
        EcastOpMode::Proxy => {
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
        EcastOpMode::Native => {
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
        Arc::new(Room {
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

#[derive(Deserialize, Serialize, Debug)]
struct BlobcastRoomResponse {
    create: bool,
    server: String,
}

async fn rooms_blobcast_handler(
    axum::extract::State(state): axum::extract::State<State>,
) -> Json<BlobcastRoomResponse> {
    match state.config.ecast.op_mode {
        EcastOpMode::Proxy => {
            let url = format!(
                "{}/room",
                state
                    .config
                    .ecast
                    .server_url
                    .as_ref()
                    .map(|s| s.as_str())
                    .unwrap_or("https://blobcast.jackboxgames.com")
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

            tracing::debug!(
                url = url,
                response = ?response,
                "blobcast request"
            );

            response.server = state.config.accessible_host.to_owned();

            tracing::debug!(
                url = url,
                response = ?response,
                "blobcast request"
            );

            return Json(response);
        }
        EcastOpMode::Native => {
            unimplemented!()
        }
    }

    // state.room_map.insert(
    //     code.clone(),
    //     Arc::new(Room {
    //         entities: DashMap::new(),
    //         connections: DashMap::new(),
    //         room_serial: 1.into(),
    //         room_config: JBRoom {
    //             app_id: room_req.app_id,
    //             app_tag: room_req.app_tag.clone(),
    //             audience_enabled: room_req.audience_enabled,
    //             code: code.clone(),
    //             host: state.config.accessible_host.clone(),
    //             audience_host: state.config.accessible_host.clone(),
    //             locked: false,
    //             full: false,
    //             moderation_enabled: false,
    //             password_required: false,
    //             twitch_locked: false, // unimplemented
    //             locale: Cow::Borrowed("en"),
    //             keepalive: false,
    //         },
    //         exit: Notify::new(),
    //     }),
    // );

    // Json(JBResponse {
    //     ok: true,
    //     body: RoomResponse {
    //         host: state.config.accessible_host.clone(),
    //         code,
    //         token,
    //     },
    // })
}

async fn rooms_get_handler(
    axum::extract::State(state): axum::extract::State<State>,
    Path(code): Path<String>,
) -> Result<Json<JBResponse<JBRoom>>, (StatusCode, &'static str)> {
    match state.config.ecast.op_mode {
        EcastOpMode::Native => {
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
        EcastOpMode::Proxy => {
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

async fn app_config_handler(
    Path(code): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    axum::extract::State(state): axum::extract::State<State>,
) -> Json<JBResponse<serde_json::Value>> {
    match state.config.ecast.op_mode {
        EcastOpMode::Native => {
            return Json(JBResponse {
                ok: true,
                body: json!({
                    "settings": {
                        "serverUrl": state.config.accessible_host.clone()
                    }
                }),
            });
        }
        EcastOpMode::Proxy => {
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

async fn redirect_http_to_https(ports: Ports) -> Result<(), std::io::Error> {
    if let Some(port) = ports.http {
        fn make_https(
            host: String,
            uri: Uri,
            http: String,
            https: String,
        ) -> Result<Uri, BoxError> {
            let mut parts = uri.into_parts();

            parts.scheme = Some(axum::http::uri::Scheme::HTTPS);

            if parts.path_and_query.is_none() {
                parts.path_and_query = Some("/".parse().unwrap());
            }

            let https_host = host.replace(&http, &https);
            parts.authority = Some(https_host.parse()?);

            Ok(Uri::from_parts(parts)?)
        }

        let http = format!("{}", port);
        let https = format!("{}", ports.https);
        let redirect = move |Host(host): Host, uri: Uri| async move {
            match make_https(host, uri, http, https) {
                Ok(uri) => Ok(Redirect::permanent(&uri.to_string())),
                Err(error) => {
                    tracing::warn!(%error, "failed to convert URI to HTTPS");
                    Err(StatusCode::BAD_REQUEST)
                }
            }
        };

        let addr = SocketAddr::from(([0, 0, 0, 0], 80));
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        tracing::debug!("listening on {}", listener.local_addr().unwrap());
        axum::serve(listener, redirect.into_make_service()).await
    } else {
        Ok(())
    }
}
