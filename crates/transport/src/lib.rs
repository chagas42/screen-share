use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use quinn::{ClientConfig, Endpoint, ServerConfig};
use rcgen::generate_simple_self_signed;
use rustls_pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ─── Packet ──────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct VideoPacket {
    pub seq: u64,
    pub timestamp: u64,
    pub keyframe: bool,
    pub payload: Vec<u8>,
}

// ─── TLS helpers ─────────────────────────────────────────────────────────────

fn self_signed_cert() -> Result<(Vec<CertificateDer<'static>>, PrivatePkcs8KeyDer<'static>)> {
    let cert = generate_simple_self_signed(vec!["localhost".into()])
        .context("gerar certificado self-signed")?;

    let cert_der = CertificateDer::from(cert.cert);
    let key_der = PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());

    Ok((vec![cert_der], key_der))
}

/// Calcula o fingerprint SHA-256 do certificado — usado pelo viewer para verificar o host
pub fn cert_fingerprint(cert: &CertificateDer) -> String {
    let hash = Sha256::digest(cert.as_ref());
    hex::encode(hash)
}

fn server_config() -> Result<(ServerConfig, String)> {
    let (certs, key) = self_signed_cert()?;
    let fingerprint = cert_fingerprint(&certs[0]);

    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key.into())
        .context("ServerConfig::with_single_cert")?;

    tls.alpn_protocols = vec![b"screenshare".to_vec()];

    let config = ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(tls)
            .context("QuicServerConfig")?,
    ));

    Ok((config, fingerprint))
}

fn client_config(expected_fingerprint: String) -> Result<ClientConfig> {
    let mut tls = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(FingerprintVerifier { expected_fingerprint }))
        .with_no_client_auth();

    tls.alpn_protocols = vec![b"screenshare".to_vec()];

    Ok(ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls)
            .context("QuicClientConfig")?,
    )))
}

// Verifier que valida o certificado pelo fingerprint recebido via signaling
#[derive(Debug)]
struct FingerprintVerifier {
    expected_fingerprint: String,
}

impl rustls::client::danger::ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer,
        _intermediates: &[CertificateDer],
        _server_name: &rustls_pki_types::ServerName,
        _ocsp: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let actual = cert_fingerprint(end_entity);
        if actual == self.expected_fingerprint {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "fingerprint do certificado nao confere: esperado {}, recebido {}",
                self.expected_fingerprint, actual
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// ─── Sender (host) ───────────────────────────────────────────────────────────

pub struct SenderEndpoint {
    endpoint: Endpoint,
    pub fingerprint: String,
}

impl SenderEndpoint {
    /// Abre o servidor QUIC e expõe o fingerprint — sem bloquear ainda.
    pub fn bind(addr: SocketAddr) -> Result<Self> {
        let (server_cfg, fingerprint) = server_config()?;
        let endpoint = Endpoint::server(server_cfg, addr)
            .context("Endpoint::server")?;
        Ok(Self { endpoint, fingerprint })
    }

    /// Espera o viewer conectar e retorna o Sender pronto para enviar.
    pub async fn accept(self) -> Result<Sender> {
        println!("[host] aguardando viewer QUIC...");

        let conn = self.endpoint
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

        Ok(Sender { send_stream, seq: 0 })
    }
}

pub struct Sender {
    send_stream: quinn::SendStream,
    seq: u64,
}

impl Sender {
    pub async fn send(&mut self, payload: Vec<u8>, keyframe: bool) -> Result<()> {
        let pkt = VideoPacket {
            seq: self.seq,
            timestamp: self.seq * 16,
            keyframe,
            payload,
        };
        self.seq += 1;

        let bytes = bincode::serde::encode_to_vec(&pkt, bincode::config::standard())
            .context("bincode::encode")?;

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
    /// Conecta ao host verificando o fingerprint do certificado.
    pub async fn connect(addr: SocketAddr, cert_fingerprint: String) -> Result<Self> {
        let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)
            .context("Endpoint::client")?;

        endpoint.set_default_client_config(client_config(cert_fingerprint)?);

        println!("[viewer] conectando em {addr}...");

        let conn = endpoint
            .connect(addr, "localhost")
            .context("connect")?
            .await
            .context("handshake QUIC — certificado invalido ou host errado")?;

        println!("[viewer] conectado ao host");

        let recv_stream = conn
            .accept_uni()
            .await
            .context("accept_uni stream de video")?;

        Ok(Self { recv_stream })
    }

    pub async fn recv(&mut self) -> Result<Option<VideoPacket>> {
        let mut len_buf = [0u8; 4];
        match self.recv_stream.read_exact(&mut len_buf).await {
            Ok(()) => {}
            Err(quinn::ReadExactError::FinishedEarly(_)) => return Ok(None),
            Err(e) => return Err(e).context("read len"),
        }

        const MAX_PACKET_SIZE: usize = 4 * 1024 * 1024; // 4MB

        let len = u32::from_le_bytes(len_buf) as usize;
        if len > MAX_PACKET_SIZE {
            anyhow::bail!("pacote muito grande: {len} bytes (max {MAX_PACKET_SIZE})");
        }
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
