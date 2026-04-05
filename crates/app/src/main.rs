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
use transport::{Receiver, Sender};

// URL do signaling server — lida de variavel de ambiente com fallback local
fn signaling_url() -> String {
    std::env::var("SIGNALING_URL")
        .unwrap_or_else(|_| "ws://localhost:3000".to_string())
}

// ─── Mensagens WebSocket (espelho do signaling-server) ────────────────────────

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Msg {
    Create { host_addr: String },
    Code { code: String },
    ViewerReady { viewer_addr: String },
    Join { code: String, viewer_addr: String },
    HostReady { host_addr: String },
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
    // Descobre o IP publico da maquina para passar ao signaling server
    let local_ip = local_ip()?;
    let quic_addr = format!("{local_ip}:5000");

    println!("[host] meu endereco QUIC: {quic_addr}");

    // Conecta ao signaling server e registra a sessao
    let url = format!("{}/session/create", signaling_url());
    let (mut ws, _) = connect_async(&url).await.context("conectar ao signaling")?;

    let msg = serde_json::to_string(&Msg::Create { host_addr: quic_addr.clone() })?;
    ws.send(Message::Text(msg.into())).await?;

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

    // Abre o servidor QUIC em paralelo enquanto espera o viewer
    let quic_addr_parsed: SocketAddr = quic_addr.parse()?;
    let sender_task = tokio::spawn(async move {
        Sender::bind_and_accept(quic_addr_parsed).await
    });

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

    let mut sender = sender_task.await??;

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
    // Pede o codigo ao usuario
    println!("Digite o codigo de sessao:");
    let mut code = String::new();
    std::io::stdin().read_line(&mut code)?;
    let code = code.trim().to_uppercase();

    // Descobre o proprio IP para passar ao signaling
    let local_ip = local_ip()?;
    let quic_addr = format!("{local_ip}:5001");

    // Conecta ao signaling e entra na sessao
    let url: String = format!("{}/session/join/{code}", signaling_url());
    let (mut ws, _) = connect_async(&url).await.context("conectar ao signaling")?;

    let msg = serde_json::to_string(&Msg::Join {
        code: code.clone(),
        viewer_addr: quic_addr,
    })?;
    ws.send(Message::Text(msg.into())).await?;

    // Recebe o endereco QUIC do host
    let host_addr: SocketAddr = match ws.next().await {
        Some(Ok(Message::Text(txt))) => {
            match serde_json::from_str::<Msg>(&txt)? {
                Msg::HostReady { host_addr } => host_addr.parse()?,
                Msg::Error { reason } => anyhow::bail!("signaling erro: {reason}"),
                other => anyhow::bail!("resposta inesperada: {other:?}"),
            }
        }
        _ => anyhow::bail!("conexao com signaling fechou inesperadamente"),
    };

    println!("[viewer] conectando ao host em {host_addr}...");

    let mut receiver = Receiver::connect(host_addr).await?;
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

// Descobre o IP local da maquina na rede
fn local_ip() -> Result<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0")?;
    socket.connect("8.8.8.8:80")?; // nao envia nada — so descobre a interface de saida
    let addr = socket.local_addr()?;
    Ok(addr.ip().to_string())
}
