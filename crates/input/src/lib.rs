#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub enum InputEvent {
    MouseMove { x: f32, y: f32 },
    MouseButton { button: u8, pressed: bool },
    MouseScroll { dx: f32, dy: f32 },
    KeyEvent { keycode: u32, pressed: bool },
}
