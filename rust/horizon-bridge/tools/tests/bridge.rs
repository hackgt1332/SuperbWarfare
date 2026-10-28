//! Real Horizon runtime + two real WASM guests; no Minecraft resources needed.
use horizon_mods::{Mods, Output, Package, Runs, Snapshot, Status, manifest::componentize};
use sbw_contract::{types::*, wire};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

const PLAYER: &str = "00000000-0000-0000-0000-000000000001";
fn packages() -> &'static (PathBuf, PathBuf) {
    static PACKAGES: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    PACKAGES.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let target = root.parent().unwrap().join("target/horizon-bridge");
        let status = Command::new(env!("CARGO"))
            .current_dir(root)
            .args([
                "build",
                "--locked",
                "--target",
                "wasm32-unknown-unknown",
                "-p",
                "sbw-horizon-client",
                "-p",
                "sbw-horizon-server",
                "--target-dir",
            ])
            .arg(&target)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "install the wasm32-unknown-unknown Rust target"
        );
        let mut paths = Vec::new();
        for side in ["client", "server"] {
            let manifest =
                std::fs::read_to_string(root.join(format!("{side}/horizon-mod.toml"))).unwrap();
            let core = std::fs::read(target.join(format!(
                "wasm32-unknown-unknown/debug/sbw_horizon_{side}.wasm"
            )))
            .unwrap();
            let path = target.join(format!("superbwarfare-{side}.hmod"));
            std::fs::write(
                &path,
                Package::write(&manifest, &componentize(&core).unwrap()).unwrap(),
            )
            .unwrap();
            paths.push(path);
        }
        (paths.remove(0), paths.remove(0))
    })
}
fn snapshot() -> Snapshot {
    Snapshot {
        network_connected: true,
        players: vec![horizon_mods::ServerPlayer {
            id: PLAYER.into(),
            name: "Test".into(),
            x: 0.0,
            y: 64.0,
            z: 0.0,
            dimension: "minecraft:overworld".into(),
            game_mode: horizon_mods::GameMode::Survival,
            health: 20.0,
        }],
        ..Default::default()
    }
}
fn pair() -> (Mods, Mods) {
    let mut client = Mods::load_for(Runs::Client, &[packages().0.clone()], |_| true, |_| false);
    let mut server = Mods::load_for(Runs::Server, &[packages().1.clone()], |_| true, |_| false);
    assert_eq!(client.entries()[0].status, Status::Running);
    assert_eq!(server.entries()[0].status, Status::Running);
    client.set_snapshot(snapshot());
    server.set_snapshot(snapshot());
    (client, server)
}
fn to_server(client: &mut Mods, server: &mut Mods, connection: u64) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    for output in client.drain() {
        if let Output::Network {
            mod_id,
            player: None,
            data,
            ..
        } = output
        {
            assert_eq!(mod_id, "superbwarfare");
            assert!(server.receive_network_from(&mod_id, PLAYER, &data, connection));
            packets.push(data);
        }
    }
    packets
}
fn to_client(server: &mut Mods, client: &mut Mods, connection: u64) -> Vec<ServerPacket> {
    let mut packets = Vec::new();
    for output in server.drain() {
        if let Output::Network {
            mod_id,
            player: Some(player),
            connection: Some(key),
            data,
        } = output
        {
            assert_eq!(player, PLAYER);
            assert_eq!(key, connection);
            packets.push(wire::decode_server(&data).unwrap());
            assert!(client.receive_network(&mod_id, None, &data));
        }
    }
    packets
}
fn status(client: &mut Mods) -> String {
    assert!(client.command("sbw_status"));
    client
        .drain()
        .into_iter()
        .find_map(|o| match o {
            Output::Feedback { text, .. } => Some(text),
            _ => None,
        })
        .unwrap()
}
fn negotiate(client: &mut Mods, server: &mut Mods) -> Vec<u8> {
    client.tick(0);
    let hello = to_server(client, server, 1).remove(0);
    assert!(matches!(
        wire::decode_client(&hello).unwrap(),
        ClientPacket::Hello(_)
    ));
    server.server_tick(1);
    assert!(matches!(
        to_client(server, client, 1).as_slice(),
        [ServerPacket::Welcome(_)]
    ));
    client.tick(2);
    assert!(status(client).starts_with("Connected"));
    hello
}

