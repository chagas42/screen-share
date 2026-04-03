use anyhow::{Context, Result};
use core_graphics::display::CGDisplay;

pub fn capture_frame() -> Result<crate::Frame> {
    let display = CGDisplay::main();

    let image = display
        .image()
        .context("CGDisplay::image() retornou None — permissao de Screen Recording concedida?")?;

    let width = image.width() as u32;
    let height = image.height() as u32;
    let bytes_per_row = image.bytes_per_row(); // stride — pode ser > width * 4

    // CGImage::data() chama CGDataProviderCopyData internamente — retorna CFData
    let cf_data = image.data();
    let raw: &[u8] = cf_data.bytes();

    // Remove padding de stride: copia linha a linha
    let row_bytes = (width * 4) as usize;
    let mut data = Vec::with_capacity(row_bytes * height as usize);
    for row in 0..height as usize {
        let start = row * bytes_per_row;
        data.extend_from_slice(&raw[start..start + row_bytes]);
    }

    // CGDisplayCreateImage retorna BGRA nativo no macOS
    Ok(crate::Frame { data, width, height })
}
