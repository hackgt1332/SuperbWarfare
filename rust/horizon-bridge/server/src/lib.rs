//! Authenticated server adapter. No client-supplied position, damage or ammo
//! is accepted. Inventory changes and projectile creation commit in one host edit.
use horizon_mod_sdk::{
    net,
    server::{Registration, ServerMod, gameplay},
};
use sbw_contract::{
    session::{InputGate, ServerPolicy},
    types::*,
    wire,
};
use sbw_sim::{Controls, Weapon, gun};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
#[path = "../../shared.rs"]
mod shared;
struct Peer {
    token: Uuid,
    gate: InputGate,
    controls: Controls,
    target: Option<WeaponRef>,
    weapon: Option<WeaponState>,
    had_entities: bool,
    equip_revision: u64,
}
struct Server {
    policy: ServerPolicy,
    peers: BTreeMap<String, Peer>,
    pending: VecDeque<net::Message>,
    bytes: usize,
    dirty: BTreeSet<String>,
    serial: u64,
    cursor: String,
    simulation_cursor: usize,
    request_cursor: String,
}
fn send(player: &str, packet: ServerPacket) -> Result<(), String> {
    net::send(
        Some(player),
        &wire::encode_server(&packet).map_err(|e| e.to_string())?,
    )
}
fn reject(player: &str, sequence: Option<u64>, reason: RejectReason) {
    let _ = send(
        player,
        ServerPacket::Rejected(Rejection { sequence, reason }),
    );
}
impl Server {
    fn receive(&mut self, player: &str, bytes: &[u8], tick: u64, received_session: net::Session) {
        let Some(token) = net::session_token(player) else {
            return;
        };
        if token.high != received_session.high || token.low != received_session.low {
            return;
        }
        let token = Uuid {
            high: token.high,
            low: token.low,
        };
        let Ok(packet) = wire::decode_client(bytes) else {
            reject(player, None, RejectReason::InvalidInput);
            return;
        };
        match packet {
            ClientPacket::Hello(hello) => {
                if self.peers.get(player).is_some_and(|p| p.token == token) {
                    reject(player, None, RejectReason::NotReady);
                    return;
                }
                match self.policy.negotiate(&hello, token, tick) {
                    Ok(welcome) => {
                        if send(player, ServerPacket::Welcome(welcome)).is_ok()
                            && let Ok(gate) = InputGate::new(token)
                        {
                            self.peers.insert(
                                player.into(),
                                Peer {
                                    token,
                                    gate,
                                    controls: Controls::default(),
                                    target: None,
                                    weapon: None,
                                    had_entities: false,
                                    equip_revision: 0,
                                },
                            );
                        }
                    }
                    Err(reason) => reject(player, None, reason),
                }
            }
            ClientPacket::Input(input) => {
                let Some(peer) = self.peers.get_mut(player) else {
                    reject(player, Some(input.sequence), RejectReason::NotReady);
                    return;
                };
                if input.session != token {
                    reject(player, Some(input.sequence), RejectReason::WrongSession);
                    return;
                }
                if let Err(reason) = peer.gate.accept(&input, tick) {
                    reject(player, Some(input.sequence), reason);
                    return;
                }
                self.dirty.insert(player.into());
                match input.intent {
                    Intent::Weapon(w) => {
                        if peer.weapon.as_ref().is_none_or(|s| s.target != w.target) {
                            reject(player, Some(input.sequence), RejectReason::StaleTarget);
                            return;
                        }
                        peer.controls.action(w.action, tick);
                    }
                    Intent::DeployVehicle(slot) => {
                        let inventories = gameplay::inventories();
                        let stack = inventories
                            .iter()
                            .find(|p| p.player == player)
                            .and_then(|p| {
                                p.stacks.iter().find(|s| {
                                    s.selected
                                        && s.slot == u32::from(slot)
                                        && sbw_sim::VEHICLES.contains(&s.item.as_str())
                                })
                            });
                        let Some(s) = stack else {
                            reject(player, Some(input.sequence), RejectReason::StaleTarget);
                            return;
                        };
                        let _ = gameplay::commit(&gameplay::Edit {
                            player: player.into(),
                            slot: s.slot,
                            item: s.item.clone(),
                            expected: s.state.clone(),
                            state: vec![],
                            ammo: String::new(),
                            consume: 0,
                            shot: None,
                            vehicle: sbw_sim::vehicle_profile(&s.item).map(String::from),
                        });
                    }
                    // Native passenger/input packets drive vehicles. The engine
                    // verifies the driver and ignores client MoveVehicle poses.
                    Intent::Vehicle(_) => {
                        reject(player, Some(input.sequence), RejectReason::NotAuthorized)
                    }
                }
            }
        }
    }
    fn simulate(&mut self, tick: u64) {
        let mut inventories = gameplay::inventories();
        if !inventories.is_empty() {
            let start = self.simulation_cursor % inventories.len();
            inventories.rotate_left(start);
            self.simulation_cursor = (start + 1) % inventories.len();
        }
        for inventory in inventories {
            let Some(peer) = self.peers.get_mut(&inventory.player) else {
                continue;
            };
            let current = net::session_token(&inventory.player);
            if current.is_none_or(|t| t.high != peer.token.high || t.low != peer.token.low) {
                peer.controls.stop();
                peer.weapon = None;
                peer.target = None;
                continue;
            }
            let selected = inventory.stacks.iter().find(|s| s.selected && s.count == 1);
            let Some((s, g)) = selected.and_then(|s| gun(&s.item).map(|g| (s, g))) else {
                if peer.weapon.is_some() {
                    self.dirty.insert(inventory.player.clone());
                }
                peer.weapon = None;
                peer.target = None;
                peer.controls.stop();
                continue;
            };
            let reserve = inventory
                .stacks
                .iter()
                .filter(|a| a.item == g.ammo)
                .map(|a| a.count)
                .sum();
            let state = Weapon::decode(&s.state, &g);
            let Some(state) = state else {
                // Never replace corrupt or newer data with a fresh magazine.
                if s.state.is_empty() {
                    self.serial += 1;
                    let initial = Weapon::new(Uuid {
                        high: peer.token.high,
                        low: peer.token.low ^ self.serial,
                    });
                    let _ = gameplay::commit(&gameplay::Edit {
                        player: inventory.player.clone(),
                        slot: s.slot,
                        item: s.item.clone(),
                        expected: vec![],
                        state: initial.encode(),
                        ammo: g.ammo.into(),
                        consume: 0,
                        shot: None,
                        vehicle: None,
                    });
                }
                peer.weapon = None;
                peer.target = None;
                peer.controls.stop();
                continue;
            };
            let identity = state.target(s.slot as u8, peer.equip_revision);
            let changed = peer
                .target
                .is_none_or(|old| old.instance != identity.instance || old.slot != identity.slot);
            if changed {
                let Some(epoch) = peer.equip_revision.checked_add(1) else {
                    peer.controls.stop();
                    peer.weapon = None;
                    continue;
                };
                peer.equip_revision = epoch;
            }
            let target = state.target(s.slot as u8, peer.equip_revision);
            if peer.target.as_ref() != Some(&target) {
                peer.controls.stop();
                peer.target = Some(target);
            }
            let mut controls = peer.controls.clone();
            if let Some(step) = sbw_sim::step(&g, &state, &mut controls, reserve, tick) {
                let accepted = gameplay::commit(&gameplay::Edit {
                    player: inventory.player.clone(),
                    slot: s.slot,
                    item: s.item.clone(),
                    expected: s.state.clone(),
                    state: step.next.encode(),
                    ammo: g.ammo.into(),
                    consume: step.consume,
                    shot: step.shot.then_some(gameplay::Projectile {
                        speed: g.speed,
                        damage: g.damage,
                        inaccuracy: if controls.aiming() { 0.3 } else { 1.5 },
                    }),
                    vehicle: None,
                })
                .is_ok();
                if accepted {
                    peer.controls = controls;
                }
            } else {
                peer.controls = controls;
            }
            peer.weapon = Some(state.snapshot(
                s.slot as u8,
                peer.equip_revision,
                &g,
                reserve,
                &peer.controls,
                tick,
            ));
            self.dirty.insert(inventory.player);
        }
    }
    fn snapshots(&mut self, tick: u64) {
        let vehicles = gameplay::vehicles();
        let dimensions: BTreeMap<_, _> = horizon_mod_sdk::server::players::online()
            .into_iter()
            .map(|p| (p.id, p.dimension))
            .collect();
        let tracked = |dimension: &str| {
            let mut budget = 10_000usize;
            vehicles
                .iter()
                .filter(|v| v.dimension == dimension)
                .take(8)
                .filter_map(|v| {
                    let state = v.profile.as_bytes().to_vec();
                    if state.len() > 4096 || state.len() > budget {
                        return None;
                    }
                    budget -= state.len();
                    let half = f64::from(v.yaw).to_radians() / 2.;
                    Some(EntityState {
                        key: EntityKey {
                            index: u64::from(v.entity),
                            generation: 1,
                        },
                        revision: tick.max(1),
                        entity: EntityRecord {
                            instance: Uuid {
                                high: 0,
                                low: u64::from(v.entity),
                            },
                            definition: v.definition.clone(),
                            dimension: v.dimension.clone(),
                            pose: Transform {
                                position: Vec3 {
                                    x: v.x,
                                    y: v.y,
                                    z: v.z,
                                },
                                orientation: Rotation {
                                    x: 0.,
                                    y: half.sin() as f32,
                                    z: 0.,
                                    w: half.cos() as f32,
                                },
                                velocity: Vec3 {
                                    x: v.vx,
                                    y: v.vy,
                                    z: v.vz,
                                },
                            },
                            state_schema: 1,
                            state,
                        },
                    })
                })
                .collect::<Vec<_>>()
        };
        // Snapshot all negotiated peers, including observers not holding a gun.
        if tick.is_multiple_of(2) {
            self.dirty.extend(
                self.peers
                    .iter()
                    .filter(|(_, p)| p.weapon.is_some() || p.had_entities || !vehicles.is_empty())
                    .map(|(id, _)| id.clone()),
            );
        }
        let mut recipients: Vec<_> = self.dirty.iter().cloned().collect();
        let split = recipients.partition_point(|p| p <= &self.cursor);
        recipients.rotate_left(split);
        for player in recipients.into_iter().take(4) {
            self.cursor = player.clone();
            self.dirty.remove(&player);
            if let Some(peer) = self.peers.get_mut(&player) {
                let dimension = dimensions.get(&player).map(String::as_str).unwrap_or("");
                let entities = tracked(dimension);
                peer.had_entities = !entities.is_empty();
                let snapshot = Snapshot {
                    session: peer.token,
                    server_tick: tick,
                    acknowledged_input: peer.gate.last_sequence(),
                    weapons: peer.weapon.iter().cloned().collect(),
                    entities,
                };
                if let Ok(pages) = wire::snapshot_pages(&snapshot) {
                    for page in pages {
                        let _ = send(&player, ServerPacket::Snapshot(page));
                    }
                }
            }
        }
    }
}
impl ServerMod for Server {
    fn init() -> Result<(Self, Registration), String> {
        horizon_mod_sdk::info(shared::MOD_ID);
        Ok((
            Self {
                policy: ServerPolicy::new(shared::CONTENT_SHA256, sbw_sim::FEATURES.to_vec())
                    .map_err(|e| e.to_string())?,
                peers: BTreeMap::new(),
                pending: VecDeque::new(),
                bytes: 0,
                dirty: BTreeSet::new(),
                serial: 0,
                cursor: String::new(),
                simulation_cursor: 0,
                request_cursor: String::new(),
            },
            Registration::new().on_tick().on_joins(),
        ))
    }
    fn tick(&mut self, tick: u64) {
        for event in net::poll() {
            if let net::Event::Received(message) = event
                && self.pending.len() < 64
                && self.bytes + message.data.len() <= 256 * 1024
                && message.player.is_some()
                && self
                    .pending
                    .iter()
                    .filter(|m| m.player == message.player)
                    .count()
                    < 8
            {
                self.bytes += message.data.len();
                self.pending.push_back(message);
            }
        }
        // Read current identity before consuming input: swapping items cannot
        // direct a delayed click at a newly selected weapon.
        self.simulate(tick);
        for _ in 0..4 {
            let ids: BTreeSet<_> = self
                .pending
                .iter()
                .filter_map(|m| m.player.clone())
                .collect();
            let player = ids
                .iter()
                .find(|id| **id > self.request_cursor)
                .or_else(|| ids.first())
                .cloned();
            let Some(player) = player else {
                break;
            };
            self.request_cursor = player.clone();
            let index = self
                .pending
                .iter()
                .position(|m| m.player.as_ref() == Some(&player))
                .expect("queued sender");
            let message = self.pending.remove(index).expect("queued message");
            self.bytes -= message.data.len();
            if let (Some(player), Some(session)) = (message.player, message.session) {
                self.receive(&player, &message.data, tick, session);
            }
        }
        self.snapshots(tick);
    }
    fn player_left(&mut self, player: &horizon_mod_sdk::server::Player) {
        self.peers.remove(&player.id);
        self.dirty.remove(&player.id);
        self.pending
            .retain(|m| m.player.as_deref() != Some(&player.id));
        self.bytes = self.pending.iter().map(|m| m.data.len()).sum();
    }
}
horizon_mod_sdk::export_server_mod!(Server);
