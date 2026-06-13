use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_rusqlite::Connection;
use tokio_tungstenite::accept_async;

use crate::config::Config;
use crate::control::handler::{self, SharedState};
use crate::db::{schema, queries};
use crate::files::handler::FileHandler;
use crate::user::manager::UserManager;
use crate::room::manager::RoomManager;
use crate::audio::udp_server::UdpAudioServer;
use crate::tls;

pub async fn run(config: Config, create_admin: bool) -> anyhow::Result<()> {
    // Ensure data directories exist
    if let Some(parent) = std::path::Path::new(&config.storage.database_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir_all(&config.storage.upload_dir)?;

    // Initialize database
    let db = Arc::new(Connection::open(&config.storage.database_path).await?);
    schema::initialize(&db).await?;
    tracing::info!("Database initialized: {}", config.storage.database_path);

    // Create admin user if requested
    if create_admin {
        match queries::create_user(&db, "admin".to_string(), "admin".to_string(), "admin".to_string()).await {
            Ok(id) => tracing::info!("Admin user created (id: {}, username: admin, password: admin)", id),
            Err(e) => tracing::warn!("Could not create admin user (may already exist): {}", e),
        }
    }

    // Create admin user from environment (for Docker: TC_ADMIN_USERNAME / TC_ADMIN_PASSWORD)
    if let (Ok(username), Ok(password)) = (
        std::env::var("TC_ADMIN_USERNAME"),
        std::env::var("TC_ADMIN_PASSWORD"),
    ) {
        if username.is_empty() || password.is_empty() {
            tracing::warn!("TC_ADMIN_USERNAME/TC_ADMIN_PASSWORD set but empty, skipping admin creation");
        } else {
            match queries::create_user(&db, username.clone(), password, "admin".to_string()).await {
                Ok(id) => tracing::info!("Admin user created from environment (id: {}, username: {})", id, username),
                Err(e) => tracing::info!("Admin user '{}' not created (may already exist): {}", username, e),
            }
        }
    }

    // Initialize managers
    let users = UserManager::new();
    let rooms = RoomManager::new(db.clone(), users.clone());
    let files = FileHandler::new(db.clone(), &config);

    // Start UDP audio server
    let udp_server = UdpAudioServer::start(&config, users.clone()).await?;

    // Setup TLS if enabled
    let tls_acceptor = if config.tls.enabled {
        match tls::setup_tls(&config.tls) {
            Ok(acceptor) => {
                tracing::info!("TLS enabled");
                Some(acceptor)
            }
            Err(e) => {
                tracing::warn!("TLS setup failed ({}), running without TLS", e);
                None
            }
        }
    } else {
        None
    };

    let state = Arc::new(SharedState {
        config: config.clone(),
        db,
        users,
        rooms,
        files,
        udp_server: Some(udp_server),
    });

    // Start WebSocket server
    let addr = format!("{}:{}", config.network.control_host, config.network.control_port);
    let listener = TcpListener::bind(&addr).await?;
    tracing::info!("Control server listening on {} (TLS: {})", addr, tls_acceptor.is_some());
    tracing::info!("Server '{}' is ready!", config.server.name);

    loop {
        let (stream, addr) = listener.accept().await?;
        let peer_addr = addr.to_string();
        let state = state.clone();
        let tls_acceptor = tls_acceptor.clone();

        tokio::spawn(async move {
            if let Some(acceptor) = tls_acceptor {
                match acceptor.accept(stream).await {
                    Ok(tls_stream) => {
                        match accept_async(tls_stream).await {
                            Ok(ws) => {
                                handler::handle_connection(ws, peer_addr, state).await;
                            }
                            Err(e) => {
                                tracing::error!("WebSocket handshake error (TLS) from {}: {}", peer_addr, e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("TLS handshake error from {}: {}", peer_addr, e);
                    }
                }
            } else {
                match accept_async(stream).await {
                    Ok(ws) => {
                        handler::handle_connection(ws, peer_addr, state).await;
                    }
                    Err(e) => {
                        tracing::error!("WebSocket handshake error from {}: {}", peer_addr, e);
                    }
                }
            }
        });
    }
}
