use std::{
    pin::pin,
    time::{Duration, Instant},
};
use actix_ws::{Message, MessageStream, Session};
use futures_util::{
    future::{select, Either},
    StreamExt as _,
};
use tokio::{sync::mpsc, time::interval};
use serde_json::Value;
use crate::server::{ChatServerHandle, ConnId, EncryptedMessage, UserProfile};

/// How often heartbeat pings are sent
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// How long before lack of client response causes a timeout
const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(serde::Deserialize)]
struct ClientEvent {
    event: String,
    data: Value,
}

#[derive(serde::Deserialize)]
struct SendMessageData {
    message: EncryptedMessage,
    is_group_chat: bool,
    group_code: Option<String>,
}

#[derive(serde::Deserialize)]
struct TypingData {
    is_group_chat: bool,
    group_code: Option<String>,
}

/// Handle WebSocket connections, process messages, and maintain connection health
pub async fn chat_ws(
    chat_server: ChatServerHandle,
    mut session: Session,
    mut msg_stream: MessageStream,
) {
    log::info!("WebSocket connection established");
    
    let mut last_heartbeat = Instant::now();
    let mut interval = interval(HEARTBEAT_INTERVAL);
    
    // Create a channel for this connection
    let (conn_tx, mut conn_rx) = mpsc::unbounded_channel();
    
    // Register with the chat server and get a connection ID
    let conn_id = chat_server.connect(conn_tx).await;
    log::info!("Client connected with ID: {}", conn_id);
    
    let close_reason = loop {
        // Set up the futures we'll select between
        let tick = pin!(interval.tick());
        let msg_rx = pin!(conn_rx.recv());
        let ws_msg = pin!(msg_stream.next());
        
        let messages = pin!(select(ws_msg, msg_rx));
        
        match select(messages, tick).await {
            // Messages from client
            Either::Left((Either::Left((Some(Ok(msg)), _)), _)) => {
                log::debug!("Received message: {:?}", msg);
                last_heartbeat = Instant::now();
                
                match msg {
                    Message::Ping(bytes) => {
                        if let Err(e) = session.pong(&bytes).await {
                            log::error!("Failed to send pong: {}", e);
                            break None;
                        }
                    }
                    Message::Pong(_) => {
                        // Heartbeat received, nothing to do
                    }
                    Message::Text(text) => {
                        process_text_msg(&chat_server, &text, conn_id.clone()).await;
                    }
                    Message::Binary(_) => {
                        log::warn!("Unexpected binary message");
                    }
                    Message::Close(reason) => break reason,
                    Message::Continuation(_) => {
                        log::warn!("Received continuation frame, which should be handled by actix-ws");
                    }
                    Message::Nop => {}
                }
            }
            
            // Client WebSocket stream error
            Either::Left((Either::Left((Some(Err(err)), _)), _)) => {
                log::error!("WebSocket error: {}", err);
                break None;
            }
            
            // Client WebSocket stream ended
            Either::Left((Either::Left((None, _)), _)) => {
                log::info!("WebSocket connection closed by client");
                break None;
            }
            
            // Messages from chat server to be sent to client
            Either::Left((Either::Right((Some(chat_msg), _)), _)) => {
                if let Err(e) = session.text(chat_msg).await {
                    log::error!("Failed to send message to client: {}", e);
                    break None;
                }
            }
            
            // All connection's message senders were dropped
            Either::Left((Either::Right((None, _)), _)) => {
                log::error!("All connection message senders were dropped; chat server may have panicked");
                break None;
            }
            
            // Heartbeat tick
            Either::Right((_, _)) => {
                // Check if client is still responsive
                if Instant::now().duration_since(last_heartbeat) > CLIENT_TIMEOUT {
                    log::info!("Client has not sent heartbeat in over {:?}; disconnecting", CLIENT_TIMEOUT);
                    break None;
                }
                
                // Send heartbeat ping
                if let Err(e) = session.ping(b"").await {
                    log::error!("Failed to send ping: {}", e);
                    break None;
                }
            }
        }
    };
    
    // Clean up when the connection ends
    chat_server.disconnect(conn_id);
    log::info!("WebSocket connection closed");
    
    // Attempt to close connection gracefully
    let _ = session.close(close_reason).await;
}

async fn process_text_msg(
    chat_server: &ChatServerHandle,
    text: &str,
    conn_id: ConnId,
) {
    // Try to parse the message as a ClientEvent
    if let Ok(client_event) = serde_json::from_str::<ClientEvent>(text) {
        match client_event.event.as_str() {
            "join_chat" => {
                if let Ok(profile) = serde_json::from_value::<UserProfile>(client_event.data) {
                    log::info!("User joining chat: {}", profile.username);
                    chat_server.join_chat(conn_id, profile).await;
                } else {
                    log::error!("Failed to parse join_chat data");
                }
            }
            "send_message" => {
                if let Ok(data) = serde_json::from_value::<SendMessageData>(client_event.data) {
                    chat_server.send_message(
                        conn_id,
                        data.message,
                        data.is_group_chat,
                        data.group_code,
                    ).await;
                } else {
                    log::error!("Failed to parse send_message data");
                }
            }
            "typing_start" => {
                if let Ok(data) = serde_json::from_value::<TypingData>(client_event.data) {
                    chat_server.typing_start(
                        conn_id,
                        data.is_group_chat,
                        data.group_code,
                    ).await;
                } else {
                    log::error!("Failed to parse typing_start data");
                }
            }
            "typing_stop" => {
                if let Ok(data) = serde_json::from_value::<TypingData>(client_event.data) {
                    chat_server.typing_stop(
                        conn_id,
                        data.is_group_chat,
                        data.group_code,
                    ).await;
                } else {
                    log::error!("Failed to parse typing_stop data");
                }
            }
            "disconnect_chat" => {
                chat_server.disconnect_chat(conn_id).await;
            }
            _ => {
                log::warn!("Unknown event type: {}", client_event.event);
            }
        }
    } else {
        log::error!("Failed to parse message as ClientEvent: {}", text);
    }
} 