#[test]
fn real_guests_negotiate_acknowledge_and_reject_replayed_intent() {
    let (mut client, mut server) = pair();
    negotiate(&mut client, &mut server);
    client.command("sbw_probe");
    let input = to_server(&mut client, &mut server, 1).remove(0);
    server.server_tick(3);
    let packets = to_client(&mut server, &mut client, 1);
    assert!(packets.iter().any(|p| matches!(
        p,
        ServerPacket::Rejected(Rejection {
            sequence: Some(1),
            reason: RejectReason::NotReady
        })
    )));
    assert!(packets.iter().any(|p| matches!(p, ServerPacket::Snapshot(page) if page.server_tick == 3 && page.acknowledged_input == Some(1))));
    client.tick(4);
    assert!(status(&mut client).contains("Some(1)"));
    assert!(server.receive_network_from("superbwarfare", PLAYER, &input, 1));
    server.server_tick(5);
    assert!(matches!(
        to_client(&mut server, &mut client, 1).as_slice(),
        [ServerPacket::Rejected(Rejection {
            sequence: Some(1),
            reason: RejectReason::ReplayedInput
        })]
    ));
}

#[test]
fn content_and_version_mismatches_never_activate_gameplay() {
    let (mut client, mut server) = pair();
    client.tick(0);
    let bytes = to_server(&mut client, &mut server, 1).remove(0);
    // Discard the valid Hello already queued before these negative cases.
    let (mut client, mut server) = pair();
    let ClientPacket::Hello(original) = wire::decode_client(&bytes).unwrap() else {
        panic!()
    };
    for (mut hello, reason) in [
        (original.clone(), RejectReason::ContentMismatch),
        (original, RejectReason::IncompatibleVersion),
    ] {
        match reason {
            RejectReason::ContentMismatch => hello.content_sha256[0] ^= 1,
            _ => hello.protocol.minor += 1,
        }
        let frame = wire::encode_client(&ClientPacket::Hello(hello)).unwrap();
        assert!(server.receive_network_from("superbwarfare", PLAYER, &frame, 1));
        server.server_tick(1);
        assert!(
            matches!(to_client(&mut server, &mut client, 1).as_slice(), [ServerPacket::Rejected(rejection)] if rejection.reason == reason)
        );
    }
    assert!(!server.receive_network_from("other_mod", PLAYER, &bytes, 1));
    assert!(!server.receive_network_from("superbwarfare", PLAYER, &vec![0; 16385], 1));
}

#[test]
fn reconnect_rotates_tokens_and_late_departure_cannot_remove_the_new_session() {
    let (mut client, mut server) = pair();
    negotiate(&mut client, &mut server);
    client.command("sbw_probe");
    let old_input = to_server(&mut client, &mut server, 1).remove(0);
    server.server_tick(3);
    to_client(&mut server, &mut client, 1);
    client.tick(4);
    client.network_disconnected();
    client.set_snapshot(snapshot());
    client.tick(5);
    to_server(&mut client, &mut server, 2);
    server.server_tick(6);
    server.network_peer_left(PLAYER, 1);
    assert!(matches!(
        to_client(&mut server, &mut client, 2).as_slice(),
        [ServerPacket::Welcome(_)]
    ));
    client.tick(7);
    assert!(status(&mut client).starts_with("Connected"));
    assert!(server.receive_network_from("superbwarfare", PLAYER, &old_input, 2));
    server.server_tick(8);
    assert!(matches!(
        to_client(&mut server, &mut client, 2).as_slice(),
        [ServerPacket::Rejected(Rejection {
            reason: RejectReason::WrongSession,
            ..
        })]
    ));
}

#[test]
fn a_burst_produces_one_snapshot_with_the_latest_acknowledgement() {
    let (mut client, mut server) = pair();
    negotiate(&mut client, &mut server);
    for _ in 0..4 {
        client.command("sbw_probe");
    }
    assert_eq!(to_server(&mut client, &mut server, 1).len(), 4);
    server.server_tick(10);
    let packets = to_client(&mut server, &mut client, 1);
    let snapshots: Vec<_> = packets
        .iter()
        .filter_map(|p| {
            if let ServerPacket::Snapshot(p) = p {
                Some(p)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].acknowledged_input, Some(4));
    client.tick(11);
    assert!(status(&mut client).contains("Some(4)"));
}

#[test]
fn deferred_old_hello_cannot_renegotiate_a_replacement_connection() {
    let (mut client, mut server) = pair();
    let old_hello = negotiate(&mut client, &mut server);
    // Fill this tick's processing budget, leaving one old Hello inside the guest.
    for _ in 0..4 {
        client.command("sbw_probe");
    }
    to_server(&mut client, &mut server, 1);
    assert!(server.receive_network_from("superbwarfare", PLAYER, &old_hello, 1));
    server.server_tick(3);
    to_client(&mut server, &mut client, 1);
    client.tick(4);

    client.network_disconnected();
    client.set_snapshot(snapshot());
    client.tick(5);
    to_server(&mut client, &mut server, 2);
    server.server_tick(6);
    // Only the new Hello is handled. The deferred request retained its old token.
    assert!(matches!(
        to_client(&mut server, &mut client, 2).as_slice(),
        [ServerPacket::Welcome(_)]
    ));
    client.tick(7);
    assert_eq!(status(&mut client), "Connected; acknowledged input: None");
}
