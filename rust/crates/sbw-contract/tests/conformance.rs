use sbw_contract::{
    PROTOCOL, TICK_RATE, limits,
    session::{
        EffectGate, InputGate, ServerPolicy, SnapshotAssembler, SnapshotGate, verify_welcome,
    },
    types::*,
    validate::{self, Validate},
    wire::{self, WireError},
};

fn uuid(low: u64) -> Uuid {
    Uuid { high: 0, low }
}
fn key(index: u64) -> EntityKey {
    EntityKey {
        index,
        generation: 1,
    }
}
fn zero() -> Vec3 {
    Vec3 {
        x: 0.0,
        y: 0.0,
        z: 0.0,
    }
}
fn hello() -> Hello {
    Hello {
        protocol: PROTOCOL,
        content_sha256: vec![7; 32],
        features: vec![Feature::AttackInput, Feature::ModChannel],
    }
}
fn input(sequence: u64) -> Input {
    Input {
        session: uuid(1),
        sequence,
        client_tick: 500,
        intent: Intent::Weapon(WeaponInput {
            target: WeaponRef {
                instance: uuid(2),
                slot: 0,
                equip_revision: 3,
            },
            action: WeaponAction::TriggerPress,
        }),
    }
}
fn entity(index: u64) -> EntityState {
    EntityState {
        key: key(index),
        revision: 1,
        entity: EntityRecord {
            instance: uuid(index + 100),
            definition: "superbwarfare:projectile".into(),
            dimension: "minecraft:overworld".into(),
            state_schema: 1,
            state: vec![0; limits::STATE_BYTES],
            pose: Transform {
                position: zero(),
                orientation: Rotation {
                    x: 0.0,
                    y: 0.0,
                    z: 0.0,
                    w: 1.0,
                },
                velocity: zero(),
            },
        },
    }
}
fn snapshot(tick: u64, count: usize) -> Snapshot {
    Snapshot {
        session: uuid(1),
        server_tick: tick,
        acknowledged_input: Some(1),
        weapons: vec![],
        entities: (1..=count).map(|i| entity(i as u64)).collect(),
    }
}
fn effect(id: u64) -> Effect {
    Effect {
        id,
        kind: EffectKind::Shot,
        resource_id: "superbwarfare:ak_47/fire".into(),
        dimension: "minecraft:overworld".into(),
        position: zero(),
        direction: zero(),
        source: None,
    }
}

#[test]
fn negotiation_checks_version_content_and_features_on_both_sides() {
    let policy = ServerPolicy::new([7; 32], vec![Feature::AttackInput]).unwrap();
    let welcome = policy.negotiate(&hello(), uuid(1), 42).unwrap();
    assert_eq!(welcome.tick_rate, TICK_RATE);
    assert_eq!(verify_welcome(&hello(), &welcome), Ok(()));
    let mut wrong = hello();
    wrong.protocol.minor += 1;
    assert_eq!(
        policy.negotiate(&wrong, uuid(1), 42),
        Err(RejectReason::IncompatibleVersion)
    );
    wrong = hello();
    wrong.content_sha256[0] ^= 1;
    assert_eq!(
        policy.negotiate(&wrong, uuid(1), 42),
        Err(RejectReason::ContentMismatch)
    );
    wrong = hello();
    wrong.features.clear();
    assert_eq!(
        policy.negotiate(&wrong, uuid(1), 42),
        Err(RejectReason::MissingFeature)
    );
    assert_eq!(
        verify_welcome(&wrong, &welcome),
        Err(RejectReason::MissingFeature)
    );
    let mut wrong_welcome = welcome;
    wrong_welcome.content_sha256[0] ^= 1;
    assert_eq!(
        verify_welcome(&hello(), &wrong_welcome),
        Err(RejectReason::ContentMismatch)
    );
}

