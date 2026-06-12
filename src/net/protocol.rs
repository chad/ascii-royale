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
    /// First message on the stream.
    Hello { name: String },
    Input(InputCmd),
    /// Begin the match — only honored from the host's own local client.
    Start,
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
    Roster { names: Vec<String>, starting_in: Option<u32> },
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
