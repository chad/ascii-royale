use anyhow::{Context, Result};
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointId};
use tokio::sync::mpsc;

use super::protocol::{recv_frame, send_frame, ClientMsg, ServerHandle, ServerMsg, ALPN};

/// Dial a host by ticket (its iroh endpoint id), introduce ourselves, and
/// return a handle the UI can treat exactly like a local server.
pub async fn connect(ticket: &str, name: &str) -> Result<ServerHandle> {
    let host_id: EndpointId = ticket
        .trim()
        .parse()
        .context("that doesn't look like an ascii-royale ticket")?;

    let endpoint = Endpoint::builder(presets::N0)
        .bind()
        .await
        .context("binding iroh endpoint")?;
    let conn = endpoint
        .connect(host_id, ALPN)
        .await
        .context("connecting to host (are they still in the lobby?)")?;
    let (mut send, mut recv) = conn.open_bi().await?;

    send_frame(&mut send, &ClientMsg::Hello { name: name.to_string() }).await?;

    let (srv_tx, srv_rx) = mpsc::channel::<ServerMsg>(64);
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<ClientMsg>(64);

    // Reader: host -> UI. Dropping srv_tx tells the UI the link died.
    tokio::spawn(async move {
        while let Ok(msg) = recv_frame::<ServerMsg>(&mut recv).await {
            if srv_tx.send(msg).await.is_err() {
                break;
            }
        }
        // keep endpoint alive for the lifetime of the connection
        drop(endpoint);
    });

    // Writer: UI -> host.
    tokio::spawn(async move {
        while let Some(msg) = cmd_rx.recv().await {
            if send_frame(&mut send, &msg).await.is_err() {
                break;
            }
        }
    });

    Ok(ServerHandle { rx: srv_rx, tx: cmd_tx })
}