#[test]
fn malformed_negotiation_cannot_activate_a_session() {
    let policy = ServerPolicy::new([7; 32], vec![]).unwrap();
    assert_eq!(
        policy.negotiate(&hello(), uuid(0), 0),
        Err(RejectReason::InvalidInput)
    );
    let mut malformed = hello();
    malformed.content_sha256.pop();
    assert_eq!(
        policy.negotiate(&malformed, uuid(1), 0),
        Err(RejectReason::InvalidInput)
    );
    malformed = hello();
    malformed.features.push(Feature::AttackInput);
    assert!(malformed.validate().is_err());
    assert!(ServerPolicy::new([7; 32], vec![Feature::AttackInput; 2]).is_err());
}

#[test]
fn authenticated_input_rejects_replay_and_old_connections() {
    let mut gate = InputGate::new(uuid(1)).unwrap();
    assert_eq!(gate.accept(&input(1), 10), Ok(()));
    assert_eq!(gate.accept(&input(1), 11), Err(RejectReason::ReplayedInput));
    let mut old = input(2);
    old.session = uuid(9);
    assert_eq!(gate.accept(&old, 11), Err(RejectReason::WrongSession));
    assert_eq!(gate.last_sequence(), Some(1));
    assert_eq!(gate.accept(&input(2), 11), Ok(()));
    let mut new_connection = InputGate::new(uuid(9)).unwrap();
    assert_eq!(
        new_connection.accept(&input(3), 11),
        Err(RejectReason::WrongSession)
    );
}

#[test]
fn rate_limit_uses_server_ticks_and_rejected_input_does_not_advance() {
    let mut gate = InputGate::new(uuid(1)).unwrap();
    for seq in 1..=limits::INPUTS_PER_TICK as u64 {
        let mut v = input(seq);
        v.client_tick = seq * 1000;
        assert_eq!(gate.accept(&v, 7), Ok(()));
    }
    let next = input(limits::INPUTS_PER_TICK as u64 + 1);
    assert_eq!(gate.accept(&next, 7), Err(RejectReason::RateLimited));
    assert_eq!(gate.accept(&next, 6), Err(RejectReason::InvalidInput));
    assert_eq!(gate.accept(&next, 8), Ok(()));
}

#[test]
fn sequences_never_wrap_and_malformed_intent_does_not_consume_one() {
    let mut gate = InputGate::new(uuid(1)).unwrap();
    let mut v = input(1);
    if let Intent::Weapon(w) = &mut v.intent {
        w.target.slot = 9;
    }
    assert_eq!(gate.accept(&v, 1), Err(RejectReason::InvalidInput));
    assert_eq!(gate.last_sequence(), None);
    assert_eq!(gate.accept(&input(u64::MAX), 1), Ok(()));
    assert_eq!(gate.accept(&input(0), 2), Err(RejectReason::InvalidInput));
    assert_eq!(gate.accept(&input(1), 2), Err(RejectReason::ReplayedInput));
}

#[test]
fn snapshot_pages_round_trip_and_commit_atomically_at_maximum_entity_count() {
    let expected = snapshot(10, limits::TRACKED_ENTITIES);
    let pages = wire::snapshot_pages(&expected).unwrap();
    assert!(pages.len() > 1 && pages.len() <= limits::SNAPSHOT_PAGES);
    let mut assembler = SnapshotAssembler::new(uuid(1)).unwrap();
    // Page order does not affect the resulting tracked set.
    for (i, page) in pages.iter().rev().enumerate() {
        let bytes = wire::encode_server(&ServerPacket::Snapshot(page.clone())).unwrap();
        assert!(bytes.len() <= limits::PACKET_BYTES);
        let ServerPacket::Snapshot(decoded) = wire::decode_server(&bytes).unwrap() else {
            panic!()
        };
        let result = assembler.push(decoded).unwrap();
        if i + 1 == pages.len() {
            assert_eq!(result, Some(expected.clone()));
        } else {
            assert_eq!(result, None);
        }
    }
    assert_eq!(
        assembler.push(pages[0].clone()),
        Err(RejectReason::ReplayedInput)
    );
}

