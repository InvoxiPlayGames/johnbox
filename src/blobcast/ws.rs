use std::{
    borrow::Cow,
    io::{Read, Write},
    process::Stdio,
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use crate::{
    acl::{Acl, Role},
    entity::{JBAttributes, JBEntity, JBObject, JBRestrictions, JBType, JBValue},
    Client, ClientType, JBProfile, Room,
};

use axum::extract::ws::{Message, WebSocket};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::{
    io::Interest,
    sync::{Mutex, Notify},
};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

#[derive(Debug)]
struct WSMessage<'a> {
    _opcode: u8,
    message: Option<JBMessage<'a>>,
}

impl<'a> FromStr for WSMessage<'a> {
    type Err = Option<u8>;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.split_once("::").ok_or(None)?;
        let opcode: u8 = s.0.parse().or(Err(None))?;
        let message: Option<JBMessage> = if opcode == 5 {
            serde_json::from_str(&s.1.get(1..).ok_or(Some(opcode))?).or(Err(Some(opcode)))?
        } else {
            None
        };

        Ok(Self {
            _opcode: opcode,
            message,
        })
    }
}

#[derive(Deserialize, Serialize, Debug)]
pub struct JBMessage<'a> {
    pub name: Cow<'a, str>,
    pub args: JBMessageArgs<'a>,
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(untagged)]
pub enum JBMessageArgs<'a> {
    Object(JBArgs<'a>),
    Array([JBArgs<'a>; 1]),
}

impl<'a> JBMessageArgs<'a> {
    pub fn get_args(&self) -> &JBArgs<'a> {
        match self {
            JBMessageArgs::Object(a) => a,
            JBMessageArgs::Array(a) => &a[0],
        }
    }
}

fn object_is_empty(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => true,
        serde_json::Value::String(s) => s.is_empty(),
        serde_json::Value::Array(a) => a.is_empty(),
        serde_json::Value::Object(o) => o.is_empty(),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => false,
    }
}

#[derive(Deserialize, Serialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct JBArgs<'a> {
    #[serde(default)]
    #[serde(skip_serializing_if = "str::is_empty")]
    pub action: Cow<'a, str>,
    #[serde(default)]
    #[serde(skip_serializing_if = "str::is_empty")]
    pub event: Cow<'a, str>,
    #[serde(default)]
    #[serde(skip_serializing_if = "String::is_empty")]
    pub app_id: String,
    #[serde(default)]
    #[serde(skip_serializing_if = "object_is_empty")]
    pub options: serde_json::Value,
    #[serde(rename = "type")]
    #[serde(skip_serializing_if = "str::is_empty")]
    pub arg_type: Cow<'a, str>,
    #[serde(default)]
    #[serde(skip_serializing_if = "object_is_empty")]
    pub blob: serde_json::Value,
    #[serde(default)]
    #[serde(skip_serializing_if = "object_is_empty")]
    pub message: serde_json::Value,
    #[serde(default)]
    #[serde(skip_serializing_if = "str::is_empty")]
    pub user_id: Cow<'a, str>,
    #[serde(default)]
    #[serde(skip_serializing_if = "str::is_empty")]
    pub customer_user_id: Cow<'a, str>,
    #[serde(default)]
    #[serde(skip_serializing_if = "str::is_empty")]
    pub customer_name: Cow<'a, str>,
    #[serde(default)]
    #[serde(skip_serializing_if = "str::is_empty")]
    pub room_id: Cow<'a, str>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
}

