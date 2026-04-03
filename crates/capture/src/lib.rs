#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

pub struct Frame {
    pub data: Vec<u8>, // BGRA, sem stride padding
    pub width: u32,
    pub height: u32,
}

pub fn capture() -> anyhow::Result<Frame> {
    #[cfg(target_os = "macos")]
    return macos::capture_frame();

    #[cfg(target_os = "windows")]
    return windows::capture_frame();

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    anyhow::bail!("plataforma nao suportada")
}
