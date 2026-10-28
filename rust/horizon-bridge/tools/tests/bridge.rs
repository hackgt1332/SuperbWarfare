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
            reason: RejectReason::StaleTarget
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
    let packets = to_client(&mut server, &mut client, 2);
    assert_eq!(
        packets
            .iter()
            .filter(|p| matches!(p, ServerPacket::Welcome(_)))
            .count(),
        1
    );
    assert!(
        packets
            .iter()
            .all(|p| !matches!(p, ServerPacket::Snapshot(s) if s.acknowledged_input.is_some()))
    );
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
    let packets = to_client(&mut server, &mut client, 2);
    assert_eq!(
        packets
            .iter()
            .filter(|p| matches!(p, ServerPacket::Welcome(_)))
            .count(),
        1
    );
    assert!(
        packets
            .iter()
            .all(|p| !matches!(p, ServerPacket::Snapshot(s) if s.acknowledged_input.is_some()))
    );
    client.tick(7);
    assert_eq!(status(&mut client), "Connected; acknowledged input: None");
}

fn gameplay_snapshot(state: Vec<u8>, ammo: u32, item: &str) -> Snapshot {
    let mut snapshot = snapshot();
    snapshot.inventories = vec![(
        "superbwarfare".into(),
        horizon_mods::PlayerInventory {
            player: PLAYER.into(),
            stacks: vec![
                horizon_mods::InventoryStack {
                    slot: 0,
                    item: item.into(),
                    count: 1,
                    selected: true,
                    state,
                },
                horizon_mods::InventoryStack {
                    slot: 1,
                    item: "superbwarfare:rifle_ammo".into(),
                    count: ammo,
                    selected: false,
                    state: vec![],
                },
            ],
        },
    )];
    snapshot
}
fn edits(server: &mut Mods) -> Vec<horizon_mods::InventoryEdit> {
    server
        .drain()
        .into_iter()
        .filter_map(|o| {
            if let Output::Gameplay { mod_id, edit } = o {
                assert_eq!(mod_id, "superbwarfare");
                Some(edit)
            } else {
                None
            }
        })
        .collect()
}
fn send_intent(server: &mut Mods, session: Uuid, sequence: u64, intent: Intent) {
    let bytes = wire::encode_client(&ClientPacket::Input(Input {
        session,
        sequence,
        client_tick: 999999,
        intent,
    }))
    .unwrap();
    assert!(server.receive_network_from("superbwarfare", PLAYER, &bytes, 1));
}
#[test]
fn real_server_reserves_reload_and_emits_atomic_shot_edits() {
    let (mut client, mut server) = pair();
    client.tick(0);
    to_server(&mut client, &mut server, 1);
    server.server_tick(1);
    let welcome = to_client(&mut server, &mut client, 1)
        .into_iter()
        .find_map(|p| {
            if let ServerPacket::Welcome(w) = p {
                Some(w)
            } else {
                None
            }
        })
        .unwrap();
    server.set_snapshot(gameplay_snapshot(vec![], 64, sbw_sim::AK47));
    server.server_tick(2);
    let init = edits(&mut server).remove(0);
    assert!(init.shot.is_none());
    assert_eq!(init.consume, 0);
    let gun = sbw_sim::gun(sbw_sim::AK47).unwrap();
    let mut bytes = init.state;
    let target = sbw_sim::Weapon::decode(&bytes, &gun).unwrap().target(0, 1);
    server.set_snapshot(gameplay_snapshot(bytes.clone(), 64, sbw_sim::AK47));
    send_intent(
        &mut server,
        welcome.session,
        1,
        Intent::Weapon(WeaponInput {
            target,
            action: WeaponAction::Reload,
        }),
    );
    server.server_tick(3);
    edits(&mut server);
    let mut ammo = 64;
    let mut consumed = 0;
    for tick in 4..=68 {
        server.set_snapshot(gameplay_snapshot(bytes.clone(), ammo, sbw_sim::AK47));
        server.server_tick(tick);
        for edit in edits(&mut server) {
            assert_eq!(edit.expected, bytes);
            assert!(edit.shot.is_none());
            consumed += edit.consume;
            ammo -= edit.consume;
            bytes = edit.state;
        }
    }
    assert_eq!(consumed, 30);
    assert_eq!(ammo, 34);
    assert_eq!(sbw_sim::Weapon::decode(&bytes, &gun).unwrap().magazine, 30);
    server.set_snapshot(gameplay_snapshot(bytes.clone(), ammo, sbw_sim::AK47));
    send_intent(
        &mut server,
        welcome.session,
        2,
        Intent::Weapon(WeaponInput {
            target,
            action: WeaponAction::TriggerPress,
        }),
    );
    server.server_tick(69);
    edits(&mut server);
    server.server_tick(70);
    let shot = edits(&mut server).remove(0);
    assert_eq!(shot.expected, bytes);
    assert_eq!(shot.consume, 0);
    assert_eq!(shot.shot.unwrap().damage, 9.);
    assert_eq!(
        sbw_sim::Weapon::decode(&shot.state, &gun).unwrap().magazine,
        29
    );
    bytes = shot.state;
    server.set_snapshot(gameplay_snapshot(bytes.clone(), ammo, sbw_sim::AK47));
    send_intent(
        &mut server,
        welcome.session,
        2,
        Intent::Weapon(WeaponInput {
            target,
            action: WeaponAction::TriggerPress,
        }),
    );
    server.server_tick(71);
    let output = server.drain();
    assert!(
        output
            .iter()
            .all(|o| !matches!(o,Output::Gameplay {edit,..} if edit.shot.is_some()))
    );
    assert!(output.iter().any(|o|matches!(o,Output::Network{data,..} if matches!(wire::decode_server(data).unwrap(),ServerPacket::Rejected(Rejection{reason:RejectReason::ReplayedInput,..})))));
    assert_eq!(server.entries()[0].status, Status::Running);
    // A reconnect rotates the host token before the replacement Hello runs.
    // Held controls from the departed connection must not fire for its successor.
    let mut saved = sbw_sim::Weapon::decode(&bytes, &gun).unwrap();
    saved.cooldown = 0;
    server.set_snapshot(gameplay_snapshot(saved.encode(), ammo, sbw_sim::AK47));
    let stale = wire::encode_client(&ClientPacket::Input(Input {
        session: welcome.session,
        sequence: 3,
        client_tick: 72,
        intent: Intent::Weapon(WeaponInput {
            target,
            action: WeaponAction::TriggerPress,
        }),
    }))
    .unwrap();
    assert!(server.receive_network_from("superbwarfare", PLAYER, &stale, 2));
    server.server_tick(72);
    assert!(
        server
            .drain()
            .iter()
            .all(|o| !matches!(o, Output::Gameplay { edit, .. } if edit.shot.is_some()))
    );
}
#[test]
fn real_server_deploys_only_the_selected_vehicle_item() {
    let (mut client, mut server) = pair();
    client.tick(0);
    to_server(&mut client, &mut server, 1);
    server.server_tick(1);
    let token = to_client(&mut server, &mut client, 1)
        .into_iter()
        .find_map(|p| {
            if let ServerPacket::Welcome(w) = p {
                Some(w.session)
            } else {
                None
            }
        })
        .unwrap();
    server.set_snapshot(gameplay_snapshot(vec![], 0, sbw_sim::PICKUP));
    send_intent(&mut server, token, 1, Intent::DeployVehicle(1));
    server.server_tick(2);
    assert!(edits(&mut server).is_empty());
    send_intent(&mut server, token, 2, Intent::DeployVehicle(0));
    server.server_tick(3);
    let spawn = edits(&mut server).remove(0);
    assert_eq!(spawn.slot, 0);
    assert_eq!(spawn.item, sbw_sim::PICKUP);
    assert_eq!(spawn.vehicle.as_deref(), Some(sbw_sim::pickup_profile()));
    assert!(spawn.shot.is_none());
}