pub async fn handle_socket(
    socket: WebSocket,
    room_map: Arc<DashMap<String, Arc<Room>>>,
    host: String,
) -> Result<(), (Arc<Client>, axum::Error)> {
    let (mut ws_write, mut ws_read) = socket.split();

    ws_write
        .send(Message::Text("1::".to_owned()))
        .await
        .unwrap();

    let Message::Text(create_room) = ws_read.next().await.unwrap().unwrap() else {
        unreachable!()
    };

    let create_room = WSMessage::from_str(&create_room).unwrap();
    let room_code = crate::room_id();

    let room = Arc::new(Room {
        entities: DashMap::new(),
        connections: DashMap::new(),
        room_serial: 1.into(),
        room_config: crate::JBRoom {
            app_tag: super::APP_TAGS
                .get(&create_room.message.as_ref().unwrap().args.get_args().app_id)
                .map(|s| s.to_string())
                .unwrap_or_default(),
            app_id: create_room
                .message
                .as_ref()
                .unwrap()
                .args
                .get_args()
                .app_id
                .clone(),
            audience_enabled: false,
            code: room_code.clone(),
            audience_host: host.clone(),
            host,
            locked: false,
            full: false,
            moderation_enabled: false,
            password_required: false,
            twitch_locked: false,
            locale: Cow::Borrowed("en"),
            keepalive: false,
        },
        exit: Notify::new(),
    });

    room_map.insert(room_code.clone(), Arc::clone(&room));

    ws_write
        .send(Message::Text(format!(
            "5:::{{\
                \"name\": \"msg\",\
                \"args\": [\
                    {{\
                        \"type\": \"Result\",\
                        \"action\": \"CreateRoom\",\
                        \"success\": true,\
                        \"roomId\": \"{}\"\
                    }}\
                ]\
            }}",
            room_code
        )))
        .await
        .unwrap();

    let client: Arc<Client> = {
        let serial = room
            .room_serial
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);

        let profile = JBProfile {
            id: serial,
            roles: crate::JBProfileRoles::Host {},
            user_id: create_room
                .message
                .as_ref()
                .unwrap()
                .args
                .get_args()
                .user_id
                .clone()
                .into_owned(),
            role: Role::Host,
            name: String::new(),
        };

        let client = Arc::new(Client {
            pc: 0.into(),
            profile,
            socket: Mutex::new(Some(ws_write)),
            client_type: ClientType::Blobcast,
        });
        room.connections.insert(serial, Arc::clone(&client));
        client
    };

    'outer: loop {
        tokio::select! {
            ws_message = ws_read.next() => {
                match ws_message {
                    Some(Ok(ws_message)) => {
                        let message: WSMessage = match ws_message {
                            Message::Text(ref t) => WSMessage::from_str(t).unwrap(),
                            Message::Close(_) => break 'outer,
                            Message::Ping(d) => {
                                client.pong(d).await
                                    .map_err(|e| (Arc::clone(&client), e))?;
                                continue;
                            }
                            _ => continue,
                        };
                        tracing::debug!(id = client.profile.id, role = ?client.profile.role, ?message, "Recieved WS Message");
                        if let Some(ref message) = message.message {
                            process_message(&client, message, &room).await
                                .map_err(|e| (Arc::clone(&client), e))?;
                        }
                        tracing::debug!("Message Processed");
                    }
                    Some(Err(e)) => {
                        tracing::error!(id = client.profile.id, role = ?client.profile.role, ?e, "Error in receiving message");
                    }
                    None => {
                        break
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(5)) => {
                client.send_ws_message(Message::Text(String::from("2:::"))).await
                    .map_err(|e| (Arc::clone(&client), e))?;
            }
            _ = room.exit.notified() => {
                break
            }
        }
    }

    client.disconnect().await;
    tracing::debug!(id = client.profile.id, role = ?client.profile.role, "Leaving room");
    room.exit.notify_waiters();
    tracing::debug!(room_code, "Removing room");
    room_map.remove(&room_code);

    Ok(())
}

