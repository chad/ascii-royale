//! End-to-end test over the real iroh stack: a remote client joins a hosted
//! lobby, the match starts, snapshots flow both ways, and a disconnect
//! resolves the match.
//!
//! Ignored by default because it needs the network (n0 discovery + relay).
//! Run with: cargo test --test e2e -- --ignored

use std::time::Duration;

use ascii_royale::net::client;
use ascii_royale::net::host::{self, HostOpts, ServeOpts};
use ascii_royale::net::protocol::{ClientMsg, ServerMsg};
use tokio::time::timeout;

async fn next_msg(handle: &mut ascii_royale::net::protocol::ServerHandle) -> ServerMsg {
    timeout(Duration::from_secs(30), handle.rx.recv())
        .await
        .expect("timed out waiting for server message")
        .expect("server channel closed")
}

#[tokio::test]
#[ignore = "needs network access to n0 discovery/relay"]
async fn remote_player_joins_plays_and_disconnects() {
    let hosted = timeout(
        Duration::from_secs(60),
        host::start(HostOpts { name: "hostess".into(), bots: 0, networked: true }),
    )
    .await
    .expect("endpoint bind timed out")
    .expect("host start failed");
    let mut host_handle = hosted.handle;
    let ticket = hosted.ticket.expect("networked host must have a ticket");

    // Host got its own Welcome.
    let ServerMsg::Welcome { id: 0, .. } = next_msg(&mut host_handle).await else {
        panic!("host should be player 0");
    };

    // Remote client dials by ticket.
    let mut remote = timeout(Duration::from_secs(60), client::connect(&ticket, "wanderer"))
        .await
        .expect("connect timed out")
        .expect("connect failed");
    let ServerMsg::Welcome { id: remote_id, .. } = next_msg(&mut remote).await else {
        panic!("remote should be welcomed");
    };
    assert_eq!(remote_id, 1);

    // Both sides hear about the roster of two.
    loop {
        if let ServerMsg::Roster { aboard, .. } = next_msg(&mut remote).await {
            if aboard.len() == 2 {
                break;
            }
        }
    }

    // Host starts the match; both sides should start receiving snapshots.
    host_handle.tx.send(ClientMsg::Start).await.unwrap();
    loop {
        if let ServerMsg::Snapshot(s) = next_msg(&mut remote).await {
            assert_eq!(s.alive, 2);
            break;
        }
    }
    // Remote can act without the host falling over.
    remote.tx.send(ClientMsg::Input(ascii_royale::game::state::InputCmd::Fire)).await.unwrap();

    // Remote rage-quits: host should win by forfeit and get the End screen.
    drop(remote);
    loop {
        match next_msg(&mut host_handle).await {
            ServerMsg::End { standings } => {
                let winner = standings.iter().find(|s| s.placement == Some(1)).unwrap();
                assert_eq!(winner.name, "hostess");
                assert!(winner.is_you);
                return;
            }
            _ => continue,
        }
    }
}

/// Arena lifecycle: auto-start with one human, mid-match joiners queue,
/// and after the match the lobby reopens and the queued player is seated.
#[tokio::test]
#[ignore = "needs network access to n0 discovery/relay"]
async fn arena_auto_starts_queues_and_resets() {
    let ticket = timeout(
        Duration::from_secs(60),
        host::serve(ServeOpts {
            bots: 1,
            auto_start_secs: 1,
            auto_reset_secs: 1,
            ticket_file: None,
            http_port: None,
            stats_file: None,
            browser_play_url: None,
        }),
    )
    .await
    .expect("serve bind timed out")
    .expect("serve failed");

    // First player joins the empty arena.
    let mut alice = client::connect(&ticket, "alice").await.expect("alice connect");
    let ServerMsg::Welcome { .. } = next_msg(&mut alice).await else {
        panic!("alice should be welcomed into the lobby");
    };

    // The arena counts down and starts on its own (1s + 3s countdown).
    loop {
        if let ServerMsg::Snapshot(s) = next_msg(&mut alice).await {
            assert_eq!(s.alive, 2, "alice + one bot");
            break;
        }
    }

    // Bob arrives mid-match: he must be queued, not rejected.
    let mut bob = client::connect(&ticket, "bob").await.expect("bob connect");
    loop {
        match next_msg(&mut bob).await {
            ServerMsg::Waiting { .. } => break,
            ServerMsg::Rejected { reason } => panic!("bob rejected: {reason}"),
            _ => {}
        }
    }

    // Alice rage-quits; the bot wins; the arena resets; bob gets seated.
    drop(alice);
    loop {
        match next_msg(&mut bob).await {
            ServerMsg::Welcome { .. } => break,
            ServerMsg::Waiting { .. } | ServerMsg::Roster { .. } => {}
            other => panic!("expected bob's Welcome after reset, got {other:?}"),
        }
    }
    // And the new lobby counts down for him too.
    loop {
        match next_msg(&mut bob).await {
            ServerMsg::Roster { starting_in: Some(_), .. } => break,
            ServerMsg::Roster { .. } | ServerMsg::Waiting { .. } => {}
            other => panic!("expected countdown roster, got {other:?}"),
        }
    }
}