#[test]
fn content_package_defines_all_playable_items_and_recipes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("client");
    let manifest = horizon_mods::Manifest::parse(
        &std::fs::read_to_string(root.join("horizon-mod.toml")).unwrap(),
    )
    .unwrap();
    let content = horizon_mods::content::Content::read(&root, &manifest).unwrap();
    assert_eq!(content.items.len(), 6);
    let mut reader = content.reader().unwrap();
    for id in [
        "ak_47",
        "glock_17",
        "rifle_ammo",
        "handgun_ammo",
        "sodayo_pick_up",
        "truck",
    ] {
        assert!(
            !reader
                .read(&format!("assets/superbwarfare/textures/item/{id}.png"))
                .unwrap()
                .is_empty()
        );
        assert!(
            !reader
                .read(&format!("data/superbwarfare/recipe/{id}.json"))
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn reequipping_the_same_gun_rejects_the_previous_equip_reference() {
    let (mut client, mut server) = pair();
    client.tick(0);
    to_server(&mut client, &mut server, 1);
    server.server_tick(1);
    let session = to_client(&mut server, &mut client, 1)
        .into_iter()
        .find_map(|packet| match packet {
            ServerPacket::Welcome(welcome) => Some(welcome.session),
            _ => None,
        })
        .unwrap();
    let mut saved = sbw_sim::Weapon::new(Uuid { high: 1, low: 2 });
    saved.magazine = 5;
    let bytes = saved.encode();
    server.set_snapshot(gameplay_snapshot(bytes.clone(), 64, sbw_sim::AK47));
    server.server_tick(2);
    let previous = to_client(&mut server, &mut client, 1)
        .into_iter()
        .find_map(|packet| match packet {
            ServerPacket::Snapshot(page) => page.weapons.into_iter().next().map(|w| w.target),
            _ => None,
        })
        .unwrap();
    assert_eq!(previous.equip_revision, 1);

    // Observe a real departure from the weapon, then reselect the exact same
    // saved instance in the exact same slot. A delayed pre-switch input must
    // remain stale despite its sequence never having been processed before.
    server.set_snapshot(gameplay_snapshot(vec![], 64, "minecraft:stick"));
    server.server_tick(3);
    server.drain();
    server.set_snapshot(gameplay_snapshot(bytes.clone(), 64, sbw_sim::AK47));
    send_intent(
        &mut server,
        session,
        1,
        Intent::Weapon(WeaponInput {
            target: previous,
            action: WeaponAction::Reload,
        }),
    );
    server.server_tick(4);
    let packets = to_client(&mut server, &mut client, 1);
    assert!(packets.iter().any(|packet| matches!(
        packet,
        ServerPacket::Rejected(Rejection {
            sequence: Some(1),
            reason: RejectReason::StaleTarget
        })
    )));
    let current = packets
        .into_iter()
        .find_map(|packet| match packet {
            ServerPacket::Snapshot(page) => page.weapons.into_iter().next().map(|w| w.target),
            _ => None,
        })
        .unwrap();
    assert_eq!(current.instance, previous.instance);
    assert_eq!(current.slot, previous.slot);
    assert_eq!(current.equip_revision, 2);

    send_intent(
        &mut server,
        session,
        2,
        Intent::Weapon(WeaponInput {
            target: current,
            action: WeaponAction::Reload,
        }),
    );
    server.server_tick(5);
    let packets = to_client(&mut server, &mut client, 1);
    assert!(packets.iter().all(|packet| !matches!(
        packet,
        ServerPacket::Rejected(Rejection {
            sequence: Some(2),
            ..
        })
    )));
    server.server_tick(6);
    let reload = edits(&mut server).remove(0);
    assert_eq!(reload.expected, bytes);
    assert_eq!(reload.consume, 25);
    assert!(reload.shot.is_none());
}

#[test]
fn vehicle_tracking_filters_recipient_dimension_before_the_tracking_budget() {
    let (mut client, mut server) = pair();
    negotiate(&mut client, &mut server);
    let mut view = snapshot();
    assert_eq!(view.players[0].dimension, "minecraft:overworld");
    let vehicle = |entity, definition: &str, dimension: &str, profile: &str| {
        (
            "superbwarfare".into(),
            horizon_mods::ModVehicle {
                entity,
                definition: definition.into(),
                dimension: dimension.into(),
                profile: profile.into(),
                x: 0.,
                y: 64.,
                z: 0.,
                yaw: 0.,
                vx: 0.,
                vy: 0.,
                vz: 0.,
            },
        )
    };
    // Other dimensions cannot consume either this recipient's first-eight
    // tracking slots or its serialized-profile byte budget.
    view.vehicles = (1..=9)
        .map(|id| {
            vehicle(
                id,
                sbw_sim::PICKUP,
                "minecraft:the_nether",
                sbw_sim::pickup_profile(),
            )
        })
        .collect();
    let overworld_profile = sbw_sim::vehicle_profile(sbw_sim::TRUCK).unwrap();
    view.vehicles.push(vehicle(
        10,
        sbw_sim::TRUCK,
        "minecraft:overworld",
        overworld_profile,
    ));
    server.set_snapshot(view);
    server.server_tick(4);
    let entities: Vec<_> = to_client(&mut server, &mut client, 1)
        .into_iter()
        .filter_map(|packet| match packet {
            ServerPacket::Snapshot(page) => Some(page.entities),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(entities.len(), 1);
    assert_eq!(entities[0].key.index, 10);
    assert_eq!(entities[0].entity.definition, sbw_sim::TRUCK);
    assert_eq!(entities[0].entity.dimension, "minecraft:overworld");
    assert_eq!(
        entities[0].entity.state.as_slice(),
        overworld_profile.as_bytes()
    );
}
