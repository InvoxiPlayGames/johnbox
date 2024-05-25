use std::{
    borrow::Cow,
    io::{Read, Write},
    process::Stdio,
    str::FromStr,
    sync::Arc,
};

use crate::{
    ecast::{
        acl::Acl,
        entity::{JBAttributes, JBEntity, JBObject, JBRestrictions, JBType, JBValue},
        Role,
    },
    Client, ClientType, JBProfile, Room,
};

use axum::extract::ws::{Message, WebSocket};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    io::Interest,
    sync::{Mutex, Notify},
};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

#[derive(Debug)]
struct WSMessage<'a> {
    opcode: u8,
    message: JBMessage<'a>,
}

impl<'a> FromStr for WSMessage<'a> {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.split_once("::").ok_or(())?;
        let opcode: u8 = s.0.parse().or(Err(()))?;
        let message: JBMessage = serde_json::from_str(&s.1[1..]).or(Err(()))?;

        Ok(Self { opcode, message })
    }
}

#[derive(Deserialize, Serialize, Debug)]
pub struct JBMessage<'a> {
    pub name: Cow<'a, str>,
    pub args: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRoomArgs<'a> {
    action: Cow<'a, str>,
    app_id: String,
    // A lot of telemetry stuff that we don't
    // need to make allocations for
    #[serde(skip)]
    options: (),
    #[serde(rename = "type")]
    arg_type: Cow<'a, str>,
    user_id: Cow<'a, str>,
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
    let create_room: CreateRoomArgs = serde_json::from_value(create_room.message.args).unwrap();
    let room_code = crate::room_id();

    let room = Arc::new(Room {
        entities: DashMap::new(),
        connections: DashMap::new(),
        room_serial: 1.into(),
        room_config: crate::JBRoom {
            app_tag: super::APP_TAGS
                .get(&create_room.app_id)
                .map(|s| s.to_string())
                .unwrap_or_default(),
            app_id: create_room.app_id,
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
            roles: json!({ "host": {} }),
            user_id: create_room.user_id.into_owned(),
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
                if let Some(Ok(ws_message)) = ws_message {
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
                    process_message(&client, message, &room).await
                        .map_err(|e| (Arc::clone(&client), e))?;
                    tracing::debug!("Message Processed");
                } else {
                    break;
                }
            }
            _ = room.exit.notified() => {
                tracing::debug!(room_code, "Removing room");
                room_map.remove(&room_code);
                break
            }
        }
    }

    client.disconnect().await;
    tracing::debug!(id = client.profile.id, role = ?client.profile.role, "Leaving room");

    Ok(())
}

async fn process_message(
    client: &Client,
    message: WSMessage<'_>,
    room: &Room,
) -> Result<(), axum::Error> {
    match message.message.args.get("action").and_then(|a| a.as_str()) {
        Some("SetRoomBlob") => {
            let entity;
            {
                let prev_value = room.entities.get("bc:room");
                entity = JBEntity(
                    JBType::Object,
                    JBObject {
                        key: "bc:room".to_owned(),
                        val: Some(JBValue::Object(
                            message
                                .message
                                .args
                                .get("blob")
                                .and_then(|b| b.as_object().cloned())
                                .unwrap(),
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
                );
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
                                    args: json!([
                                        {
                                            "type": "Event",
                                            "action": "RoomBlobChanged",
                                            "roomId": room.room_config.code,
                                            "blob": entity.1.val.as_ref().unwrap()
                                        }
                                    ]),
                                })
                                .await?;
                        }
                    }
                }
            }
            room.entities.insert("bc:room".to_owned(), entity);
            client
                .send_blobcast(JBMessage {
                    name: Cow::Borrowed("msg"),
                    args: json!([
                        {
                            "type": "Result",
                            "action": "SetRoomBlob",
                            "success": true
                        }
                    ]),
                })
                .await?;
        }
        Some("SetCustomerBlob") => {
            eprintln!("user_id");
            let entity;
            let key;
            {
                let user_id = message
                    .message
                    .args
                    .get("customerUserId")
                    .unwrap()
                    .as_str()
                    .unwrap();
                eprintln!("key");
                key = format!("bc:customer:{}", user_id);
                eprintln!("prev_value");
                let prev_value = room.entities.get(&key);
                eprintln!("connection");
                let connection = room
                    .connections
                    .iter()
                    .find(|c| c.profile.user_id == user_id)
                    .unwrap();
                eprintln!("entity");
                entity = JBEntity(
                    JBType::Object,
                    JBObject {
                        key: key.clone(),
                        val: Some(JBValue::Object(
                            message
                                .message
                                .args
                                .get("blob")
                                .and_then(|b| b.as_object().cloned())
                                .unwrap(),
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
                            principle: crate::ecast::acl::Principle::Id(*connection.key()),
                        }],
                    },
                );
                eprintln!("value");
                let value = serde_json::to_value(&entity.1).unwrap();
                eprintln!("send ecast");
                connection
                    .send_ecast(crate::ecast::ws::JBMessage {
                        pc: 0,
                        re: None,
                        opcode: Cow::Borrowed("object"),
                        result: &value,
                    })
                    .await?;
                eprintln!("insert");
            }
            room.entities.insert(key, entity);
            eprintln!("done");
        }
        Some("LockRoom") => {
            client
                .send_blobcast(JBMessage {
                    name: Cow::Borrowed("msg"),
                    args: json!([
                        {
                            "type": "Result",
                            "action": "LockRoom",
                            "success": true,
                            "roomId": room.room_config.code
                        }
                    ]),
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
