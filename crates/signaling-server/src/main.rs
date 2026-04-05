// Servidor de sinalizacao via axum + WebSocket
//
// Endpoints:
//   WS /session/create          -- host abre sessao, recebe codigo
//   WS /session/join/:codigo    -- viewer entra na sessao

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, State};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

// ─── Estado compartilhado ─────────────────────────────────────────────────────

#[derive(Debug)]
struct Session {
    host_addr: Option<String>,
    cert_fingerprint: Option<String>,
    viewer_tx: Option<tokio::sync::oneshot::Sender<String>>,
    created_at: Instant,
}

#[derive(Debug)]
struct RateEntry {
    attempts: u32,
    first_attempt: Instant,
    blocked_until: Option<Instant>,
}

#[derive(Clone)]
struct AppState {
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    rate_limit: Arc<Mutex<HashMap<IpAddr, RateEntry>>>,
    auth_key: String,
}

// ─── Mensagens WebSocket ──────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Msg {
    Auth { key: String },
    Create { host_addr: String, cert_fingerprint: String },
    Code { code: String },
    ViewerReady { viewer_addr: String },
    Join { code: String, viewer_addr: String },
    HostReady { host_addr: String, cert_fingerprint: String },
    Error { reason: String },
}

// ─── Gerador de codigo ────────────────────────────────────────────────────────

fn gen_code() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..6)
        .map(|_| {
            let idx = rng.gen_range(0..36u8);
            if idx < 10 { (b'0' + idx) as char } else { (b'A' + idx - 10) as char }
        })
        .collect()
}

// ─── Rate limiting ────────────────────────────────────────────────────────────

const MAX_ATTEMPTS: u32 = 10;
const WINDOW_SECS: u64 = 60;
const BLOCK_SECS: u64 = 300;

async fn check_rate_limit(rate_limit: &Mutex<HashMap<IpAddr, RateEntry>>, ip: IpAddr) -> bool {
    let mut map = rate_limit.lock().await;
    let now = Instant::now();
    let entry = map.entry(ip).or_insert(RateEntry {
        attempts: 0,
        first_attempt: now,
        blocked_until: None,
    });

    // verifica se ainda está bloqueado
    if let Some(until) = entry.blocked_until {
        if now < until {
            return false;
        }
        // bloqueio expirou — reseta
        entry.attempts = 0;
        entry.first_attempt = now;
        entry.blocked_until = None;
    }

    // reseta janela se expirou
    if entry.first_attempt.elapsed().as_secs() > WINDOW_SECS {
        entry.attempts = 0;
        entry.first_attempt = now;
    }

    entry.attempts += 1;

    if entry.attempts > MAX_ATTEMPTS {
        entry.blocked_until = Some(now + Duration::from_secs(BLOCK_SECS));
        return false;
    }

    true
}

// ─── Limpeza de sessoes expiradas ─────────────────────────────────────────────

async fn cleanup_task(state: AppState) {
    loop {
        tokio::time::sleep(Duration::from_secs(30)).await;
        state.sessions.lock().await
            .retain(|_, s| s.created_at.elapsed() < Duration::from_secs(300));
        state.rate_limit.lock().await
            .retain(|_, e| e.first_attempt.elapsed().as_secs() < BLOCK_SECS * 2);
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

async fn send(ws: &mut WebSocket, msg: Msg) {
    if let Ok(txt) = serde_json::to_string(&msg) {
        let _ = ws.send(Message::Text(txt.into())).await;
    }
}

async fn validate_auth(ws: &mut WebSocket, auth_key: &str) -> bool {
    match ws.recv().await {
        Some(Ok(Message::Text(txt))) => {
            match serde_json::from_str::<Msg>(&txt) {
                Ok(Msg::Auth { key }) if key == auth_key => true,
                _ => {
                    send(ws, Msg::Error { reason: "autenticacao invalida".into() }).await;
                    false
                }
            }
        }
        _ => false,
    }
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn handle_create(
    ws: WebSocketUpgrade,
    ConnectInfo(_addr): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| create_session(socket, state))
}

async fn create_session(mut ws: WebSocket, state: AppState) {
    if !validate_auth(&mut ws, &state.auth_key).await {
        return;
    }

    let (host_addr, cert_fingerprint) = match ws.recv().await {
        Some(Ok(Message::Text(txt))) => {
            match serde_json::from_str::<Msg>(&txt) {
                Ok(Msg::Create { host_addr, cert_fingerprint }) => (host_addr, cert_fingerprint),
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
        let map = state.sessions.lock().await;
        if !map.contains_key(&c) { break c; }
    };

    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    {
        let mut map = state.sessions.lock().await;
        map.insert(code.clone(), Session {
            host_addr: Some(host_addr),
            cert_fingerprint: Some(cert_fingerprint),
            viewer_tx: Some(tx),
            created_at: Instant::now(),
        });
    }

    send(&mut ws, Msg::Code { code: code.clone() }).await;
    println!("[signaling] sessao criada: {code}");

    match tokio::time::timeout(Duration::from_secs(300), rx).await {
        Ok(Ok(viewer_addr)) => {
            send(&mut ws, Msg::ViewerReady { viewer_addr }).await;
            println!("[signaling] sessao {code} estabelecida");
        }
        _ => println!("[signaling] sessao {code} expirou"),
    }

    state.sessions.lock().await.remove(&code);
}

async fn handle_join(
    Path(code): Path<String>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| join_session(socket, state, code, addr.ip()))
}

async fn join_session(mut ws: WebSocket, state: AppState, code: String, ip: IpAddr) {
    if !validate_auth(&mut ws, &state.auth_key).await {
        return;
    }

    // rate limiting por IP
    if !check_rate_limit(&state.rate_limit, ip).await {
        send(&mut ws, Msg::Error { reason: "muitas tentativas, tente novamente em 5 minutos".into() }).await;
        return;
    }

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

    let (host_addr, cert_fingerprint) = {
        let mut map = state.sessions.lock().await;
        match map.get_mut(&code) {
            Some(session) => {
                let host_addr = session.host_addr.clone().unwrap_or_default();
                let fingerprint = session.cert_fingerprint.clone().unwrap_or_default();
                if let Some(tx) = session.viewer_tx.take() {
                    let _ = tx.send(viewer_addr);
                }
                (host_addr, fingerprint)
            }
            None => {
                send(&mut ws, Msg::Error { reason: format!("sessao {code} nao encontrada") }).await;
                return;
            }
        }
    };

    send(&mut ws, Msg::HostReady { host_addr, cert_fingerprint }).await;
}

// ─── Main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let auth_key = std::env::var("AUTH_KEY")
        .expect("AUTH_KEY nao definida — defina a variavel de ambiente antes de iniciar");

    let state = AppState {
        sessions: Arc::new(Mutex::new(HashMap::new())),
        rate_limit: Arc::new(Mutex::new(HashMap::new())),
        auth_key,
    };

    tokio::spawn(cleanup_task(state.clone()));

    let app = Router::new()
        .route("/session/create", get(handle_create))
        .route("/session/join/:code", get(handle_join))
        .with_state(state)
        .into_make_service_with_connect_info::<SocketAddr>();

    let addr = "0.0.0.0:3000";
    println!("[signaling] escutando em {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
