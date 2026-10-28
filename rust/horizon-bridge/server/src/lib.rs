//! Negotiated, authenticated transport only. All gameplay remains NotReady.
use horizon_mod_sdk::{
    net,
    server::{Registration, ServerMod},
};
use sbw_contract::{
    session::{InputGate, ServerPolicy},
    types::*,
    wire,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
#[path = "../../shared.rs"]
mod shared;

struct Peer {
    token: Uuid,
    gate: InputGate,
}
struct Server {
    policy: ServerPolicy,
    peers: BTreeMap<String, Peer>,
    pending: VecDeque<net::Message>,
    bytes: usize,
    dirty: BTreeSet<String>,
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
                            self.peers.insert(player.into(), Peer { token, gate });
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
                match peer.gate.accept(&input, tick) {
                    Ok(()) => {
                        // Acknowledged means processed, never that damage was applied.
                        reject(player, Some(input.sequence), RejectReason::NotReady);
                        self.dirty.insert(player.into());
                    }
                    Err(reason) => reject(player, Some(input.sequence), reason),
                }
            }
        }
    }
}
impl ServerMod for Server {
    fn init() -> Result<(Self, Registration), String> {
        Ok((
            Self {
                policy: ServerPolicy::new(shared::CONTENT_SHA256, vec![Feature::ModChannel])
                    .map_err(|e| e.to_string())?,
                peers: BTreeMap::new(),
                pending: VecDeque::new(),
                bytes: 0,
                dirty: BTreeSet::new(),
            },
            Registration::new().on_tick().on_joins(),
        ))
    }
    fn tick(&mut self, tick: u64) {
        for event in net::poll() {
            if let net::Event::Received(message) = event {
                if self.pending.len() < 64 && self.bytes + message.data.len() <= 256 * 1024 {
                    self.bytes += message.data.len();
                    self.pending.push_back(message);
                } else {
                    horizon_mod_sdk::warn(&format!("{}: transport queue full", shared::MOD_ID));
                }
            }
        }
        // Each request emits at most two packets; stay inside the host's 8/call cap.
        for _ in 0..4 {
            let Some(message) = self.pending.pop_front() else {
                break;
            };
            self.bytes -= message.data.len();
            if let (Some(player), Some(session)) = (message.player, message.session) {
                self.receive(&player, &message.data, tick, session);
            }
        }
        // At most one replacement snapshot per recipient/tick, even for a burst.
        for player in std::mem::take(&mut self.dirty) {
            if let Some(peer) = self.peers.get(&player) {
                let snapshot = Snapshot {
                    session: peer.token,
                    server_tick: tick,
                    acknowledged_input: peer.gate.last_sequence(),
                    weapons: vec![],
                    entities: vec![],
                };
                if let Ok(pages) = wire::snapshot_pages(&snapshot) {
                    for page in pages {
                        let _ = send(&player, ServerPacket::Snapshot(page));
                    }
                }
            }
        }
    }
    fn player_left(&mut self, player: &horizon_mod_sdk::server::Player) {
        self.peers.remove(&player.id);
        self.pending
            .retain(|m| m.player.as_deref() != Some(&player.id));
        self.bytes = self.pending.iter().map(|m| m.data.len()).sum();
    }
}
horizon_mod_sdk::export_server_mod!(Server);