async fn process_message(
    client: &Client,
    message: &JBMessage<'_>,
    room: &Room,
) -> Result<(), axum::Error> {
    match message.args.get_args().action.as_ref() {
        "SetRoomBlob" => {
            let entity = {
                let prev_value = room.entities.get("bc:room");
                JBEntity(
                    JBType::Object,
                    JBObject {
                        key: "bc:room".to_owned(),
                        val: Some(JBValue::Object(
                            message.args.get_args().blob.as_object().cloned().unwrap(),
                        )),
                        restrictions: JBRestrictions::default(),
                        version: prev_value
                            .as_ref()
                            .map(|p| p.value().1.version + 1)
                            .unwrap_or_default(),
                        from: client.profile.id.into(),
                    },
                    JBAttributes {
                        locked: false.into(),
                        acl: Acl::default_vec(),
                    },
                )
            };
            let value = serde_json::to_value(&entity.1).unwrap();
            for client in room.connections.iter() {
                match client.client_type {
                    ClientType::Ecast => {
                        client
                            .send_ecast(crate::ecast::ws::JBMessage {
                                pc: 0,
                                re: None,
                                opcode: Cow::Borrowed("object"),
                                result: &value,
                            })
                            .await?;
                    }
                    ClientType::Blobcast => {
                        client
                            .send_blobcast(JBMessage {
                                name: Cow::Borrowed("msg"),
                                args: JBMessageArgs::Array([JBArgs {
                                    arg_type: Cow::Borrowed("Event"),
                                    action: Cow::Borrowed("RoomBlobChanged"),
                                    room_id: Cow::Borrowed(&room.room_config.code),
                                    blob: serde_json::to_value(&entity.1.val).unwrap(),
                                    ..Default::default()
                                }]),
                            })
                            .await?;
                    }
                }
            }
            room.entities.insert("bc:room".to_owned(), entity);
            client
                .send_blobcast(JBMessage {
                    name: Cow::Borrowed("msg"),
                    args: JBMessageArgs::Array([JBArgs {
                        arg_type: Cow::Borrowed("Result"),
                        action: Cow::Borrowed("SetRoomBlob"),
                        success: Some(true),
                        ..Default::default()
                    }]),
                })
                .await?;
        }
        "SetCustomerBlob" => {
            let user_id = message.args.get_args().customer_user_id.as_ref();
            let key = format!("bc:customer:{}", user_id);
            let connection = room
                .connections
                .iter()
                .find(|c| c.profile.user_id == user_id)
                .unwrap();
            let entity = {
                let prev_value = room.entities.get(&key);
                JBEntity(
                    JBType::Object,
                    JBObject {
                        key: key.clone(),
                        val: Some(JBValue::Object(
                            message.args.get_args().blob.as_object().cloned().unwrap(),
                        )),
                        restrictions: JBRestrictions::default(),
                        version: prev_value
                            .as_ref()
                            .map(|p| p.value().1.version + 1)
                            .unwrap_or_default(),
                        from: client.profile.id.into(),
                    },
                    JBAttributes {
                        locked: false.into(),
                        acl: vec![Acl {
                            interest: Interest::READABLE,
                            principle: crate::acl::Principle::Id(*connection.key()),
                        }],
                    },
                )
            };
            let value = serde_json::to_value(&entity.1).unwrap();
            connection
                .send_ecast(crate::ecast::ws::JBMessage {
                    pc: 0,
                    re: None,
                    opcode: Cow::Borrowed("object"),
                    result: &value,
                })
                .await?;
            room.entities.insert(key, entity);
        }
        "LockRoom" => {
            client
                .send_blobcast(JBMessage {
                    name: Cow::Borrowed("msg"),
                    args: JBMessageArgs::Array([JBArgs {
                        arg_type: Cow::Borrowed("Result"),
                        action: Cow::Borrowed("LockRoom"),
                        success: Some(true),
                        room_id: Cow::Borrowed(&room.room_config.code),
                        ..Default::default()
                    }]),
                })
                .await?;
        }
        a => unimplemented!("Unimplemented blobcast action {:?}", a),
    }

    Ok(())
}

