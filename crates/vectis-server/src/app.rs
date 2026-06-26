use std::net::SocketAddr;

use axum::Router;
use axum::extract::{Query, State, ws::WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;

use crate::connection::handle_socket;
use crate::state::ServerState;

#[derive(serde::Deserialize)]
struct WsParams {
    room: Option<String>,
}

pub async fn run() {
    let state = ServerState::new();

    let app = Router::new()
        .route("/", get(index))
        .route("/ws", get(ws_handler))
        .with_state(state);

    let addr: SocketAddr = std::env::var("VECTIS_ADDR")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 3000)));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind vectis server");
    println!("vectis-server listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server failed");
}

async fn index() -> &'static str {
    "vectis-server: connect WebSocket clients at /ws"
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<ServerState>,
    Query(params): Query<WsParams>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state, params.room))
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
