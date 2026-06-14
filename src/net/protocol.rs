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

/// Bump on any wire-incompatible change to ClientMsg/ServerMsg/Snapshot.
/// A mismatch is reported clearly via the frozen handshake below.
pub const PROTOCOL_VERSION: u16 = 1;

/// FROZEN wire format — never change these bytes. The client opens with a
/// 5-byte preamble (magic + version); the server replies with one verdict byte
/// (1 = ok, 0 = rejected followed by a u16-LE length + UTF-8 reason). This is
/// deliberately independent of postcard so version drift in the message schema
/// can't stop us from telling an out-of-date client to update.
const HANDSHAKE_MAGIC: [u8; 3] = *b"ARv";

/// Client side: announce our version, then read the server's verdict.
pub async fn client_handshake(send: &mut SendStream, recv: &mut RecvStream) -> Result<()> {
    let mut hello = [0u8; 5];
    hello[..3].copy_from_slice(&HANDSHAKE_MAGIC);
    hello[3..].copy_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    send.write_all(&hello).await?;
    let mut verdict = [0u8; 1];
    recv.read_exact(&mut verdict).await?;
    if verdict[0] == 1 {
        return Ok(());
    }
    let mut len = [0u8; 2];
    recv.read_exact(&mut len).await?;
    let n = (u16::from_le_bytes(len) as usize).min(1024);
    let mut reason = vec![0u8; n];
    recv.read_exact(&mut reason).await?;
    bail!("{}", String::from_utf8_lossy(&reason));
}

/// Server side: read the client's preamble and accept or reject by version.
pub async fn server_handshake(send: &mut SendStream, recv: &mut RecvStream) -> Result<()> {
    let mut hello = [0u8; 5];
    recv.read_exact(&mut hello).await?;
    if hello[..3] != HANDSHAKE_MAGIC {
        bail!("not an ascii-royale client");
    }
    let v = u16::from_le_bytes([hello[3], hello[4]]);
    if v == PROTOCOL_VERSION {
        send.write_all(&[1u8]).await?;
        Ok(())
    } else {
        let reason = format!(
            "your ascii-royale is out of date (client v{v}, server v{PROTOCOL_VERSION}). \
             update: cargo install --git https://github.com/chad/ascii-royale"
        );
        let mut out = vec![0u8];
        out.extend_from_slice(&(reason.len() as u16).to_le_bytes());
        out.extend_from_slice(reason.as_bytes());
        send.write_all(&out).await?;
        bail!("rejected out-of-date client v{v}");
    }
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
