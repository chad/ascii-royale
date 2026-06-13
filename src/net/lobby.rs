//! Decentralized lobby discovery over iroh-gossip. Hosts broadcast a small
//! beacon on a well-known topic; browsers collect beacons and show a live list
//! of open games — no ticket sharing, no central match registry.
//!
//! Gossip runs on its own iroh endpoint (separate from the game endpoint), so
//! the game's accept loop is untouched. The beacon carries the *game* ticket;
//! this endpoint is just a courier. Bootstrap: gossip needs one initial peer —
//! the arena publishes its gossip endpoint id at `/lobby` and everyone boots
//! off it, after which the mesh is peer-to-peer (same caveat class as iroh's
//! n0 discovery: a hint to find peers, not a server the game runs through).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use bytes::Bytes;
use futures_lite::StreamExt;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointId};
use iroh_gossip::api::Event;
use iroh_gossip::net::{Gossip, GOSSIP_ALPN};
use iroh_gossip::proto::TopicId;
use serde::{Deserialize, Serialize};

/// Well-known lobby topic. Bump the suffix to break compatibility cleanly.
const TOPIC_SEED: &[u8] = b"ascii-royale/lobby/v0";
/// Hosts re-broadcast their beacon this often.
const BEACON_INTERVAL: Duration = Duration::from_secs(2);
/// Browsers forget a host not heard from for this long.
const BEACON_TTL: Duration = Duration::from_secs(8);

fn topic() -> TopicId {
    TopicId::from_bytes(*blake3::hash(TOPIC_SEED).as_bytes())
}

/// What a host advertises. Kept tiny — it rides every gossip round.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Beacon {
    /// The game ticket to join (host's game endpoint id).
    pub ticket: String,
    /// Host/arena display name.
    pub name: String,
    pub aboard: u8,
    pub seats: u8,
    /// "boarding" | "countdown" | "live" | "results"
    pub phase: String,
    pub starting_in: Option<u32>,
}

/// A beacon plus when we last heard it (browser side).
#[derive(Debug, Clone)]
pub struct Listing {
    pub beacon: Beacon,
    pub last_seen: Instant,
}

impl Listing {
    /// Open lobbies you can still join (not mid-match).
    pub fn joinable(&self) -> bool {
        matches!(self.beacon.phase.as_str(), "boarding" | "countdown")
    }
}

async fn bind_gossip() -> Result<(Endpoint, Gossip, Router)> {
    let endpoint = Endpoint::builder(presets::N0)
        .alpns(vec![GOSSIP_ALPN.to_vec()])
        .bind()
        .await
        .context("binding gossip endpoint")?;
    endpoint.online().await;
    let gossip = Gossip::builder().spawn(endpoint.clone());
    let router = Router::builder(endpoint.clone()).accept(GOSSIP_ALPN, gossip.clone()).spawn();
    Ok((endpoint, gossip, router))
}

/// Start announcing on the lobby topic. `source` is polled each round so the
/// beacon reflects live lobby state. Returns this announcer's gossip endpoint
/// id (publish it so browsers/other hosts can bootstrap off it). The arena
/// passes `bootstrap = None` (it's the seed); other hosts pass the arena's id.
pub async fn spawn_announce<F>(bootstrap: Option<EndpointId>, mut source: F) -> Result<String>
where
    F: FnMut() -> Beacon + Send + 'static,
{
    let (endpoint, gossip, router) = bind_gossip().await?;
    let id = endpoint.id().to_string();
    let boot: Vec<EndpointId> = bootstrap.into_iter().collect();
    let topic = gossip.subscribe(topic(), boot).await?;
    let (sender, _recv) = topic.split();
    tokio::spawn(async move {
        let _endpoint = endpoint; // keep alive
        let _router = router;
        let mut ticker = tokio::time::interval(BEACON_INTERVAL);
        loop {
            ticker.tick().await;
            let beacon = source();
            if let Ok(bytes) = postcard::to_stdvec(&beacon) {
                let _ = sender.broadcast(Bytes::from(bytes)).await;
            }
        }
    });
    Ok(id)
}

