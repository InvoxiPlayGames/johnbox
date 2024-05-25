use std::{
    io::{Read, Write},
    process::Stdio,
};

use axum::extract::ws::WebSocket;
use futures_util::StreamExt;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

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
