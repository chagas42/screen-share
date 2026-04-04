// Fase 1 — pipeline local: capture -> encode -> decode -> render
// Sem rede. Valida que cada componente funciona corretamente.
use std::sync::mpsc::channel;
use codec::Decoder;
use codec::Encoder;
use std::time::Duration;
use std::thread::sleep;
use renderer::Renderer;
use codec::Codec;

fn main() -> anyhow::Result<()> {
    let (tx, rx) = channel::<capture::Frame>();

    // Thread do pipeline: capture -> encode -> decode -> tx.send()
    std::thread::spawn(move || -> anyhow::Result<()> {
        // Captura o primeiro frame para descobrir as dimensoes reais da tela
        let first = capture::capture()?;
        let width = first.width;
        let height = first.height;

        let mut encoder = Encoder::new(width, height)?;
        let mut decoder = Decoder::new()?;

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

            // ~60fps
            sleep(Duration::from_millis(16));
        }

        Ok(())
    });

    // Renderer na thread principal (requisito do macOS / winit)
    Renderer::new(rx).run()
}
