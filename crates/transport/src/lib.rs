use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use quinn::{ClientConfig, Endpoint, ServerConfig};
use rcgen::generate_simple_self_signed;
use rustls_pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};

// ─── Packet ──────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct VideoPacket {
    pub seq: u64,
    pub timestamp: u64,
    pub keyframe: bool,
    pub payload: Vec<u8>,
}

// ─── TLS helpers ─────────────────────────────────────────────────────────────

// Gera certificado self-signed em memoria — suficiente para uso local
fn self_signed_cert() -> Result<(Vec<CertificateDer<'static>>, PrivatePkcs8KeyDer<'static>)> {
    let cert = generate_simple_self_signed(vec!["localhost".into()])
        .context("gerar certificado self-signed")?;

    let cert_der = CertificateDer::from(cert.cert);
    let key_der = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());

    Ok((vec![cert_der], key_der))
}

fn server_config() -> Result<ServerConfig> {
    let (certs, key) = self_signed_cert()?;

    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key.into())
        .context("ServerConfig::with_single_cert")?;

    tls.alpn_protocols = vec![b"screenshare".to_vec()];

    Ok(ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(tls)
            .context("QuicServerConfig")?,
    )))
}

fn client_config() -> Result<ClientConfig> {
    // Aceita qualquer certificado — conexao local, sem necessidade de CA real
    let mut tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipVerify))
        .with_no_client_auth();

    tls.alpn_protocols = vec![b"screenshare".to_vec()];

    Ok(ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls)
            .context("QuicClientConfig")?,
    )))
}

// Verifier que aceita qualquer certificado (so para uso local)
#[derive(Debug)]
struct SkipVerify;

impl rustls::client::danger::ServerCertVerifier for SkipVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer,
        _intermediates: &[CertificateDer],
        _server_name: &rustls_pki_types::ServerName,
        _ocsp: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// ─── Sender (host) ───────────────────────────────────────────────────────────

pub struct Sender {
    send_stream: quinn::SendStream,
    seq: u64,
}

impl Sender {
    /// Abre servidor QUIC e espera o viewer conectar.
    pub async fn bind_and_accept(addr: SocketAddr) -> Result<Self> {
        let endpoint = Endpoint::server(server_config()?, addr)
            .context("Endpoint::server")?;

        println!("[host] aguardando viewer em {addr}...");

        let conn = endpoint
            .accept()
            .await
            .context("nenhuma conexao recebida")?
            .await
            .context("handshake QUIC")?;

        println!("[host] viewer conectado: {}", conn.remote_address());

        let send_stream = conn
            .open_uni()
            .await
            .context("open_uni stream de video")?;

        Ok(Self { send_stream, seq: 0 })
    }

    /// Envia NAL units como VideoPacket serializado.
    pub async fn send(&mut self, payload: Vec<u8>, keyframe: bool) -> Result<()> {
        let pkt = VideoPacket {
            seq: self.seq,
            timestamp: self.seq * 16, // ~60fps em ms
            keyframe,
            payload,
        };
        self.seq += 1;

        let bytes = bincode::serde::encode_to_vec(&pkt, bincode::config::standard())
            .context("bincode::encode")?;

        // Prefixo de 4 bytes com o tamanho do pacote — framing simples
        let len = (bytes.len() as u32).to_le_bytes();
        self.send_stream.write_all(&len).await.context("write len")?;
        self.send_stream.write_all(&bytes).await.context("write pkt")?;

        Ok(())
    }
}

// ─── Receiver (viewer) ───────────────────────────────────────────────────────

pub struct Receiver {
    recv_stream: quinn::RecvStream,
}

impl Receiver {
    /// Conecta ao host e abre o stream de video.
    pub async fn connect(addr: SocketAddr) -> Result<Self> {
        let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)
            .context("Endpoint::client")?;

        endpoint.set_default_client_config(client_config()?);

        println!("[viewer] conectando em {addr}...");

        let conn = endpoint
            .connect(addr, "localhost")
            .context("connect")?
            .await
            .context("handshake QUIC")?;

        println!("[viewer] conectado ao host");

        let recv_stream = conn
            .accept_uni()
            .await
            .context("accept_uni stream de video")?;

        Ok(Self { recv_stream })
    }

    /// Recebe o proximo VideoPacket. Retorna None se a conexao encerrou.
    pub async fn recv(&mut self) -> Result<Option<VideoPacket>> {
        // Le os 4 bytes do tamanho
        let mut len_buf = [0u8; 4];
        match self.recv_stream.read_exact(&mut len_buf).await {
            Ok(()) => {}
            Err(quinn::ReadExactError::FinishedEarly(_)) => return Ok(None),
            Err(e) => return Err(e).context("read len"),
        }

        let len = u32::from_le_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];

        self.recv_stream
            .read_exact(&mut buf)
            .await
            .context("read pkt")?;

        let (pkt, _) = bincode::serde::decode_from_slice(&buf, bincode::config::standard())
            .context("bincode::decode")?;

        Ok(Some(pkt))
    }
}
