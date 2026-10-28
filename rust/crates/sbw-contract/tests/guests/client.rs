//! Linkage fixture only: no gameplay or host adapter implementation.
use sbw_contract::{
    client::exports::superbwarfare::contract::client_events::Guest,
    types::{LocalInput, ServerPacket},
};

pub struct Client;
impl Guest for Client {
    fn init() -> Result<(), String> {
        Ok(())
    }
    fn input(_: LocalInput) {}
    fn receive(_: ServerPacket) {}
    fn tick(_: u64) {}
    fn disconnected() {}
}

#[cfg(target_arch = "wasm32")]
sbw_contract::export_client!(Client);
