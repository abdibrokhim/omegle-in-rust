use actix::prelude::*;
use actix_web::{web, HttpRequest, HttpResponse};
use actix_web_actors::ws;
use serde::{Deserialize, Serialize};
use shuttle_actix_web::ShuttleActixWeb;
use std::collections::HashMap;
use uuid::Uuid;
use rand::Rng;

// ### Message Types

#[derive(Serialize, Deserialize, Clone)]
struct EncryptedMessage {
    encrypted: String,
    nonce: String,
}

#[derive(Deserialize)]
struct ClientEvent {
    event: String,
    data: serde_json::Value,
}

#[derive(Serialize)]
struct ServerEvent {
    event: String,
    data: serde_json::Value,
}

#[derive(Deserialize)]
struct UserProfile {
    user_id: String,
    username: String,
    preference: String,
    gender: String,
    room_type: String,
    group_code: Option<String>,
    group_join_method: Option<String>,
}

#[derive(Deserialize)]
struct SendMessageData {
    message: EncryptedMessage,
    is_group_chat: bool,
    group_code: Option<String>,
}

#[derive(Deserialize)]
struct TypingData {
    is_group_chat: bool,
    group_code: Option<String>,
}

// ### Data Structures

struct User {
    id: String, // socket id
    user_id: String,
    username: String,
    gender: String,
    preference: String,
    room_type: String,
    partner_id: Option<String>,
    group_id: Option<String>,
}

struct Group {
    code: String,
    members: Vec<String>, // socket ids
    usernames: Vec<String>,
}

// ### Chat Server Actor

#[derive(Default)]
struct ChatServer {
    sessions: HashMap<String, Addr<WebSocketActor>>,
    users: HashMap<String, User>,
    waiting_users: HashMap<String, Vec<String>>, // preference -> Vec<socket_id>
    groups: HashMap<String, Group>,
}

impl Actor for ChatServer {
    type Context = Context<Self>;
}

#[derive(Message)]
#[rtype(result = "()")]
struct RegisterUser {
    socket_id: String,
    addr: Addr<WebSocketActor>,
}

#[derive(Message)]
#[rtype(result = "()")]
struct UnregisterUser {
    socket_id: String,
}

#[derive(Message)]
#[rtype(result = "()")]
struct JoinChat {
    socket_id: String,
    profile: UserProfile,
}

#[derive(Message)]
#[rtype(result = "()")]
struct SendChatMessage {
    socket_id: String,
    message: EncryptedMessage,
    is_group_chat: bool,
    group_code: Option<String>,
}

#[derive(Message)]
#[rtype(result = "()")]
struct TypingStart {
    socket_id: String,
    is_group_chat: bool,
    group_code: Option<String>,
}

#[derive(Message)]
#[rtype(result = "()")]
struct TypingStop {
    socket_id: String,
    is_group_chat: bool,
    group_code: Option<String>,
}

#[derive(Message)]
#[rtype(result = "()")]
struct DisconnectChat {
    socket_id: String,
}

impl ChatServer {
    fn generate_group_code(&self) -> String {
        let mut rng = rand::thread_rng();
        (0..6).map(|_| rng.gen_range(0..36).to_string().to_uppercase()).collect()
    }

    fn handle_disconnect(&mut self, socket_id: &str) {
        if let Some(user) = self.users.remove(socket_id) {
            if user.room_type == "group" {
                if let Some(group_id) = user.group_id {
                    if let Some(group) = self.groups.get_mut(&group_id) {
                        group.members.retain(|id| id != socket_id);
                        group.usernames.retain(|name| name != &user.username);
                        if group.members.is_empty() {
                            self.groups.remove(&group_id);
                        } else {
                            for member_id in &group.members {
                                if let Some(addr) = self.sessions.get(member_id) {
                                    addr.do_send(ServerMessage::UserLeftGroup(user.username.clone()));
                                    addr.do_send(ServerMessage::GroupMembersUpdate(group.usernames.clone()));
                                }
                            }
                        }
                    }
                }
            } else {
                if let Some(partner_id) = user.partner_id {
                    if let Some(partner_addr) = self.sessions.get(&partner_id) {
                        partner_addr.do_send(ServerMessage::PartnerDisconnected);
                    }
                    if let Some(partner) = self.users.get_mut(&partner_id) {
                        partner.partner_id = None;
                    }
                }
            }
        }
        for list in self.waiting_users.values_mut() {
            list.retain(|id| id != socket_id);
        }
    }

