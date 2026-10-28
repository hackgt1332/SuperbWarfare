//! Transport adapter only. It never predicts or applies weapon damage.
use horizon_mod_sdk::{Mod, Registration, net};
use sbw_contract::{
    PROTOCOL,
    session::{EffectGate, SnapshotAssembler, verify_welcome},
    types::*,
    wire,
};
#[path = "../../shared.rs"]
mod shared;

struct Client {
    attempted: bool,
    welcome: Option<Welcome>,
    snapshots: Option<SnapshotAssembler>,
    effects: Option<EffectGate>,
    sequence: u64,
    acknowledged: Option<u64>,
    tick: u64,
}
impl Client {
    fn hello() -> Hello {
        Hello {
            protocol: PROTOCOL,
            content_sha256: shared::CONTENT_SHA256.to_vec(),
            features: vec![Feature::ModChannel],
        }
    }
    fn reset(&mut self) {
        self.attempted = false;
        self.welcome = None;
        self.snapshots = None;
        self.effects = None;
        self.sequence = 0;
        self.acknowledged = None;
    }
    fn receive(&mut self, bytes: &[u8]) -> Result<(), String> {
        match wire::decode_server(bytes).map_err(|e| e.to_string())? {
            ServerPacket::Welcome(welcome) => {
                if self.welcome.is_some() {
                    return Err("duplicate welcome".into());
                }
                verify_welcome(&Self::hello(), &welcome).map_err(|e| format!("{e:?}"))?;
                self.snapshots =
                    Some(SnapshotAssembler::new(welcome.session).map_err(|e| e.to_string())?);
                self.effects = Some(EffectGate::new(welcome.session).map_err(|e| e.to_string())?);
                self.welcome = Some(welcome);
            }
            ServerPacket::Snapshot(page) => {
                let assembler = self.snapshots.as_mut().ok_or("snapshot before welcome")?;
                if let Some(snapshot) = assembler.push(page).map_err(|e| format!("{e:?}"))? {
                    self.acknowledged = snapshot.acknowledged_input;
                }
            }
            ServerPacket::Effects(batch) => self
                .effects
                .as_mut()
                .ok_or("effects before welcome")?
                .accept(&batch)
                .map_err(|e| format!("{e:?}"))?,
            ServerPacket::Rejected(rejection) => {
                if rejection.sequence.is_none() {
                    self.reset();
                    self.attempted = true;
                }
                horizon_mod_sdk::info(&format!(
                    "{}: request rejected: {:?}",
                    shared::MOD_ID,
                    rejection.reason
                ));
            }
        }
        Ok(())
    }
}
impl Mod for Client {
    fn init() -> Result<(Self, Registration), String> {
        Ok((
            Self {
                attempted: false,
                welcome: None,
                snapshots: None,
                effects: None,
                sequence: 0,
                acknowledged: None,
                tick: 0,
            },
            Registration::new()
                .command("sbw_status")
                .command("sbw_probe")
                .on_tick(),
        ))
    }
    fn tick(&mut self, tick: u64) {
        self.tick = tick;
        for event in net::poll() {
            match event {
                net::Event::Disconnected => self.reset(),
                net::Event::Received(message) => {
                    if let Err(error) = self.receive(&message.data) {
                        horizon_mod_sdk::warn(&format!("SuperbWarfare: {error}"));
                    }
                }
            }
        }
        if net::connected()
            && !self.attempted
            && let Ok(packet) = wire::encode_client(&ClientPacket::Hello(Self::hello()))
            && net::send(None, &packet).is_ok()
        {
            self.attempted = true;
        }
    }
    fn command(&mut self, name: &str, _: &str) -> Result<Option<String>, String> {
        if name == "sbw_status" {
            return Ok(Some(if self.welcome.is_some() {
                format!("Connected; acknowledged input: {:?}", self.acknowledged)
            } else if self.attempted {
                "Waiting for a compatible server".into()
            } else {
                "Disconnected".into()
            }));
        }
        let welcome = self
            .welcome
            .as_ref()
            .ok_or("Not connected to a compatible server")?;
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or("Reconnect to reset input sequence")?;
        // Explicit diagnostic command: no real weapon is selected or fired.
        let packet = ClientPacket::Input(Input {
            session: welcome.session,
            sequence,
            client_tick: self.tick,
            intent: Intent::Weapon(WeaponInput {
                target: WeaponRef {
                    instance: Uuid { high: 0, low: 1 },
                    slot: 0,
                    equip_revision: 0,
                },
                action: WeaponAction::TriggerRelease,
            }),
        });
        net::send(
            None,
            &wire::encode_client(&packet).map_err(|e| e.to_string())?,
        )?;
        self.sequence = sequence;
        Ok(Some("Probe sent".into()))
    }
}
horizon_mod_sdk::export_mod!(Client);
