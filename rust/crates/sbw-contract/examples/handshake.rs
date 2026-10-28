//! Native end-to-end protocol exercise; does not start Minecraft or Horizon.
use sbw_contract::{
    PROTOCOL,
    session::{InputGate, ServerPolicy, SnapshotAssembler, verify_welcome},
    types::*,
    wire,
};

fn main() {
    let hello = Hello {
        protocol: PROTOCOL,
        content_sha256: vec![7; 32],
        features: vec![Feature::AttackInput],
    };
    let client_bytes = wire::encode_client(&ClientPacket::Hello(hello.clone())).unwrap();
    let ClientPacket::Hello(received) = wire::decode_client(&client_bytes).unwrap() else {
        unreachable!()
    };
    let policy = ServerPolicy::new([7; 32], vec![Feature::AttackInput]).unwrap();
    // Real adapters generate an unpredictable per-connection token, not this fixture UUID.
    let welcome = policy
        .negotiate(&received, Uuid { high: 0, low: 1 }, 100)
        .unwrap();
    let server_bytes = wire::encode_server(&ServerPacket::Welcome(welcome)).unwrap();
    let ServerPacket::Welcome(welcome) = wire::decode_server(&server_bytes).unwrap() else {
        unreachable!()
    };
    verify_welcome(&hello, &welcome).unwrap();

    let input = Input {
        session: welcome.session,
        sequence: 1,
        client_tick: 100,
        intent: Intent::Weapon(WeaponInput {
            target: WeaponRef {
                instance: Uuid { high: 0, low: 2 },
                slot: 0,
                equip_revision: 0,
            },
            action: WeaponAction::TriggerPress,
        }),
    };
    let bytes = wire::encode_client(&ClientPacket::Input(input)).unwrap();
    let ClientPacket::Input(received) = wire::decode_client(&bytes).unwrap() else {
        unreachable!()
    };
    let mut gate = InputGate::new(welcome.session).unwrap();
    gate.accept(&received, 100).unwrap();
    assert_eq!(
        gate.accept(&received, 100),
        Err(RejectReason::ReplayedInput)
    );

    let snapshot = Snapshot {
        session: welcome.session,
        server_tick: 100,
        acknowledged_input: gate.last_sequence(),
        weapons: vec![],
        entities: vec![],
    };
    let mut client = SnapshotAssembler::new(welcome.session).unwrap();
    for page in wire::snapshot_pages(&snapshot).unwrap() {
        let bytes = wire::encode_server(&ServerPacket::Snapshot(page)).unwrap();
        let ServerPacket::Snapshot(page) = wire::decode_server(&bytes).unwrap() else {
            unreachable!()
        };
        assert_eq!(client.push(page).unwrap(), Some(snapshot.clone()));
    }
    println!("Protocol 0.1 negotiated; input 1 acknowledged; replay rejected; snapshot applied.");
}
