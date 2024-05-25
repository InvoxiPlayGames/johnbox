use std::{
    borrow::Cow,
    fmt::{Debug, Display, LowerHex},
    net::SocketAddr,
    ops::DerefMut,
    path::PathBuf,
    sync::{
        atomic::{AtomicI64, AtomicU64},
        Arc,
    },
    time::Duration,
};

use axum::{
    extract::{
        ws::{Message, WebSocket},
        Host, OriginalUri,
    },
    handler::HandlerWithoutStateExt,
    http::{uri::PathAndQuery, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
    BoxError, Json, Router,
};
use axum_server::tls_rustls::RustlsConfig;
use dashmap::DashMap;
use futures_util::{stream::SplitSink, SinkExt};
use helix_stdx::rope::RegexBuilder;
use rand::{rngs::OsRng, RngCore, SeedableRng};
use rand_xoshiro::Xoshiro128PlusPlus;
use reqwest::header::USER_AGENT;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex, Notify, RwLock};
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use tree_sitter::{Parser, QueryCursor};

mod blobcast;
mod ecast;
mod http_cache;

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

    fn from_seed(s: u64) -> Token {
        let mut t = Token([0u8; 12]);
        Xoshiro128PlusPlus::seed_from_u64(s).fill_bytes(&mut t.0);
        t
    }
}

#[derive(Clone)]
struct State {
    blobcast_host: Arc<RwLock<String>>,
    room_map: Arc<DashMap<String, Arc<Room>>>,
    http_cache: http_cache::HttpCache,
    config: Arc<Config>,
    offline: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum OpMode {
    Native,
    Proxy,
}

#[derive(Deserialize)]
struct Config {
    doodles: DoodleConfig,
    ecast: Ecast,
    blobcast: Ecast,
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
    op_mode: OpMode,
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
    blobcast: u16,
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

#[derive(PartialEq, Eq, Debug)]
pub enum ClientType {
    Blobcast,
    Ecast,
}

type Connections = DashMap<i64, Arc<Client>>;

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct JBProfile {
    pub id: i64,
    user_id: String,
    pub role: ecast::Role,
    name: String,
    roles: serde_json::Value,
}

pub struct Client {
    pub profile: JBProfile,
    socket: Mutex<Option<SplitSink<WebSocket, Message>>>,
    pc: AtomicU64,
    client_type: ClientType,
}

impl Client {
    pub async fn send_ecast(
        &self,
        mut message: ecast::ws::JBMessage<'_>,
    ) -> Result<(), axum::Error> {
        message.pc = self.pc.fetch_add(1, std::sync::atomic::Ordering::AcqRel);

        tracing::debug!(id = self.profile.id, role = ?self.profile.role, ?message, "Sending WS Message");

        if let Err(e) = self
            .send_ws_message(Message::Text(serde_json::to_string(&message).unwrap()))
            .await
        {
            self.disconnect().await;
            return Err(e);
        }

        Ok(())
    }

    pub async fn send_blobcast(
        &self,
        message: blobcast::ws::JBMessage<'_>,
    ) -> Result<(), axum::Error> {
        tracing::debug!(id = self.profile.id, role = ?self.profile.role, ?message, "Sending WS Message");

        if let Err(e) = self
            .send_ws_message(Message::Text(format!(
                "5:::{}",
                serde_json::to_string(&message).unwrap()
            )))
            .await
        {
            self.disconnect().await;
            return Err(e);
        }

        Ok(())
    }

    pub async fn pong(&self, d: Vec<u8>) -> Result<(), axum::Error> {
        if let Err(e) = self.send_ws_message(Message::Pong(d)).await {
            self.disconnect().await;
            return Err(e);
        }

        Ok(())
    }

    pub async fn close(&self) -> Result<(), axum::Error> {
        tracing::debug!(id = self.profile.id, role = ?self.profile.role, "Closing connection");

        self.send_ws_message(Message::Close(Some(axum::extract::ws::CloseFrame {
            code: 1000,
            reason: Cow::Borrowed("normal close"),
        })))
        .await?;

        Ok(())
    }

