use anyhow::{bail, Result};
use iroh::endpoint::{RecvStream, SendStream};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::game::map::Map;
use crate::game::state::{InputCmd, Snapshot};
use crate::game::GameConfig;

pub const ALPN: &[u8] = b"ascii-royale/0";

/// Frames bigger than this are a protocol violation, not a big map.
const MAX_FRAME: u32 = 1 << 20;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMsg {
    /// First message on the stream. `color` is a 0xRRGGBB skin color.
    Hello { name: String, color: u32 },
    Input(InputCmd),
    /// Begin the match — only honored from the host's own local client.
    Start,
    /// Toggle/set "ready to drop" in the dropship lobby.
    Ready(bool),
    /// Update name + skin color in the lobby.
    SetProfile { name: String, color: u32 },
}

/// Parse a hex skin color like "ff8800" / "#ff8800" into 0xRRGGBB.
pub fn parse_hex_color(s: &str) -> Option<u32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}

/// A pleasant default skin for a player id, so unset players still look distinct.
pub fn default_skin(id: u8) -> u32 {
    const PALETTE: [u32; 8] = [
        0xffd75f, // amber
        0x7dcfff, // cyan
        0xbb9af7, // violet
        0x9ece6a, // green
        0xff899d, // pink
        0xff9e64, // orange
        0x73daca, // teal
        0xe0e0e0, // white
    ];
    PALETTE[(id as usize) % PALETTE.len()]
}

/// One combatant shown in the dropship lobby roster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Aboard {
    pub name: String,
    pub ready: bool,
    pub is_you: bool,
    pub color: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Standing {
    pub name: String,
    pub placement: Option<u8>,
    pub kills: u8,
    pub is_you: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMsg {
    Welcome { id: u8, map: Map, config: GameConfig },
    /// The dropship lobby. `starting_in` is Some(secs) when a countdown is
    /// running (arena) or None when waiting on a boss to start (host mode).
    /// `seats` is the island capacity; empty seats fill with combatants at drop.
    Roster { aboard: Vec<Aboard>, seats: u8, starting_in: Option<u32> },
    Snapshot(Box<Snapshot>),
    End { standings: Vec<Standing> },
    /// You arrived mid-match: hang tight, the lobby reopens after this one.
    Waiting { alive: u8 },
    Rejected { reason: String },
}

/// What the UI holds, regardless of whether the server is across the
/// network or a task in this same process.
pub struct ServerHandle {
    pub rx: mpsc::Receiver<ServerMsg>,
    pub tx: mpsc::Sender<ClientMsg>,
}

pub async fn send_frame<T: Serialize>(stream: &mut SendStream, msg: &T) -> Result<()> {
    let bytes = postcard::to_stdvec(msg)?;
    stream.write_all(&(bytes.len() as u32).to_le_bytes()).await?;
    stream.write_all(&bytes).await?;
    Ok(())
}

pub async fn recv_frame<T: DeserializeOwned>(stream: &mut RecvStream) -> Result<T> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).await?;
    let len = u32::from_le_bytes(len);
    if len > MAX_FRAME {
        bail!("oversized frame: {len} bytes");
    }
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf).await?;
    Ok(postcard::from_bytes(&buf)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_roundtrip_through_postcard() {
        let msg = ClientMsg::Input(InputCmd::Move(crate::game::Dir::East));
        let bytes = postcard::to_stdvec(&msg).unwrap();
        let back: ClientMsg = postcard::from_bytes(&bytes).unwrap();
        assert!(matches!(back, ClientMsg::Input(InputCmd::Move(crate::game::Dir::East))));

        let map = crate::game::map::generate(5);
        let welcome = ServerMsg::Welcome { id: 3, map, config: GameConfig::default() };
        let bytes = postcard::to_stdvec(&welcome).unwrap();
        let back: ServerMsg = postcard::from_bytes(&bytes).unwrap();
        match back {
            ServerMsg::Welcome { id, map, .. } => {
                assert_eq!(id, 3);
                assert_eq!(map.w, crate::game::map::MAP_W);
            }
            _ => panic!("wrong variant"),
        }
    }
}