pub async fn handle_socket_proxy(host: String, socket: WebSocket, ecast_req: String) {
    tracing::debug!(host = host, "proying blobcast to");
    let ecast_req = ecast_req.into_client_request().unwrap();
    let (ecast_connection, _) = tokio_tungstenite::connect_async(ecast_req).await.unwrap();

    let (local_write, local_read) = socket.split();

    let (ecast_write, ecast_read) = ecast_connection.split();

    let local_to_ecast = local_read
        .map(move |m| {
            let m = match m.unwrap() {
                axum::extract::ws::Message::Text(m) => {
                    let mut sm = m.split(":::");
                    let opcode = sm.next().unwrap();
                    let json_message = sm.next();
                    tracing::debug!(
                        role = ?Role::Host,
                        opcode = opcode,
                        message = %{
                            json_message.map(|m| {
                                let jq = std::process::Command::new("jq")
                                    .stdin(Stdio::piped())
                                    .stdout(Stdio::piped())
                                    .arg("-C")
                                    .spawn()
                                    .unwrap();
                                let mut jq_in = jq.stdin.unwrap();
                                let mut jq_out = jq.stdout.unwrap();
                                jq_in.write_all(m.as_bytes()).unwrap();
                                jq_in.write_all(b"\n").unwrap();
                                drop(jq_in);
                                let mut jm = String::new();
                                jq_out.read_to_string(&mut jm).unwrap();
                                jm
                            }).unwrap_or_default()
                        },
                        "to blobcast",
                    );
                    return Ok(tokio_tungstenite::tungstenite::Message::Text(m));
                }
                axum::extract::ws::Message::Binary(m) => {
                    Ok(tokio_tungstenite::tungstenite::Message::Binary(m))
                }
                axum::extract::ws::Message::Ping(m) => {
                    Ok(tokio_tungstenite::tungstenite::Message::Ping(m))
                }
                axum::extract::ws::Message::Pong(m) => {
                    Ok(tokio_tungstenite::tungstenite::Message::Pong(m))
                }
                axum::extract::ws::Message::Close(m) => {
                    Ok(tokio_tungstenite::tungstenite::Message::Close(m.map(|f| {
                        tokio_tungstenite::tungstenite::protocol::CloseFrame {
                            code: f.code.into(),
                            reason: f.reason,
                        }
                    })))
                }
            };
            tracing::debug!(
                role = ?Role::Host,
                message = ?m,
                "to blobcast",
            );
            m
        })
        .forward(ecast_write);

    let ecast_to_local = ecast_read
        .map(|m| {
            let m = match m.unwrap() {
                tokio_tungstenite::tungstenite::Message::Text(m) => {
                    let mut sm = m.split(":::");
                    let opcode = sm.next().unwrap();
                    let json_message = sm.next();
                    tracing::debug!(
                        role = ?Role::Host,
                        opcode = opcode,
                        message = %{
                            json_message.map(|m| {
                                let jq = std::process::Command::new("jq")
                                    .stdin(Stdio::piped())
                                    .stdout(Stdio::piped())
                                    .arg("-C")
                                    .spawn()
                                    .unwrap();
                                let mut jq_in = jq.stdin.unwrap();
                                let mut jq_out = jq.stdout.unwrap();
                                jq_in.write_all(m.as_bytes()).unwrap();
                                jq_in.write_all(b"\n").unwrap();
                                drop(jq_in);
                                let mut jm = String::new();
                                jq_out.read_to_string(&mut jm).unwrap();
                                jm
                            }).unwrap_or_default()
                        },
                        "blobcast to",
                    );
                    return Ok(axum::extract::ws::Message::Text(m));
                }
                tokio_tungstenite::tungstenite::Message::Binary(m) => {
                    Ok(axum::extract::ws::Message::Binary(m))
                }
                tokio_tungstenite::tungstenite::Message::Ping(m) => {
                    Ok(axum::extract::ws::Message::Ping(m))
                }
                tokio_tungstenite::tungstenite::Message::Pong(m) => {
                    Ok(axum::extract::ws::Message::Pong(m))
                }
                tokio_tungstenite::tungstenite::Message::Close(m) => {
                    Ok(axum::extract::ws::Message::Close(m.map(|f| {
                        axum::extract::ws::CloseFrame {
                            code: f.code.into(),
                            reason: f.reason,
                        }
                    })))
                }
                tokio_tungstenite::tungstenite::Message::Frame(_) => unimplemented!(),
            };
            tracing::debug!(
                role = ?Role::Host,
                message = ?m,
                "blobcast to",
            );
            m
        })
        .forward(local_write);

    tokio::pin!(local_to_ecast, ecast_to_local);

    tokio::select! {
        _ = local_to_ecast => {}
        _ = ecast_to_local => {}
    }
}