#[test]
fn a_new_snapshot_supersedes_an_incomplete_one_without_applying_it() {
    let old = wire::snapshot_pages(&snapshot(10, 8)).unwrap();
    let mut assembler = SnapshotAssembler::new(uuid(1)).unwrap();
    assert_eq!(assembler.push(old[0].clone()), Ok(None));
    let next = snapshot(11, 0);
    assert_eq!(
        assembler.push(wire::snapshot_pages(&next).unwrap().remove(0)),
        Ok(Some(next))
    );
    assert_eq!(
        assembler.push(old[1].clone()),
        Err(RejectReason::ReplayedInput)
    );
}

#[test]
fn inconsistent_and_duplicate_pages_do_not_commit() {
    let pages = wire::snapshot_pages(&snapshot(10, 8)).unwrap();
    let mut assembler = SnapshotAssembler::new(uuid(1)).unwrap();
    assert_eq!(assembler.push(pages[0].clone()), Ok(None));
    assert_eq!(
        assembler.push(pages[0].clone()),
        Err(RejectReason::ReplayedInput)
    );
    let mut bad = pages[1].clone();
    bad.acknowledged_input = Some(2);
    assert_eq!(assembler.push(bad), Err(RejectReason::InvalidInput));
    let mut complete = None;
    for page in &pages[1..] {
        complete = assembler.push(page.clone()).unwrap();
    }
    assert_eq!(complete, Some(snapshot(10, 8)));
}

#[test]
fn duplicate_entities_across_pages_are_rejected_at_commit() {
    let mut pages = wire::snapshot_pages(&snapshot(10, 8)).unwrap();
    pages[1].entities[0] = pages[0].entities[0].clone();
    let mut assembler = SnapshotAssembler::new(uuid(1)).unwrap();
    let last = pages.pop().unwrap();
    for page in pages {
        assert_eq!(assembler.push(page), Ok(None));
    }
    assert_eq!(assembler.push(last), Err(RejectReason::InvalidInput));
}

#[test]
fn snapshot_sessions_ticks_and_acknowledgements_cannot_regress() {
    let mut gate = SnapshotGate::new(uuid(1)).unwrap();
    assert_eq!(gate.accept(&snapshot(0, 0)), Ok(()));
    let mut next = snapshot(1, 0);
    next.acknowledged_input = None;
    assert_eq!(gate.accept(&next), Err(RejectReason::InvalidInput));
    next.acknowledged_input = Some(2);
    next.session = uuid(2);
    assert_eq!(gate.accept(&next), Err(RejectReason::WrongSession));
    next.session = uuid(1);
    assert_eq!(gate.accept(&next), Ok(()));
    assert_eq!(
        gate.accept(&snapshot(0, 0)),
        Err(RejectReason::ReplayedInput)
    );
}

#[test]
fn effects_have_session_scoped_once_only_ids() {
    let mut gate = EffectGate::new(uuid(1)).unwrap();
    let mut batch = Effects {
        session: uuid(1),
        server_tick: 10,
        events: vec![effect(1), effect(2)],
    };
    assert_eq!(gate.accept(&batch), Ok(()));
    assert_eq!(gate.accept(&batch), Err(RejectReason::ReplayedInput));
    batch.events = vec![effect(4), effect(3)];
    assert_eq!(gate.accept(&batch), Err(RejectReason::InvalidInput));
    batch.events = vec![effect(3)];
    assert_eq!(gate.accept(&batch), Ok(()));
    batch.session = uuid(2);
    assert_eq!(gate.accept(&batch), Err(RejectReason::WrongSession));
}