    fn find_match(&mut self, socket_id: &str) {
        if let Some(user) = self.users.get(socket_id) {
            let preference = &user.preference;
            let match_pool: Vec<String> = self.waiting_users.get(preference).cloned().unwrap_or_default()
                .into_iter()
                .filter(|id| {
                    if let Some(potential_match) = self.users.get(id) {
                        match preference.as_str() {
                            "male" => potential_match.gender == "male",
                            "female" => potential_match.gender == "female",
                            _ => false,
                        }
                    } else {
                        false
                    }
                })
                .collect();
            
            if !match_pool.is_empty() {
                let random_index = rand::random::<usize>() % match_pool.len();
                let partner_id = match_pool[random_index].clone();
                self.connect_users(socket_id, &partner_id);
            } else {
                self.waiting_users.entry(preference.clone()).or_insert_with(Vec::new).push(socket_id.to_string());
                if let Some(addr) = self.sessions.get(socket_id) {
                    addr.do_send(ServerMessage::WaitingForMatch);
                }
            }
        }
    }

    fn connect_users(&mut self, user1_id: &str, user2_id: &str) {
        if let Some(user1) = self.users.get_mut(user1_id) {
            user1.partner_id = Some(user2_id.to_string());
        }
        if let Some(user2) = self.users.get_mut(user2_id) {
            user2.partner_id = Some(user1_id.to_string());
        }
        for list in self.waiting_users.values_mut() {
            list.retain(|id| id != user1_id && id != user2_id);
        }
        if let Some(addr1) = self.sessions.get(user1_id) {
            addr1.do_send(ServerMessage::ChatStarted { group_code: None });
        }
        if let Some(addr2) = self.sessions.get(user2_id) {
            addr2.do_send(ServerMessage::ChatStarted { group_code: None });
        }
    }

    fn create_new_group(&mut self, socket_id: &str) {
        let group_code = self.generate_group_code();
        if let Some(user) = self.users.get_mut(socket_id) {
            let group = Group {
                code: group_code.clone(),
                members: vec![socket_id.to_string()],
                usernames: vec![user.username.clone()],
            };
            self.groups.insert(group_code.clone(), group);
            user.group_id = Some(group_code.clone());
            if let Some(addr) = self.sessions.get(socket_id) {
                addr.do_send(ServerMessage::ChatStarted { group_code: Some(group_code.clone()) });
                addr.do_send(ServerMessage::GroupMembersUpdate(vec![user.username.clone()]));
            }
        }
    }

    fn join_group_by_code(&mut self, socket_id: &str, group_code: &str) {
        if let Some(group) = self.groups.get_mut(group_code) {
            if let Some(user) = self.users.get_mut(socket_id) {
                group.members.push(socket_id.to_string());
                group.usernames.push(user.username.clone());
                user.group_id = Some(group_code.to_string());
                for member_id in &group.members {
                    if let Some(addr) = self.sessions.get(member_id) {
                        addr.do_send(ServerMessage::GroupMembersUpdate(group.usernames.clone()));
                        if member_id != socket_id {
                            addr.do_send(ServerMessage::UserJoinedGroup(user.username.clone()));
                        }
                    }
                }
                if let Some(addr) = self.sessions.get(socket_id) {
                    addr.do_send(ServerMessage::ChatStarted { group_code: Some(group_code.to_string()) });
                }
            }
        } else {
            if let Some(addr) = self.sessions.get(socket_id) {
                addr.do_send(ServerMessage::GroupNotFound);
            }
        }
    }

    fn join_random_group(&mut self, socket_id: &str) {
        let group_code_option = {
            let available_groups: Vec<&Group> = self.groups.values().filter(|g| !g.members.is_empty()).collect();
            if available_groups.is_empty() {
                None
            } else {
                let random_index = rand::random::<usize>() % available_groups.len();
                Some(available_groups[random_index].code.clone())
            }
        };
        
        match group_code_option {
            Some(code) => self.join_group_by_code(socket_id, &code),
            None => self.create_new_group(socket_id),
        }
    }
}

impl Handler<RegisterUser> for ChatServer {
    type Result = ();
    fn handle(&mut self, msg: RegisterUser, _: &mut Self::Context) {
        self.sessions.insert(msg.socket_id, msg.addr);
    }
}

impl Handler<UnregisterUser> for ChatServer {
    type Result = ();
    fn handle(&mut self, msg: UnregisterUser, _: &mut Self::Context) {
        self.sessions.remove(&msg.socket_id);
        self.handle_disconnect(&msg.socket_id);
    }
}

impl Handler<JoinChat> for ChatServer {
    type Result = ();
    fn handle(&mut self, msg: JoinChat, _: &mut Self::Context) {
        let socket_id = msg.socket_id;
        let profile = msg.profile;
        let user = User {
            id: socket_id.clone(),
            user_id: profile.user_id.clone(),
            username: if profile.username.is_empty() { format!("User-{}", profile.user_id[..5].to_string()) } else { profile.username.clone() },
            gender: profile.gender.clone(),
            preference: profile.preference.clone(),
            room_type: profile.room_type.clone(),
            partner_id: None,
            group_id: None,
        };
        self.users.insert(socket_id.clone(), user);
        if profile.room_type == "group" {
            let join_method = profile.group_join_method.unwrap_or("random".to_string());
            if join_method == "create" {
                self.create_new_group(&socket_id);
            } else if join_method == "join" && profile.group_code.is_some() {
                self.join_group_by_code(&socket_id, &profile.group_code.unwrap());
            } else {
                self.join_random_group(&socket_id);
            }
        } else {
            self.find_match(&socket_id);
        }
    }
}

