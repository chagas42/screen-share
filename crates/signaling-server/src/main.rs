// Servidor de sinalizacao via axum + WebSocket
//
// Endpoints:
//   WS /session/create          -- host abre sessao, recebe codigo
//   WS /session/join/:codigo    -- viewer entra na sessao
//   WS /session/:id/signal      -- troca de ICE candidates

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    Ok(())
}
