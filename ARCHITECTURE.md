# Arquitetura — Software de Compartilhamento de Tela
## Stack: Rust | Plataformas: macOS (ARM) + Windows | Modelo: P2P + Signaling

---

## Visao geral

Dois processos identicos (mesmo binario): um atua como **host** (captura e transmite),
o outro como **viewer** (recebe e exibe). A conexao P2P e estabelecida via um
servidor de sinalizacao leve. Apos o handshake, o trafego de video passa
diretamente entre as maquinas sem tocar o servidor.

---

## Componentes

### 1. Screen Capture

Responsavel por capturar frames da tela do host em alta frequencia.

**macOS (ARM):**
- API: `CGDisplayStream` via bindings Rust (`core-graphics` crate)
- Alternativa mais moderna: `ScreenCaptureKit` (macOS 12.3+) — preferir esta se
  o target minimo permitir, pois tem menor latencia e suporte a captura por janela

**Windows:**
- API: `DXGI Desktop Duplication` — acesso direto ao framebuffer da GPU,
  latencia minima
- Crate: `windows-rs` (bindings oficiais da Microsoft para Rust)

**Output:** frames brutos em formato `BGRA` ou `NV12` (YUV), a depender do que
o encoder aceitar diretamente para evitar conversao desnecessaria

---

### 2. Encoder

Comprime os frames antes de transmitir. Foco em baixa latencia, nao em
eficiencia maxima de compressao.

**Codec recomendado:** H.264 com perfil `baseline` ou `main`
- Latencia menor que H.265
- Suporte universal em decoders de hardware

**Crates:**
- `ffmpeg-next` — bindings para libavcodec/libavformat
- Alternativa sem FFmpeg: `openh264` crate (encoder puro em Rust)

**Parametros importantes para baixa latencia:**
```
tune=zerolatency
preset=ultrafast
keyint_max=60       # keyframe a cada 2s em 30fps
b_frames=0          # desabilita B-frames para reduzir latencia
```

**Aceleracao por hardware (opcional, fase 2):**
- Windows: NVENC (NVIDIA) ou AMF (AMD) via `ffmpeg-next`
- macOS: VideoToolbox (`videotoolbox` crate)

---

### 3. Transporte (Packet Sender / Receiver)

**Protocolo:** QUIC sobre UDP
- Multiplexacao de streams sem head-of-line blocking
- Controle de congestionamento nativo
- Retransmissao seletiva (diferente de UDP puro)

**Crate:** `quinn` — implementacao QUIC em Rust puro

**Estrutura do pacote:**
```rust
struct VideoPacket {
    seq: u64,           // numero de sequencia
    timestamp: u64,     // pts do frame (microsegundos)
    keyframe: bool,     // indica se e keyframe
    payload: Vec<u8>,   // NAL units H.264
}
```

**Streams QUIC separadas:**
- Stream 0: video (unidirecional, host -> viewer)
- Stream 1: input events (unidirecional, viewer -> host)
- Stream 2: controle / keepalive (bidirecional)

---

### 4. Session Manager

Gerencia autenticacao via codigo de sessao simples.

**Fluxo:**
1. Host gera codigo de 6 digitos alfanumerico (ex: `X7K2QP`)
2. Codigo e exibido na UI do host
3. Viewer digita o codigo
4. Signaling server valida e troca os ICE candidates entre as partes
5. Apos handshake P2P estabelecido, signaling server nao participa mais

**Seguranca minima:**
- Codigo expira apos 5 minutos ou apos primeira conexao
- HMAC-SHA256 para verificar integridade das mensagens de handshake
- TLS obrigatorio entre clients e signaling server

---

### 5. Signaling Server

Servidor leve que faz apenas a troca de metadados de conexao.
**Nao trafega video.** Pode ser hosteado em qualquer VPS pequeno.

**Stack sugerida:** Rust + `axum` + WebSocket

**Responsabilidades:**
- Armazenar sessoes ativas (em memoria, sem banco de dados)
- Receber e repassar SDP offers/answers
- Repassar ICE candidates entre host e viewer
- Remover sessao apos conexao P2P estabelecida ou timeout

**Endpoints:**
```
WS /session/create         -- host abre sessao, recebe codigo
WS /session/join/:codigo   -- viewer entra na sessao
WS /session/:id/signal     -- troca de ICE candidates
```

---

### 6. NAT Traversal (STUN)

Para que dois computadores atras de NAT consigam se conectar diretamente.

**Fluxo ICE simplificado:**
1. Cada cliente consulta servidor STUN para descobrir seu IP publico
2. Troca os candidates via signaling server
3. Testa conectividade direta (ICE connectivity checks)
4. Estabelece conexao QUIC diretamente entre as maquinas

