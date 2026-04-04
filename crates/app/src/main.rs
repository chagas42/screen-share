// Fase 2 — transporte QUIC entre host e viewer
// Uso:
//   cargo run -p app -- --host      (captura e transmite)
//   cargo run -p app -- --viewer    (recebe e exibe)

use std::net::SocketAddr;
use codec::Codec;
use codec::{Decoder, Encoder};
use renderer::Renderer;
use transport::{Receiver, Sender};

const ADDR: &str = "127.0.0.1:5000";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok(); // ignora se já foi instalado

    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        Some("--host")   => host().await,
        Some("--viewer") => viewer().await,
        _ => {
            eprintln!("uso: screenshare --host | --viewer");
            Ok(())
        }
    }
}

// ─── Host ────────────────────────────────────────────────────────────────────

async fn host() -> anyhow::Result<()> {
    let addr: SocketAddr = ADDR.parse()?;
    let mut sender = Sender::bind_and_accept(addr).await?;

    let first = capture::capture()?;
    let mut encoder = Encoder::new(first.width, first.height)?;

    loop {
        let frame = capture::capture()?;

        if let Some(nal) = encoder.encode(&frame)? {
            let keyframe = false; // TODO: expor do encoder
            sender.send(nal, keyframe).await?;
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(16)).await;
    }
}

// ─── Viewer ──────────────────────────────────────────────────────────────────

async fn viewer() -> anyhow::Result<()> {
    let addr: SocketAddr = ADDR.parse()?;
    let mut receiver = Receiver::connect(addr).await?;
    let mut decoder = Decoder::new()?;

    let (tx, rx) = std::sync::mpsc::channel::<capture::Frame>();

    // Renderer precisa da thread principal — spawna em background e bloqueia aqui
    std::thread::spawn(move || {
        Renderer::new(rx).run().ok();
    });

    loop {
        match receiver.recv().await? {
            Some(pkt) => {
                if let Some(frame) = decoder.decode(&pkt.payload)? {
                    if tx.send(frame).is_err() {
                        break; // renderer fechou
                    }
                }
            }
            None => break, // host desconectou
        }
    }

    Ok(())
}