    pub async fn disconnect(&self) {
        *self.socket.lock().await = None;
    }

    pub async fn send_ws_message(&self, message: Message) -> Result<(), axum::Error> {
        if let Some(socket) = self.socket.lock().await.deref_mut() {
            tokio::select! {
                r = socket.send(message) => r?,
                _ = tokio::time::sleep(Duration::from_secs(3)) => {
                    tracing::error!(id = self.profile.id, role = ?self.profile.role, "Connection timed out");
                    self.disconnect().await;
                }
            }
        }

        Ok(())
    }
}

pub struct Room {
    pub entities: DashMap<String, ecast::entity::JBEntity>,
    pub connections: Connections,
    pub room_serial: AtomicI64,
    pub room_config: JBRoom,
    pub exit: Notify,
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
        .build("blobcast.jackboxgames.com|ecast.jackboxgames.com|bundles.jackbox.tv|jackbox.tv|cdn.jackboxgames.com|s3.amazonaws.com")
        .unwrap();
    let content_to_compress = RegexBuilder::new().build("text/html|text/css|text/xml|text/javascript|application/javascript|application/x-javascript|application/json").unwrap();

    let js_lang = tree_sitter_javascript::language();
    let fragment_query = tree_sitter::Query::new(&js_lang, "(string_fragment) @frag").unwrap();
    let css_lang = tree_sitter_css::language();
    let css_query = tree_sitter::Query::new(&css_lang, "(plain_value) @frag").unwrap();
    let state = State {
        blobcast_host: Arc::new(RwLock::new(String::new())),
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
        .route("/api/v2/rooms/:code/play", get(ecast::play_handler))
        .route("/api/v2/audience/:code/play", get(ecast::play_handler))
        .route("/api/v2/rooms", post(ecast::rooms_handler))
        .route(
            "/@ecast.jackboxgames.com/api/v2/rooms/:code",
            get(ecast::rooms_get_handler),
        )
        .route(
            "/api/v2/app-configs/:app_tag",
            get(ecast::app_config_handler),
        )
        .route("/room", get(blobcast::rooms_handler))
        .route("/accessToken", post(blobcast::access_token_handler))
        .route("/socket.io/1", get(blobcast::load_handler))
        .route("/socket.io/1/websocket/:id", get(blobcast::play_handler))
        .fallback(serve_jb_tv)
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], ports.https));
    tracing::debug!("listening on {}", addr);
    let blobcast_addr = SocketAddr::from(([0, 0, 0, 0], ports.blobcast));
    tokio::try_join!(
        axum_server::bind_rustls(addr, tls_config.clone()).serve(app.clone().into_make_service()),
        axum_server::bind_rustls(blobcast_addr, tls_config).serve(app.into_make_service()),
        redirect_http_to_https(ports),
    )
    .unwrap();
}

async fn serve_jb_tv(
    axum::extract::State(state): axum::extract::State<State>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
) -> Result<Response, (StatusCode, String)> {
    tracing::error!(?uri);
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

pub fn room_id() -> String {
    fn random(size: usize) -> Vec<u8> {
        let mut bytes: Vec<u8> = vec![0; size];

        OsRng.fill_bytes(&mut bytes);

        bytes
    }
    const ALPHA_CAPITAL: [char; 26] = [
        'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R',
        'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
    ];
    let mut code = nanoid::nanoid!(4, &ALPHA_CAPITAL, random);
    code.make_ascii_uppercase();
    code
}

async fn redirect_http_to_https(ports: Ports) -> Result<(), std::io::Error> {
    if let Some(port) = ports.http {
        fn make_https(
            host: String,
            uri: Uri,
            http: String,
            https: String,
        ) -> Result<Uri, BoxError> {
            tracing::debug!(host, ?uri, http, https, "Received HTTP request");
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
