// Fase 1 — pipeline local: capture -> encode -> decode -> render
// Sem rede. Valida que cada componente funciona corretamente.

fn main() -> anyhow::Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<capture::Frame>();

    // Thread do pipeline: capture -> encode -> decode -> tx.send()
    std::thread::spawn(move || -> anyhow::Result<()> {
        // Captura o primeiro frame para descobrir as dimensoes reais da tela
        let first = capture::capture()?;
        let width = first.width;
        let height = first.height;

        let mut encoder = codec::Encoder::new(width, height)?;
        let mut decoder = codec::Decoder::new()?;

        // Envia o primeiro frame diretamente (antes do encode/decode)
        // para que o renderer inicialize o GpuState com as dimensoes corretas
        tx.send(first).ok();

        loop {
            let frame = capture::capture()?;

            if let Some(nal) = encoder.encode(&frame)? {
                if let Some(decoded) = decoder.decode(&nal)? {
                    if tx.send(decoded).is_err() {
                        break; // renderer foi fechado
                    }
                }
            }

            // ~30fps
            std::thread::sleep(std::time::Duration::from_millis(33));
        }

        Ok(())
    });

    // Renderer na thread principal (requisito do macOS / winit)
    renderer::Renderer::new(rx).run()
}
