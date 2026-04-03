# screenshare

Software de compartilhamento de tela P2P escrito em Rust, com suporte a macOS (ARM) e Windows.

A conexão entre host e viewer é direta — após o handshake inicial via servidor de sinalização, o tráfego de vídeo não passa por nenhum servidor intermediário.

## Como funciona

O host captura a tela, comprime os frames em H.264 e os transmite via QUIC. O viewer recebe, decodifica e exibe em uma janela. Eventos de mouse e teclado do viewer são enviados de volta ao host e injetados via API do sistema operacional.

```
host:    captura → encode → QUIC ──────────────────────────▶ decode → render
viewer:  ◀─────────────────────── QUIC ← input (mouse/teclado)
```

A conexão P2P é estabelecida com troca de ICE candidates via servidor de sinalização leve. Após o handshake, o servidor sai da equação.

## Stack

- **Captura:** `CGDisplayStream` no macOS, `DXGI Desktop Duplication` no Windows
- **Codec:** H.264 via `ffmpeg-next` — `preset=ultrafast`, `tune=zerolatency`, sem B-frames
- **Transporte:** QUIC via `quinn`
- **Renderer:** `wgpu` (abstrai Metal no macOS e DirectX/Vulkan no Windows)
- **Sinalização:** `axum` + WebSocket
- **Autenticação:** código de sessão de 6 dígitos com HMAC-SHA256

## Estrutura

```
crates/
  capture/          # captura de tela (platform-specific)
  codec/            # encoder e decoder H.264
  transport/        # QUIC + framing de pacotes
  renderer/         # exibição via wgpu
  session/          # geração e validação de código de sessão
  signaling-server/ # servidor de sinalização (binário separado)
  input/            # captura e injeção de eventos de input
  app/              # binário principal
```

## Status

| Fase | Descrição | Status |
|---|---|---|
| 1 | Pipeline local: captura → encode → decode → render | Completo |
| 2 | Transporte via QUIC entre dois processos | Pendente |
| 3 | Sessão, sinalização e NAT traversal | Pendente |
| 4 | Input relay, UI e build cross-platform | Pendente |

## Rodando

### macOS

Dependências: Rust e FFmpeg.

```bash
brew install ffmpeg
cargo run -p app
```

Na primeira execução o macOS vai solicitar permissão de Screen Recording em System Settings → Privacy & Security → Screen Recording.

### Windows

1. Instale o [Rust](https://rustup.rs)
2. Instale o [LLVM](https://github.com/llvm/llvm-project/releases) e adicione ao PATH
3. Baixe as libs do FFmpeg em [ffmpeg-builds](https://github.com/BtbN/FFmpeg-Builds/releases) — escolha a versão `ffmpeg-master-latest-win64-gpl-shared`
4. Extraia e configure as variáveis de ambiente apontando para a pasta extraída:
   ```
   FFMPEG_DIR=C:\ffmpeg
   PATH=%PATH%;C:\ffmpeg\bin
   ```
5. Rode:
   ```bash
   cargo run -p app
   ```
