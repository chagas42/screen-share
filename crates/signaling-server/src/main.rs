// Servidor de sinalizacao via axum + WebSocket
//
// Endpoints:
//   WS /session/create          -- host abre sessao, recebe codigo
//   WS /session/join/:codigo    -- viewer entra na sessao

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

// ─── Estado compartilhado ─────────────────────────────────────────────────────

#[derive(Debug)]
struct Session {
    host_addr: Option<String>,   // IP:porta QUIC do host
    viewer_tx: Option<tokio::sync::oneshot::Sender<String>>, // canal para notificar o host
    created_at: Instant,
}

type Sessions = Arc<Mutex<HashMap<String, Session>>>;

// ─── Mensagens WebSocket ──────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Msg {
    // host → server: "quero criar sessao, meu endereco QUIC é esse"
    Create { host_addr: String },
    // server → host: "seu codigo é X7K2QP"
    Code { code: String },
    // server → host: "viewer conectou, o IP dele é esse"
    ViewerReady { viewer_addr: String },

    // viewer → server: "quero entrar na sessao X7K2QP, meu endereco é esse"
    Join { code: String, viewer_addr: String },
    // server → viewer: "host encontrado, IP dele é esse"
    HostReady { host_addr: String },

    Error { reason: String },
}

// ─── Gerador de codigo ────────────────────────────────────────────────────────

fn gen_code() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..6)
        .map(|_| {
            let idx = rng.gen_range(0..36);
            if idx < 10 {
                (b'0' + idx) as char
            } else {
                (b'A' + idx - 10) as char
            }
        })
        .collect()
}

// ─── Limpeza de sessoes expiradas ─────────────────────────────────────────────

async fn cleanup_task(sessions: Sessions) {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        let mut map = sessions.lock().await;
        map.retain(|_, s| s.created_at.elapsed() < Duration::from_secs(300));
    }
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn handle_create(
    ws: WebSocketUpgrade,
    State(sessions): State<Sessions>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| create_session(socket, sessions))
}

async fn create_session(mut ws: WebSocket, sessions: Sessions) {
    // Espera a mensagem Create do host
    let host_addr = match ws.recv().await {
        Some(Ok(Message::Text(txt))) => {
            match serde_json::from_str::<Msg>(&txt) {
                Ok(Msg::Create { host_addr }) => host_addr,
                _ => {
                    send(&mut ws, Msg::Error { reason: "esperava {type:create}".into() }).await;
                    return;
                }
            }
        }
        _ => return,
    };

    let code = loop {
        let c = gen_code();
        let map = sessions.lock().await;
        if !map.contains_key(&c) {
            break c;
        }
    };

    let (tx, rx) = tokio::sync::oneshot::channel::<String>();

    {
        let mut map = sessions.lock().await;
        map.insert(code.clone(), Session {
            host_addr: Some(host_addr),
            viewer_tx: Some(tx),
            created_at: Instant::now(),
        });
    }

    // Envia o codigo para o host
    send(&mut ws, Msg::Code { code: code.clone() }).await;
    println!("[signaling] sessao criada: {code}");

    // Espera o viewer entrar (timeout 5 min)
    match tokio::time::timeout(Duration::from_secs(300), rx).await {
        Ok(Ok(viewer_addr)) => {
            send(&mut ws, Msg::ViewerReady { viewer_addr }).await;
            println!("[signaling] sessao {code} estabelecida");
        }
        _ => {
            println!("[signaling] sessao {code} expirou");
        }
    }

    // Remove sessao
    sessions.lock().await.remove(&code);
}

async fn handle_join(
    Path(code): Path<String>,
    ws: WebSocketUpgrade,
    State(sessions): State<Sessions>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| join_session(socket, sessions, code))
}

async fn join_session(mut ws: WebSocket, sessions: Sessions, code: String) {
    // Espera a mensagem Join do viewer
    let viewer_addr = match ws.recv().await {
        Some(Ok(Message::Text(txt))) => {
            match serde_json::from_str::<Msg>(&txt) {
                Ok(Msg::Join { viewer_addr, .. }) => viewer_addr,
                _ => {
                    send(&mut ws, Msg::Error { reason: "esperava {type:join}".into() }).await;
                    return;
                }
            }
        }
        _ => return,
    };

    // Busca a sessao e notifica o host
    let host_addr = {
        let mut map = sessions.lock().await;
        match map.get_mut(&code) {
            Some(session) => {
                let host_addr = session.host_addr.clone().unwrap_or_default();
                if let Some(tx) = session.viewer_tx.take() {
                    let _ = tx.send(viewer_addr);
                }
                host_addr
            }
            None => {
                send(&mut ws, Msg::Error { reason: format!("sessao {code} nao encontrada") }).await;
                return;
            }
        }
    };

    // Envia o endereco do host para o viewer
    send(&mut ws, Msg::HostReady { host_addr }).await;
}

// ─── Helper ───────────────────────────────────────────────────────────────────

async fn send(ws: &mut WebSocket, msg: Msg) {
    if let Ok(txt) = serde_json::to_string(&msg) {
        let _ = ws.send(Message::Text(txt.into())).await;
    }
}

// ─── Main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let sessions: Sessions = Arc::new(Mutex::new(HashMap::new()));

    tokio::spawn(cleanup_task(sessions.clone()));

    let app = Router::new()
        .route("/session/create", get(handle_create))
        .route("/session/join/:code", get(handle_join))
        .with_state(sessions);

    let addr = "0.0.0.0:3000";
    println!("[signaling] escutando em {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