**STUN publico:** `stun.l.google.com:19302` (suficiente para MVP)

**Crate:** `webrtc-ice` ou implementar ICE basico manualmente sobre `quinn`

**Fallback (fase 2):** se P2P falhar (NAT simetrico), implementar TURN relay

---

### 7. Decoder + Renderer (Viewer)

**Decoder:**
- `ffmpeg-next` com aceleracao de hardware quando disponivel
- Saida: frames `BGRA` ou `NV12`

**Renderer:**
- `wgpu` — abstrai Metal (macOS) e DirectX/Vulkan (Windows)
- Upload do frame como textura, fullscreen quad, apresentacao via swapchain
- Permite scaling e filtros futuros sem mudar a arquitetura

**Latencia alvo:** < 100ms ponta a ponta em rede local

---

### 8. Input Relay

Viewer captura eventos de mouse e teclado e envia para o host.

```rust
enum InputEvent {
    MouseMove { x: f32, y: f32 },         // coordenadas normalizadas 0..1
    MouseButton { button: u8, pressed: bool },
    MouseScroll { dx: f32, dy: f32 },
    KeyEvent { keycode: u32, pressed: bool },
}
```

**No host:** injeta os eventos via API do sistema operacional
- Windows: `SendInput` via `windows-rs`
- macOS: `CGEventPost` via `core-graphics`

---

## Estrutura de crates (Cargo workspace)

```
screenshare/
  Cargo.toml                  # workspace root

  crates/
    capture/                  # captura de tela (platform-specific)
      src/
        lib.rs
        macos.rs
        windows.rs

    codec/                    # encoder + decoder
      src/lib.rs

    transport/                # QUIC + packet framing
      src/lib.rs

    signaling-server/         # servidor de sinalizacao (binario separado)
      src/main.rs

    session/                  # logica de sessao e autenticacao
      src/lib.rs

    input/                    # captura e injecao de eventos
      src/
        lib.rs
        macos.rs
        windows.rs

    renderer/                 # wgpu renderer
      src/lib.rs

    app/                      # binario principal (host + viewer modes)
      src/main.rs
```

---

## Dependencias principais

```toml
[dependencies]
# Transport
quinn = "0.11"
rustls = "0.23"

# Codec
ffmpeg-next = "7"

# Renderer
wgpu = "22"
winit = "0.30"          # janela cross-platform

# Platform (condicional)
[target.'cfg(target_os = "windows")'.dependencies]
windows = { version = "0.58", features = ["Win32_Graphics_Dxgi", "Win32_UI_Input_KeyboardAndMouse"] }

[target.'cfg(target_os = "macos")'.dependencies]
core-graphics = "0.23"
core-video = "0.1"

# Signaling server
axum = "0.7"
tokio = { version = "1", features = ["full"] }
tokio-tungstenite = "0.23"

# Util
serde = { version = "1", features = ["derive"] }
bincode = "2"           # serializacao binaria dos pacotes
hmac = "0.12"
sha2 = "0.10"
```

---

## Ordem de implementacao sugerida

### Fase 1 — Pipeline local (sem rede)
1. `capture` crate: captura um frame e salva como PNG (valida a API)
2. `codec` crate: encoda e decoda o frame (valida a qualidade)
3. `renderer` crate: exibe o frame decodado em uma janela (valida o wgpu)
4. Conectar os tres em um loop local: captura -> encode -> decode -> render

### Fase 2 — Transporte local (mesma maquina)
5. `transport` crate: envia pacotes via QUIC entre dois processos locais
6. Integrar com o pipeline: captura -> encode -> QUIC -> decode -> render
7. Medir latencia e ajustar parametros do encoder

### Fase 3 — Sessao e sinalizacao
8. `signaling-server`: WebSocket server basico
9. `session` crate: geracao e validacao de codigo
10. NAT traversal com STUN
11. Teste entre duas maquinas na mesma rede local

### Fase 4 — Input relay e polish
12. `input` crate: captura e injecao de eventos
13. UI minima (janela com campo de codigo)
14. Teste atraves da internet com NAT real
15. Build cross-platform e empacotamento

---

## Decisoes tecnicas a revisar no futuro

- **AV1 em vez de H.264:** melhor qualidade por bitrate, mas encoder mais lento
  e suporte de hardware ainda limitado. Vale avaliar na fase 2.
- **TURN server proprio:** necessario para usuarios atras de NAT simetrico
  (corporativo). Crate `turn-rs` ou usar Coturn.
- **Criptografia do stream de video:** atualmente apenas o handshake e protegido.
  Considerar DTLS sobre o canal QUIC para o video tambem.
- **Multi-monitor:** `CGDisplayStream` e `DXGI` suportam captura por monitor.
  Adicionar selecao de monitor na UI.