impl Handler<SendChatMessage> for ChatServer {
    type Result = ();
    fn handle(&mut self, msg: SendChatMessage, _: &mut Self::Context) {
        let socket_id = &msg.socket_id;
        let message = msg.message;
        let is_group_chat = msg.is_group_chat;
        let group_code = msg.group_code;
        if let Some(user) = self.users.get(socket_id) {
            if is_group_chat {
                let group_id = group_code.or(user.group_id.clone());
                if let Some(group_id) = group_id {
                    if let Some(group) = self.groups.get(&group_id) {
                        for member_id in &group.members {
                            if member_id != socket_id {
                                if let Some(addr) = self.sessions.get(member_id) {
                                    addr.do_send(ServerMessage::ReceiveMessage {
                                        message: message.clone(),
                                        sender: user.username.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
            } else {
                if let Some(partner_id) = &user.partner_id {
                    if let Some(addr) = self.sessions.get(partner_id) {
                        addr.do_send(ServerMessage::ReceiveMessage {
                            message: message.clone(),
                            sender: user.username.clone(),
                        });
                    }
                }
            }
        }
    }
}

impl Handler<TypingStart> for ChatServer {
    type Result = ();
    fn handle(&mut self, msg: TypingStart, _: &mut Self::Context) {
        let socket_id = &msg.socket_id;
        let is_group_chat = msg.is_group_chat;
        let group_code = msg.group_code;
        if let Some(user) = self.users.get(socket_id) {
            if is_group_chat {
                let group_id = group_code.or(user.group_id.clone());
                if let Some(group_id) = group_id {
                    if let Some(group) = self.groups.get(&group_id) {
                        for member_id in &group.members {
                            if member_id != socket_id {
                                if let Some(addr) = self.sessions.get(member_id) {
                                    addr.do_send(ServerMessage::TypingStarted { username: Some(user.username.clone()) });
                                }
                            }
                        }
                    }
                }
            } else {
                if let Some(partner_id) = &user.partner_id {
                    if let Some(addr) = self.sessions.get(partner_id) {
                        addr.do_send(ServerMessage::TypingStarted { username: None });
                    }
                }
            }
        }
    }
}

impl Handler<TypingStop> for ChatServer {
    type Result = ();
    fn handle(&mut self, msg: TypingStop, _: &mut Self::Context) {
        let socket_id = &msg.socket_id;
        let is_group_chat = msg.is_group_chat;
        let group_code = msg.group_code;
        if let Some(user) = self.users.get(socket_id) {
            if is_group_chat {
                let group_id = group_code.or(user.group_id.clone());
                if let Some(group_id) = group_id {
                    if let Some(group) = self.groups.get(&group_id) {
                        for member_id in &group.members {
                            if member_id != socket_id {
                                if let Some(addr) = self.sessions.get(member_id) {
                                    addr.do_send(ServerMessage::TypingStopped { username: Some(user.username.clone()) });
                                }
                            }
                        }
                    }
                }
            } else {
                if let Some(partner_id) = &user.partner_id {
                    if let Some(addr) = self.sessions.get(partner_id) {
                        addr.do_send(ServerMessage::TypingStopped { username: None });
                    }
                }
            }
        }
    }
}

impl Handler<DisconnectChat> for ChatServer {
    type Result = ();
    fn handle(&mut self, msg: DisconnectChat, _: &mut Self::Context) {
        self.handle_disconnect(&msg.socket_id);
    }
}

// ### Server Messages

#[derive(Message)]
#[rtype(result = "()")]
enum ServerMessage {
    ChatStarted { group_code: Option<String> },
    ReceiveMessage { message: EncryptedMessage, sender: String },
    TypingStarted { username: Option<String> },
    TypingStopped { username: Option<String> },
    PartnerDisconnected,
    GroupMembersUpdate(Vec<String>),
    UserJoinedGroup(String),
    UserLeftGroup(String),
    GroupNotFound,
    WaitingForMatch,
}

// ### WebSocket Actor

struct WebSocketActor {
    chat_server: Addr<ChatServer>,
    socket_id: String,
}

impl Actor for WebSocketActor {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        self.chat_server.do_send(RegisterUser {
            socket_id: self.socket_id.clone(),
            addr: ctx.address(),
        });
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        self.chat_server.do_send(UnregisterUser {
            socket_id: self.socket_id.clone(),
        });
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WebSocketActor {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match msg {
            Ok(ws::Message::Text(text)) => {
                if let Ok(client_event) = serde_json::from_str::<ClientEvent>(&text) {
                    match client_event.event.as_str() {
                        "join_chat" => {
                            if let Ok(profile) = serde_json::from_value::<UserProfile>(client_event.data) {
                                self.chat_server.do_send(JoinChat {
                                    socket_id: self.socket_id.clone(),
                                    profile,
                                });
                            }
                        }
                        "send_message" => {
                            if let Ok(data) = serde_json::from_value::<SendMessageData>(client_event.data) {
                                self.chat_server.do_send(SendChatMessage {
                                    socket_id: self.socket_id.clone(),
                                    message: data.message,
                                    is_group_chat: data.is_group_chat,
                                    group_code: data.group_code,
                                });
                            }
                        }
                        "typing_start" => {
                            if let Ok(data) = serde_json::from_value::<TypingData>(client_event.data) {
                                self.chat_server.do_send(TypingStart {
                                    socket_id: self.socket_id.clone(),
                                    is_group_chat: data.is_group_chat,
                                    group_code: data.group_code,
                                });
                            }
                        }
                        "typing_stop" => {
                            if let Ok(data) = serde_json::from_value::<TypingData>(client_event.data) {
                                self.chat_server.do_send(TypingStop {
                                    socket_id: self.socket_id.clone(),
                                    is_group_chat: data.is_group_chat,
                                    group_code: data.group_code,
                                });
                            }
                        }
                        "disconnect_chat" => {
                            self.chat_server.do_send(DisconnectChat {
                                socket_id: self.socket_id.clone(),
                            });
                        }
                        _ => {}
                    }
                }
            }
            Ok(ws::Message::Ping(msg)) => ctx.pong(&msg),
            Ok(ws::Message::Close(reason)) => {
                ctx.close(reason);
                ctx.stop();
            }
            _ => {}
        }
    }
}

impl Handler<ServerMessage> for WebSocketActor {
    type Result = ();
    fn handle(&mut self, msg: ServerMessage, ctx: &mut Self::Context) {
        let (event, data) = match msg {
            ServerMessage::ChatStarted { group_code } => {
                if let Some(group_code) = group_code {
                    ("chat_started", serde_json::json!({ "groupCode": group_code }))
                } else {
                    ("chat_started", serde_json::json!({}))
                }
            }
            ServerMessage::ReceiveMessage { message, sender } => {
                ("receive_message", serde_json::json!({ "message": message, "sender": sender }))
            }
            ServerMessage::TypingStarted { username } => {
                if let Some(username) = username {
                    ("typing_started", serde_json::json!({ "username": username }))
                } else {
                    ("typing_started", serde_json::json!({}))
                }
            }
            ServerMessage::TypingStopped { username } => {
                if let Some(username) = username {
                    ("typing_stopped", serde_json::json!({ "username": username }))
                } else {
                    ("typing_stopped", serde_json::json!({}))
                }
            }
            ServerMessage::PartnerDisconnected => ("partner_disconnected", serde_json::json!({})),
            ServerMessage::GroupMembersUpdate(members) => ("group_members_update", serde_json::json!(members)),
            ServerMessage::UserJoinedGroup(username) => ("user_joined_group", serde_json::json!(username)),
            ServerMessage::UserLeftGroup(username) => ("user_left_group", serde_json::json!(username)),
            ServerMessage::GroupNotFound => ("group_not_found", serde_json::json!({})),
            ServerMessage::WaitingForMatch => ("waiting_for_match", serde_json::json!({})),
        };
        let server_event = ServerEvent { event: event.to_string(), data };
        let json = serde_json::to_string(&server_event).unwrap();
        ctx.text(json);
    }
}

// ### Server Setup

async fn index() -> &'static str {
    "Socket.io server for Random Tune Harmony chat is running"
}

async fn ws_index(req: HttpRequest, stream: web::Payload, srv: web::Data<Addr<ChatServer>>) -> Result<HttpResponse, actix_web::Error> {
    let socket_id = Uuid::new_v4().to_string();
    let ws = WebSocketActor {
        chat_server: srv.get_ref().clone(),
        socket_id,
    };
    ws::start(ws, &req, stream)
}

#[shuttle_runtime::main]
async fn main() -> ShuttleActixWeb<impl FnOnce(&mut web::ServiceConfig) + Send + Clone + 'static> {
    // Create a chat server actor
    let chat_server = ChatServer::default().start();
    
    // Define the config function to set up routes
    let config = move |cfg: &mut web::ServiceConfig| {
        cfg.app_data(web::Data::new(chat_server.clone()));
        cfg.route("/", web::get().to(index));
        cfg.route("/ws/", web::get().to(ws_index));
    };
    
    Ok(config.into())
}