/// Live, de-duplicated map of hosts heard on the topic (browser side). Shared
/// with the UI; entries expire after [`BEACON_TTL`].
pub type Listings = Arc<Mutex<HashMap<String, Listing>>>;

/// Join the topic and keep `listings` updated. Returns the shared map and keeps
/// the gossip endpoint/router alive via the spawned task.
pub async fn discover(bootstrap: Option<EndpointId>) -> Result<Listings> {
    let (endpoint, gossip, router) = bind_gossip().await?;
    let boot = bootstrap.into_iter().collect::<Vec<_>>();
    let topic = gossip.subscribe(topic(), boot).await?;
    let (_sender, mut receiver) = topic.split();
    let listings: Listings = Arc::new(Mutex::new(HashMap::new()));
    let out = listings.clone();
    tokio::spawn(async move {
        // Keep these alive for the lifetime of discovery.
        let _endpoint = endpoint;
        let _router = router;
        let mut sweep = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                ev = receiver.next() => {
                    match ev {
                        Some(Ok(Event::Received(msg))) => {
                            if let Ok(beacon) = postcard::from_bytes::<Beacon>(&msg.content) {
                                let mut g = listings.lock().unwrap();
                                g.insert(beacon.ticket.clone(), Listing { beacon, last_seen: Instant::now() });
                            }
                        }
                        Some(_) => {}
                        None => break,
                    }
                }
                _ = sweep.tick() => {
                    let mut g = listings.lock().unwrap();
                    g.retain(|_, l| l.last_seen.elapsed() < BEACON_TTL);
                }
            }
        }
    });
    Ok(out)
}

/// A snapshot of current listings, sorted: joinable first, then most aboard,
/// then soonest drop.
pub fn snapshot(listings: &Listings) -> Vec<Listing> {
    let mut v: Vec<Listing> = listings.lock().unwrap().values().cloned().collect();
    v.sort_by(|a, b| {
        b.joinable()
            .cmp(&a.joinable())
            .then(b.beacon.aboard.cmp(&a.beacon.aboard))
            .then(a.beacon.starting_in.unwrap_or(u32::MAX).cmp(&b.beacon.starting_in.unwrap_or(u32::MAX)))
    });
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_is_stable() {
        assert_eq!(topic(), topic());
    }

    #[test]
    fn beacon_roundtrips() {
        let b = Beacon {
            ticket: "abc".into(),
            name: "arena".into(),
            aboard: 3,
            seats: 16,
            phase: "countdown".into(),
            starting_in: Some(12),
        };
        let bytes = postcard::to_stdvec(&b).unwrap();
        let back: Beacon = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back.ticket, "abc");
        assert_eq!(back.starting_in, Some(12));
    }

    #[test]
    fn snapshot_orders_joinable_and_fuller_first() {
        let listings: Listings = Arc::new(Mutex::new(HashMap::new()));
        {
            let mut g = listings.lock().unwrap();
            let mk = |ticket: &str, phase: &str, aboard: u8| Listing {
                beacon: Beacon {
                    ticket: ticket.into(),
                    name: "h".into(),
                    aboard,
                    seats: 16,
                    phase: phase.into(),
                    starting_in: None,
                },
                last_seen: Instant::now(),
            };
            g.insert("live".into(), mk("live", "live", 9));
            g.insert("empty".into(), mk("empty", "boarding", 1));
            g.insert("full".into(), mk("full", "boarding", 5));
        }
        let snap = snapshot(&listings);
        assert_eq!(snap[0].beacon.ticket, "full"); // joinable + most aboard
        assert_eq!(snap[1].beacon.ticket, "empty"); // joinable
        assert_eq!(snap[2].beacon.ticket, "live"); // not joinable, last
    }
}
