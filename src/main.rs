mod server;
mod handler;

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use actix_cors::Cors;
use server::ChatServer;
use shuttle_actix_web::ShuttleActixWeb;
use std::env;

// ### Server Setup

async fn index() -> impl Responder {
    "Socket.io server for Random Tune Harmony chat is running"
}

async fn ws_route(
    req: HttpRequest,
    body: web::Payload,
    srv: web::Data<server::ChatServerHandle>,
) -> Result<HttpResponse, actix_web::Error> {
    // Upgrade the HTTP connection to a WebSocket connection
    let (response, session, stream) = actix_ws::handle(&req, body)?;
    
    // Spawn a task to handle the WebSocket connection
    let chat_server = srv.get_ref().clone();
    actix_web::rt::spawn(handler::chat_ws(chat_server, session, stream));
    
    Ok(response)
}

#[shuttle_runtime::main]
async fn main() -> ShuttleActixWeb<impl FnOnce(&mut web::ServiceConfig) + Send + Clone + 'static> {
    // Create a chat server
    let chat_server = ChatServer::start();
    
    // Define the config function to set up routes
    let config = move |cfg: &mut web::ServiceConfig| {
        // Get allowed origin from environment variable or use default
        let allowed_origin = env::var("ALLOWED_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".to_string());
        
        log::info!("Configuring CORS with allowed origin: {}", allowed_origin);
        
        // Configure CORS
        let cors = Cors::default()
            .allowed_origin(&allowed_origin)
            .allowed_methods(vec!["GET", "POST"])
            .allowed_headers(vec![
                actix_web::http::header::AUTHORIZATION,
                actix_web::http::header::ACCEPT,
                actix_web::http::header::CONTENT_TYPE,
            ])
            .supports_credentials()
            .max_age(3600);
        
        // With Shuttle, we need to use a different approach for middleware
        cfg.service(
            web::scope("")
                .wrap(cors)
                .app_data(web::Data::new(chat_server.clone()))
                .route("/", web::get().to(index))
                .route("/ws/", web::get().to(ws_route))
        );
    };
    
    Ok(config.into())
}