#[test]
fn wire_rejects_wrong_direction_truncation_trailing_and_oversized_data() {
    let bytes = wire::encode_client(&ClientPacket::Input(input(1))).unwrap();
    assert!(matches!(
        wire::decode_server(&bytes),
        Err(WireError::WrongDirection)
    ));
    for length in 0..bytes.len() {
        assert!(wire::decode_client(&bytes[..length]).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(matches!(
        wire::decode_client(&trailing),
        Err(WireError::TrailingBytes)
    ));
    let mut wrong_format = bytes;
    wrong_format[4] += 1;
    assert!(matches!(
        wire::decode_client(&wrong_format),
        Err(WireError::InvalidHeader)
    ));
    assert!(matches!(
        wire::decode_client(&vec![0; limits::PACKET_BYTES + 1]),
        Err(WireError::TooLarge)
    ));
}

#[test]
fn non_finite_controls_are_rejected_even_if_they_bypass_the_encoder() {
    let mut v = input(1);
    v.intent = Intent::Vehicle(VehicleInput {
        target: key(1),
        seat: 0,
        action: VehicleAction::Controls(VehicleControls {
            throttle: f32::NAN,
            strafe: 0.0,
            lift: 0.0,
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
        }),
    });
    assert!(wire::encode_client(&ClientPacket::Input(v)).is_err());
    let mut malicious = b"SBW\0\x01\x01".to_vec();
    malicious.extend(postcard::to_allocvec(&ClientPacket::Input(v)).unwrap());
    assert!(matches!(
        wire::decode_client(&malicious),
        Err(WireError::InvalidValue(_))
    ));
}

#[test]
fn malformed_packet_corpus_does_not_panic() {
    // Deterministic malformed payload corpus, including huge declared lengths.
    for tag in 0..=255u8 {
        let mut bytes = b"SBW\0\x01\x01".to_vec();
        bytes.extend([tag; 256]);
        let _ = wire::decode_client(&bytes);
        bytes[5] = 2;
        let _ = wire::decode_server(&bytes);
    }
}

#[test]
fn identifiers_geometry_and_persisted_state_have_explicit_bounds() {
    for id in [
        "../x",
        "minecraft:",
        ":stone",
        "X:stone",
        "a:b/../c",
        "a:b//c",
        "a:b:c",
    ] {
        assert!(validate::resource_id(id).is_err(), "{id}");
    }
    assert!(validate::resource_id("superbwarfare:weapons/ak_47").is_ok());
    let mut v = entity(1);
    v.entity.pose.orientation.w = 2.0;
    assert!(v.validate().is_err());
    v = entity(1);
    v.entity.state.push(0);
    assert!(v.validate().is_err());
    v = entity(1);
    v.entity.state_schema = 0;
    assert!(v.validate().is_err());
    v = entity(1);
    v.entity.pose.position.x = f64::INFINITY;
    assert!(v.validate().is_err());
}

#[test]
fn inventory_edits_reject_ambiguous_writes_and_stack_identity() {
    let stack = Stack {
        instance: uuid(3),
        item: "superbwarfare:ak_47".into(),
        count: 1,
        state_schema: 1,
        state: vec![],
    };
    let first = Slot {
        index: 0,
        value: Some(stack.clone()),
    };
    let mut edit = InventoryEdit {
        expected_revision: 4,
        writes: vec![first.clone()],
    };
    assert!(edit.validate().is_ok());
    edit.writes.push(first);
    assert!(edit.validate().is_err());
    edit.writes[1].index = 1;
    assert!(edit.validate().is_err());
    edit.writes[1].value = None;
    assert!(edit.validate().is_ok());
    edit.writes[1].index = limits::INVENTORY_SLOTS as u16;
    assert!(edit.validate().is_err());
}

#[test]
fn collision_results_are_bounded_and_sorted() {
    let first = Hit {
        target: HitTarget::Entity(key(1)),
        position: zero(),
        normal: zero(),
        fraction: 0.25,
    };
    let mut second = first;
    second.fraction = 0.5;
    assert!(validate::hits(&[first, second]).is_ok());
    assert!(validate::hits(&[second, first]).is_err());
}

#[test]
fn server_player_view_carries_a_valid_selection_and_collision_handle() {
    let mut player = Player {
        id: uuid(1),
        entity: key(1),
        dimension: "minecraft:overworld".into(),
        position: zero(),
        yaw: 0.0,
        pitch: 0.0,
        spectator: false,
        selected_slot: 8,
        equip_revision: 7,
    };
    assert!(player.validate().is_ok());
    player.selected_slot = 9;
    assert!(player.validate().is_err());
    player.selected_slot = 0;
    player.entity.generation = 0;
    assert!(player.validate().is_err());
}

#[test]
fn every_weapon_action_and_vehicle_intent_round_trips() {
    for action in [
        WeaponAction::TriggerPress,
        WeaponAction::TriggerRelease,
        WeaponAction::Reload,
        WeaponAction::CancelReload,
        WeaponAction::CycleFireMode,
        WeaponAction::AimStart,
        WeaponAction::AimStop,
    ] {
        let mut v = input(1);
        if let Intent::Weapon(w) = &mut v.intent {
            w.action = action;
        }
        let packet = ClientPacket::Input(v);
        assert_eq!(
            wire::decode_client(&wire::encode_client(&packet).unwrap()).unwrap(),
            packet
        );
    }
    for action in [
        VehicleAction::Dismount,
        VehicleAction::ChangeSeat(2),
        VehicleAction::Controls(VehicleControls {
            throttle: 1.0,
            strafe: -1.0,
            lift: 0.0,
            yaw: 0.25,
            pitch: 0.0,
            roll: 0.0,
        }),
    ] {
        let mut v = input(1);
        v.intent = Intent::Vehicle(VehicleInput {
            target: key(1),
            seat: 0,
            action,
        });
        let packet = ClientPacket::Input(v);
        assert_eq!(
            wire::decode_client(&wire::encode_client(&packet).unwrap()).unwrap(),
            packet
        );
    }
}

#[test]
fn frozen_wire_v1_fixtures_preserve_discriminants_and_field_order() {
    let packet = ClientPacket::Hello(Hello {
        protocol: PROTOCOL,
        content_sha256: vec![0; 32],
        features: vec![Feature::AttackInput],
    });
    let mut expected = b"SBW\0\x01\x01\x00\x00\x02\x20".to_vec();
    expected.extend([0; 32]);
    expected.extend([1, 1]);
    assert_eq!(wire::encode_client(&packet).unwrap(), expected);
    assert_eq!(wire::decode_client(&expected).unwrap(), packet);

    let rejected = ServerPacket::Rejected(Rejection {
        sequence: Some(7),
        reason: RejectReason::StaleTarget,
    });
    let fixture = b"SBW\0\x01\x02\x01\x01\x07\x07";
    assert_eq!(wire::encode_server(&rejected).unwrap(), fixture);
    assert_eq!(wire::decode_server(fixture).unwrap(), rejected);

    let fixture = b"SBW\0\x01\x01\x01\x00\x01\x01\xf4\x03\x00\x00\x02\x00\x03\x00";
    assert_eq!(
        wire::encode_client(&ClientPacket::Input(input(1))).unwrap(),
        fixture
    );
    assert_eq!(
        wire::decode_client(fixture).unwrap(),
        ClientPacket::Input(input(1))
    );
}

#[test]
fn vehicle_deployment_carries_only_a_valid_selected_slot() {
    let mut packet = input(1);
    packet.intent = Intent::DeployVehicle(8);
    let bytes = wire::encode_client(&ClientPacket::Input(packet.clone())).unwrap();
    assert_eq!(
        wire::decode_client(&bytes).unwrap(),
        ClientPacket::Input(packet.clone())
    );
    packet.intent = Intent::DeployVehicle(9);
    assert!(wire::encode_client(&ClientPacket::Input(packet)).is_err());
}
