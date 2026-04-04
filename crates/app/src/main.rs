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

fn main() -> anyhow::Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        Some("--host") => {
            tokio::runtime::Runtime::new()?.block_on(host())
        }
        Some("--viewer") => {
            // viewer precisa do renderer na thread principal — tokio vai para background
            viewer_main()
        }
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

fn viewer_main() -> anyhow::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<capture::Frame>();

    // Tokio + QUIC rodam em background
    std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .expect("tokio runtime")
            .block_on(async move {
                if let Err(e) = viewer_recv(tx).await {
                    eprintln!("[viewer] erro: {e}");
                }
            });
    });

    // Renderer na thread principal — requisito do macOS
    Renderer::new(rx).run()
}

async fn viewer_recv(tx: std::sync::mpsc::Sender<capture::Frame>) -> anyhow::Result<()> {
    let addr: SocketAddr = ADDR.parse()?;
    let mut receiver = Receiver::connect(addr).await?;
    let mut decoder = Decoder::new()?;

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
