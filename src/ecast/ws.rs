use std::{
    borrow::Cow,
    io::{Read, Write},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use axum::extract::{
    ws::{Message, WebSocket},
    Path, Query,
};
use dashmap::DashMap;
use futures_util::StreamExt;
use serde::{ser::SerializeMap, Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use super::{
    acl::Acl,
    entity::{JBAttributes, JBDoodle, JBEntity, JBLine, JBObject, JBRestrictions, JBType, JBValue},
    Role, WSQuery,
};
use crate::{
    blobcast::ws::{JBArgs, JBMessageArgs},
    Client, ClientType, Connections, DoodleConfig, JBProfile, Room, Token,
};

#[derive(Serialize, Debug)]
pub struct JBMessage<'a> {
    pub pc: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub re: Option<u64>,
    pub opcode: Cow<'a, str>,
    pub result: &'a serde_json::Value,
}

#[derive(Deserialize, Debug)]
struct WSMessage {
    opcode: Cow<'static, str>,
    params: serde_json::Value,
    seq: u64,
}

#[derive(Deserialize, Debug)]
struct JBCreateParams {
    #[serde(default = "Acl::default_vec")]
    acl: Vec<Acl>,
    key: String,
    #[serde(default)]
    val: serde_json::Value,
    #[serde(flatten)]
    restrictions: JBRestrictions,
    #[serde(flatten)]
    doodle: JBDoodle,
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct JBKeyWithLine {
    key: String,
    #[serde(flatten)]
    line: JBLine,
}

#[derive(Deserialize, Debug)]
struct JBKeyParam {
    key: String,
}

#[derive(Deserialize, Debug)]
struct JBClientSendParams {
    #[serde(rename = "from")]
    _from: i64,
    to: i64,
    body: serde_json::Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientWelcome<'a> {
    id: i64,
    secret: Token,
    reconnect: bool,
    device_id: Cow<'static, str>,
    entities: GetEntities<'a>,
    here: GetHere<'a>,
    profile: &'a JBProfile,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientConnected<'a> {
    id: i64,
    user_id: &'a str,
    name: &'a str,
    role: Role,
    reconnect: bool,
    profile: &'a JBProfile,
}

pub async fn handle_socket(
    socket: WebSocket,
    Path(code): Path<String>,
    Query(url_query): Query<WSQuery>,
    room: Arc<Room>,
    doodle_config: &DoodleConfig,
    room_map: Arc<DashMap<String, Arc<Room>>>,
) -> Result<(), (Arc<Client>, axum::Error)> {
    let (ws_write, mut ws_read) = socket.split();

    let (reconnect, client): (bool, Arc<Client>) = {
        if let Some(profile) = room.connections.get(&url_query.id) {
            *profile.value().socket.lock().await = Some(ws_write);
            (true, Arc::clone(profile.value()))
        } else {
            let serial = room
                .room_serial
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);

            let profile = match url_query.role {
                Role::Host => JBProfile {
                    id: serial,
                    roles: json!({ "host": {} }),
                    user_id: url_query.user_id.clone(),
                    role: url_query.role,
                    name: url_query.name,
                },
                Role::Player => JBProfile {
                    id: serial,
                    roles: json!({ "player": { "name": url_query.name.clone() } }),
                    user_id: url_query.user_id.clone(),
                    role: url_query.role,
                    name: url_query.name,
                },
                Role::Audience | Role::Moderator => unimplemented!(),
            };

            let profile = Arc::new(Client {
                pc: 0.into(),
                profile,
                socket: Mutex::new(Some(ws_write)),
                client_type: ClientType::Ecast,
            });
            room.connections.insert(serial, Arc::clone(&profile));
            (false, profile)
        }
    };

    client
        .send_ecast(JBMessage {
            pc: 0,
            re: None,
            opcode: Cow::Borrowed("client/welcome"),
            result: &serde_json::to_value(ClientWelcome {
                id: client.profile.id,
                secret: url_query.secret.unwrap_or_else(|| Token::random()),
                reconnect,
                device_id: Cow::Borrowed("0000000000.0000000000000000000000"),
                entities: GetEntities {
                    entities: &room.entities,
                    role: url_query.role,
                    id: client.profile.id,
                },
                here: GetHere(&room.connections, client.profile.id),
                profile: &client.profile,
            })
            .unwrap(),
        })
        .await
        .map_err(|e| (Arc::clone(&client), e))?;

    {
        if let Some(host) = room.connections.get(&1) {
            match host.value().client_type {
                ClientType::Blobcast => {
                    host.value()
                        .send_blobcast(crate::blobcast::ws::JBMessage {
                            name: Cow::Borrowed("msg"),
                            args: JBMessageArgs::Array([JBArgs {
                                arg_type: Cow::Borrowed("Event"),
                                event: Cow::Borrowed("CustomerJoinedRoom"),
                                room_id: Cow::Borrowed(&room.room_config.code),
                                customer_user_id: Cow::Borrowed(&client.profile.user_id),
                                customer_name: Cow::Borrowed(&client.profile.name),
                                options: json!({
                                    "roomcode": "",
                                    "name": client.profile.name,
                                    "email": "",
                                    "phone": ""
                                }),
                                ..Default::default()
                            }]),
                        })
                        .await
                        .map_err(|e| (Arc::clone(&client), e))?;
                }
                ClientType::Ecast => {
                    let client_connected = serde_json::to_value(ClientConnected {
                        id: client.profile.id,
                        user_id: &client.profile.user_id,
                        name: &client.profile.name,
                        role: client.profile.role,
                        reconnect,
                        profile: &client.profile,
                    })
                    .unwrap();

                    host.value()
                        .send_ecast(JBMessage {
                            pc: 0,
                            re: None,
                            opcode: Cow::Borrowed("client/connected"),
                            result: &client_connected,
                        })
                        .await
                        .map_err(|e| (Arc::clone(&client), e))?;
                }
            }
        }
    }

    'outer: loop {
        tokio::select! {
            ws_message = ws_read.next() => {
                match ws_message {
                    Some(Ok(ws_message)) => {
                        let message: WSMessage = match ws_message {
                            Message::Text(ref t) => serde_json::from_str(t).unwrap(),
                            Message::Close(_) => break 'outer,
                            Message::Ping(d) => {
                                client.pong(d).await
                                    .map_err(|e| (Arc::clone(&client), e))?;
                                continue;
                            }
                            _ => continue,
                        };
                        tracing::debug!(id = client.profile.id, role = ?client.profile.role, ?message, "Recieved WS Message");
                        process_message(&client, message, &room, doodle_config).await
                            .map_err(|e| (Arc::clone(&client), e))?;
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
                client.ping(b"jackbox".to_vec()).await
                    .map_err(|e| (Arc::clone(&client), e))?;
            }
            _ = room.exit.notified() => {
                tracing::debug!(code, "Removing room");
                room_map.remove(&code);
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
    message: WSMessage,
    room: &Room,
    doodle_config: &DoodleConfig,
) -> Result<(), axum::Error> {
    let mut split = message.opcode.split('/');
    let scope = split.next().unwrap();
    let action = split.next();
    match action {
        Some("create" | "set" | "update")
            if matches!(scope, "text" | "number" | "object" | "doodle") =>
        {
            let params: JBCreateParams = serde_json::from_value(message.params).unwrap();
            let entity = {
                let prev_value = room.entities.get(&params.key);
                let has_been_created = prev_value.is_some();
                let is_unlocked = prev_value.as_ref().is_some_and(|pv| {
                    !pv.value()
                        .2
                        .locked
                        .load(std::sync::atomic::Ordering::Acquire)
                });
                let has_perms = prev_value.as_ref().is_some_and(|p| {
                    p.value()
                        .2
                        .perms(client.profile.role, client.profile.id)
                        .is_some_and(|i| i.is_writable())
                });
                if !(has_been_created || is_unlocked || has_perms)
                    && client.profile.role != Role::Host
                {
                    tracing::error!(id = client.profile.id, role = ?client.profile.role, acl = ?prev_value.as_ref().map(|pv| pv.value().2.acl.as_slice()), has_been_created, is_unlocked, has_perms, "Returned to sender");
                    client
                        .send_ecast(JBMessage {
                            pc: 0,
                            re: None,
                            opcode: Cow::Borrowed("error"),
                            result: &json!("Permission denied"),
                        })
                        .await?;

                    return Ok(());
                }
                let jb_type: JBType = scope
                    .parse()
                    .map_err(|_| axum::Error::new(format!("Invalid JBType {}", scope)))?;
                JBEntity(
                    jb_type,
                    JBObject {
                        key: params.key.clone(),
                        val: match jb_type {
                            JBType::Text => match params.val {
                                serde_json::Value::String(s) => Some(JBValue::Text(s)),
                                serde_json::Value::Null => None,
                                _ => unreachable!(),
                            },
                            JBType::Number => match params.val {
                                serde_json::Value::Number(n) => {
                                    Some(JBValue::Number(n.as_f64().unwrap()))
                                }
                                serde_json::Value::Null => None,
                                _ => unreachable!(),
                            },
                            JBType::Object => match params.val {
                                serde_json::Value::Object(o) => Some(JBValue::Object(o)),
                                serde_json::Value::Null => None,
                                _ => unreachable!(),
                            },
                            JBType::Doodle => Some(JBValue::Doodle(params.doodle)),
                        },
                        restrictions: params.restrictions,
                        version: prev_value
                            .as_ref()
                            .map(|p| p.value().1.version + 1)
                            .unwrap_or_default(),
                        from: client.profile.id.into(),
                    },
                    JBAttributes {
                        locked: false.into(),
                        acl: prev_value
                            .map(|pv| pv.value().2.acl.clone())
                            .unwrap_or(params.acl),
                    },
                )
            };
            let value = serde_json::to_value(&entity.1).unwrap();
            for client in room
                .connections
                .iter()
                .filter(|c| c.profile.id != client.profile.id)
                .filter(|c| {
                    entity
                        .2
                        .perms(c.profile.role, c.profile.id)
                        .is_some_and(|pv| pv.is_readable())
                })
            {
                client
                    .send_ecast(JBMessage {
                        pc: 0,
                        re: None,
                        opcode: Cow::Borrowed(scope),
                        result: &value,
                    })
                    .await?;
            }
            room.entities.insert(params.key, entity);
            client
                .send_ecast(JBMessage {
                    pc: 0,
                    re: Some(message.seq),
                    opcode: Cow::Borrowed("ok"),
                    result: &json!({}),
                })
                .await?;
        }
        Some("stroke") if scope == "doodle" => {
            let params: JBKeyWithLine = serde_json::from_value(message.params).unwrap();

            if let Some(mut entity) = room.entities.get_mut(&params.key) {
                if let Some(JBValue::Doodle(ref mut doodle)) = entity.value_mut().1.val {
                    let line_value = json!({
                         "key": params.key,
                         "from": client.profile.id,
                         "val": serde_json::to_value(&params.line).unwrap()
                    });
                    doodle.lines.push(params.line);
                    doodle.lines.sort_unstable_by_key(|l| l.index);

                    for client in room
                        .connections
                        .iter()
                        .filter(|c| c.profile.id != client.profile.id)
                        .filter(|c| {
                            entity
                                .2
                                .perms(c.profile.role, c.profile.id)
                                .is_some_and(|pv| pv.is_readable())
                        })
                    {
                        client
                            .send_ecast(JBMessage {
                                pc: 0,
                                re: None,
                                opcode: Cow::Borrowed("doodle/line"),
                                result: &line_value,
                            })
                            .await?;
                    }

                    client
                        .send_ecast(JBMessage {
                            pc: 0,
                            re: Some(message.seq),
                            opcode: Cow::Borrowed("ok"),
                            result: &json!({}),
                        })
                        .await?;
                }
            }
        }
        Some("get") if matches!(scope, "text" | "number" | "object" | "doodle") => {
            let params: JBKeyParam = serde_json::from_value(message.params).unwrap();
            if let Some(entity) = room.entities.get(&params.key) {
                client
                    .send_ecast(JBMessage {
                        pc: 0,
                        re: Some(message.seq),
                        opcode: Cow::Borrowed(scope),
                        result: &serde_json::to_value(&entity.1).unwrap(),
                    })
                    .await?;
            }
        }
        Some("send") if scope == "client" => {
            let params: JBClientSendParams =
                serde_json::from_value(message.params.clone()).unwrap();
            // Only used for blobcast compatibility?
            assert_eq!(params.to, 1);
            if let Some(con) = room.connections.get(&params.to) {
                assert_eq!(con.client_type, ClientType::Blobcast);
                match con.client_type {
                    ClientType::Blobcast => {
                        con.send_blobcast(crate::blobcast::ws::JBMessage {
                            name: Cow::Borrowed("msg"),
                            args: JBMessageArgs::Array([JBArgs {
                                arg_type: Cow::Borrowed("Event"),
                                event: Cow::Borrowed("CustomerMessage"),
                                room_id: Cow::Borrowed(&room.room_config.code),
                                user_id: Cow::Borrowed(&client.profile.user_id),
                                message: params.body,
                                ..Default::default()
                            }]),
                        })
                        .await?;
                    }
                    ClientType::Ecast => {
                        con.send_ecast(JBMessage {
                            pc: 0,
                            re: None,
                            opcode: Cow::Borrowed("client/send"),
                            result: &message.params,
                        })
                        .await?;
                    }
                }
            }
            client
                .send_ecast(JBMessage {
                    pc: 0,
                    re: Some(message.seq),
                    opcode: Cow::Borrowed("ok"),
                    result: &json!({}),
                })
                .await?;
        }
        Some("exit") if scope == "room" => {
            client
                .send_ecast(JBMessage {
                    pc: 0,
                    re: Some(message.seq),
                    opcode: Cow::Borrowed("ok"),
                    result: &json!({}),
                })
                .await?;
            for client in room.connections.iter() {
                client.close().await?;
            }
            room.exit.notify_waiters();
        }
        None => match scope {
            "lock" => {
                let params: JBKeyParam = serde_json::from_value(message.params).unwrap();
                if let Some(entity) = room.entities.get(&params.key) {
                    entity
                        .value()
                        .2
                        .locked
                        .store(true, std::sync::atomic::Ordering::Release);
                    entity
                        .value()
                        .1
                        .from
                        .store(client.profile.id, std::sync::atomic::Ordering::Release);
                    let value = json!({ "key": params.key.clone(), "from": client.profile.id });
                    for client in room
                        .connections
                        .iter()
                        .filter(|c| c.profile.id != client.profile.id)
                        .filter(|c| {
                            entity
                                .2
                                .perms(c.profile.role, c.profile.id)
                                .is_some_and(|pv| pv.is_readable())
                        })
                    {
                        client
                            .send_ecast(JBMessage {
                                pc: 0,
                                re: None,
                                opcode: Cow::Borrowed("lock"),
                                result: &value,
                            })
                            .await?;
                    }

                    if doodle_config.render {
                        if let Some(JBValue::Doodle(ref d)) = entity.value().1.val {
                            d.render()
                                .save_png(doodle_config.path.join(format!("{}.png", entity.key())))
                                .unwrap();
                        }
                    }
                }
                client
                    .send_ecast(JBMessage {
                        pc: 0,
                        re: Some(message.seq),
                        opcode: Cow::Borrowed("ok"),
                        result: &json!({}),
                    })
                    .await?;
            }
            "drop" => {
                let params: JBKeyParam = serde_json::from_value(message.params).unwrap();
                room.entities.remove(&params.key);
                client
                    .send_ecast(JBMessage {
                        pc: 0,
                        re: Some(message.seq),
                        opcode: Cow::Borrowed("ok"),
                        result: &json!({}),
                    })
                    .await?;
            }
            _ => {
                client
                    .send_ecast(JBMessage {
                        pc: 0,
                        re: Some(message.seq),
                        opcode: Cow::Borrowed("ok"),
                        result: &json!({}),
                    })
                    .await?;
            }
        },
        _ => {
            client
                .send_ecast(JBMessage {
                    pc: 0,
                    re: Some(message.seq),
                    opcode: Cow::Borrowed("ok"),
                    result: &json!({}),
                })
                .await?;
        }
    }

    Ok(())
}

struct GetEntities<'a> {
    entities: &'a DashMap<String, JBEntity>,
    role: Role,
    id: i64,
}

impl<'a> Serialize for GetEntities<'a> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(None)?;

        for e in self.entities.iter() {
            if e.value()
                .2
                .perms(self.role, self.id)
                .is_some_and(|i| i.is_readable())
            {
                map.serialize_entry(e.key(), e.value())?;
            }
        }

        map.end()
    }
}

struct GetHere<'a>(&'a Connections, i64);

impl<'a> Serialize for GetHere<'a> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for profile in self.0.iter() {
            if *profile.key() != self.1 {
                map.serialize_entry(profile.key(), &profile.value().profile)?;
            }
        }
        map.end()
    }
}

