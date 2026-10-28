//! Linkage fixture only: no gameplay or host adapter implementation.
use sbw_contract::{
    server::exports::superbwarfare::contract::server_events::Guest,
    types::{ClientPacket, EntityKey, EntityRecord, Uuid},
};

pub struct Server;
impl Guest for Server {
    fn init() -> Result<(), String> {
        Ok(())
    }
    fn receive(_: Uuid, _: ClientPacket) {}
    fn left(_: Uuid) {}
    fn tick(_: u64) {}
    fn entity_loaded(_: EntityKey, _: EntityRecord) {}
    fn entity_unloaded(_: EntityKey) {}
}

#[cfg(target_arch = "wasm32")]
sbw_contract::export_server!(Server);
