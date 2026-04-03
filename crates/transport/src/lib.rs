use serde::{Deserialize, Serialize};

// Stream 0: video (host -> viewer)
// Stream 1: input events (viewer -> host)
// Stream 2: controle / keepalive (bidirecional)

#[derive(Serialize, Deserialize)]
pub struct VideoPacket {
    pub seq: u64,
    pub timestamp: u64,
    pub keyframe: bool,
    pub payload: Vec<u8>,
}