pub async fn handle_socket_proxy(
    host: String,
    socket: WebSocket,
    ecast_req: String,
    url_query: Query<WSQuery>,
) {
    let role = url_query.role;
    tracing::debug!(host = host, "proying to");
    let mut ecast_req = ecast_req.into_client_request().unwrap();
    ecast_req
        .headers_mut()
        .append("Sec-WebSocket-Protocol", "ecast-v0".parse().unwrap());
    let (ecast_connection, _) = tokio_tungstenite::connect_async(ecast_req).await.unwrap();

    let (local_write, local_read) = socket.split();

    let (ecast_write, ecast_read) = ecast_connection.split();

    let local_to_ecast = local_read
        .map(move |m| {
            let m = match m.unwrap() {
                axum::extract::ws::Message::Text(m) => {
                    tracing::debug!(
                        role = ?role,
                        message = %{
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
                        },
                        "to ecast",
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
                role = ?role,
                message = ?m,
                "to ecast",
            );
            m
        })
        .forward(ecast_write);

    let ecast_to_local = ecast_read
        .map(|m| {
            let m = match m.unwrap() {
                tokio_tungstenite::tungstenite::Message::Text(m) => {
                    tracing::debug!(
                        role = ?role,
                        message = %{
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
                        },
                        "ecast to",
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
                role = ?role,
                message = ?m,
                "ecast to",
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
