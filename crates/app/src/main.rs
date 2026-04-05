// Fase 3 — signaling server para troca de enderecos
// Uso:
//   cargo run -p app -- --host
//   cargo run -p app -- --viewer

use std::net::SocketAddr;

use anyhow::{Context, Result};
use codec::{Codec, Decoder, Encoder};
use futures_util::{SinkExt, StreamExt};
use renderer::Renderer;
use serde::{Deserialize, Serialize};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use transport::{Receiver, SenderEndpoint};

fn signaling_url() -> String {
    std::env::var("SIGNALING_URL")
        .unwrap_or_else(|_| "ws://localhost:3000".to_string())
}

fn auth_key() -> Result<String> {
    std::env::var("AUTH_KEY").context("AUTH_KEY nao definida — defina a variavel de ambiente")
}

// ─── Mensagens WebSocket (espelho do signaling-server) ────────────────────────

#[derive(Serialize, Deserialize, Debug)]
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

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        Some("--host")   => tokio::runtime::Runtime::new()?.block_on(host()),
        Some("--viewer") => viewer_main(),
        _ => {
            eprintln!("uso: screenshare --host | --viewer");
            Ok(())
        }
    }
}

// ─── Host ────────────────────────────────────────────────────────────────────

async fn host() -> Result<()> {
    let key = auth_key()?;
    let local_ip = local_ip()?;
    let quic_addr = format!("{local_ip}:5000");

    println!("[host] meu endereco QUIC: {quic_addr}");

    // Bind do QUIC — não bloqueia, só abre a porta e gera o certificado
    let quic_addr_parsed: SocketAddr = quic_addr.parse()?;
    let endpoint = SenderEndpoint::bind(quic_addr_parsed)?;
    let fingerprint = endpoint.fingerprint.clone();

    // Conecta ao signaling com o fingerprint já disponível
    let url = format!("{}/session/create", signaling_url());
    let (mut ws, _) = connect_async(&url).await.context("conectar ao signaling")?;

    // Autenticação
    let msg = serde_json::to_string(&Msg::Auth { key })?;
    ws.send(Message::Text(msg.into())).await?;

    // Anuncia sessão com endereço e fingerprint
    let create_msg = serde_json::to_string(&Msg::Create {
        host_addr: quic_addr,
        cert_fingerprint: fingerprint,
    })?;
    ws.send(Message::Text(create_msg.into())).await?;

    // Recebe o codigo de sessao
    let code = match ws.next().await {
        Some(Ok(Message::Text(txt))) => {
            match serde_json::from_str::<Msg>(&txt)? {
                Msg::Code { code } => code,
                Msg::Error { reason } => anyhow::bail!("signaling erro: {reason}"),
                other => anyhow::bail!("resposta inesperada: {other:?}"),
            }
        }
        _ => anyhow::bail!("conexao com signaling fechou inesperadamente"),
    };

    println!("[host] codigo de sessao: {code}");
    println!("[host] aguardando viewer...");

    // Aceita a conexao QUIC em paralelo enquanto espera o viewer no signaling
    let accept_task = tokio::spawn(async move { endpoint.accept().await });

    // Espera o viewer entrar na sessao
    let _viewer_addr = match ws.next().await {
        Some(Ok(Message::Text(txt))) => {
            match serde_json::from_str::<Msg>(&txt)? {
                Msg::ViewerReady { viewer_addr } => viewer_addr,
                Msg::Error { reason } => anyhow::bail!("signaling erro: {reason}"),
                other => anyhow::bail!("resposta inesperada: {other:?}"),
            }
        }
        _ => anyhow::bail!("conexao com signaling fechou inesperadamente"),
    };

    println!("[host] viewer conectou, iniciando stream...");

    let mut sender = accept_task.await??;

    let first = capture::capture()?;
    let mut encoder = Encoder::new(first.width, first.height)?;

    loop {
        let frame = capture::capture()?;

        if let Some(nal) = encoder.encode(&frame)? {
            sender.send(nal, false).await?;
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(16)).await;
    }
}

// ─── Viewer ───────────────────────────────────────────────────────────────────

fn viewer_main() -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<capture::Frame>();

    std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .expect("tokio runtime")
            .block_on(async move {
                if let Err(e) = viewer_recv(tx).await {
                    eprintln!("[viewer] erro: {e}");
                }
            });
    });

    Renderer::new(rx).run()
}

async fn viewer_recv(tx: std::sync::mpsc::Sender<capture::Frame>) -> Result<()> {
    let key = auth_key()?;

    println!("Digite o codigo de sessao:");
    let mut code = String::new();
    std::io::stdin().read_line(&mut code)?;
    let code = code.trim().to_uppercase();

    let local_ip = local_ip()?;
    let quic_addr = format!("{local_ip}:5001");

    let url = format!("{}/session/join/{code}", signaling_url());
    let (mut ws, _) = connect_async(&url).await.context("conectar ao signaling")?;

    // Autenticação
    let msg = serde_json::to_string(&Msg::Auth { key })?;
    ws.send(Message::Text(msg.into())).await?;

    // Entra na sessao
    let join_msg = serde_json::to_string(&Msg::Join {
        code: code.clone(),
        viewer_addr: quic_addr,
    })?;
    ws.send(Message::Text(join_msg.into())).await?;

    // Recebe endereco e fingerprint do host
    let (host_addr, cert_fingerprint) = match ws.next().await {
        Some(Ok(Message::Text(txt))) => {
            match serde_json::from_str::<Msg>(&txt)? {
                Msg::HostReady { host_addr, cert_fingerprint } => (host_addr, cert_fingerprint),
                Msg::Error { reason } => anyhow::bail!("signaling erro: {reason}"),
                other => anyhow::bail!("resposta inesperada: {other:?}"),
            }
        }
        _ => anyhow::bail!("conexao com signaling fechou inesperadamente"),
    };

    let host_addr: SocketAddr = host_addr.parse()?;
    println!("[viewer] conectando ao host em {host_addr}...");

    // Conecta verificando o fingerprint — rejeita se não conferir
    let mut receiver = Receiver::connect(host_addr, cert_fingerprint).await?;
    let mut decoder = Decoder::new()?;

    loop {
        match receiver.recv().await? {
            Some(pkt) => {
                if let Some(frame) = decoder.decode(&pkt.payload)? {
                    if tx.send(frame).is_err() {
                        break;
                    }
                }
            }
            None => break,
        }
    }

    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn local_ip() -> Result<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0")?;
    socket.connect("8.8.8.8:80")?;
    let addr = socket.local_addr()?;
    Ok(addr.ip().to_string())
}
