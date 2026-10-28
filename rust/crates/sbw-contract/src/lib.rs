//! Proposed component ABI and multiplayer protocol for the Horizon port.
//!
//! WIT is the source of truth for data types; see `wit/contract.wit` and
//! the workspace README for authority, limits and host obligations.

// Generated bindings necessarily contain ABI glue with unsafe code. Handwritten
// contract code is kept safe; the generated modules are the only exceptions.
#[allow(unsafe_code)]
mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "schema",
        additional_derives: [PartialEq, serde::Serialize, serde::Deserialize],
        generate_unused_types: true,
        generate_all,
    });
}

pub use bindings::superbwarfare::contract::types;

#[cfg(feature = "client-bindings")]
#[allow(unsafe_code)]
pub mod client {
    wit_bindgen::generate!({
        path: "wit",
        world: "client",
        pub_export_macro: true,
        export_macro_name: "export_client",
        default_bindings_module: "sbw_contract::client",
        with: { "superbwarfare:contract/types@0.3.0": crate::types },
        generate_all,
    });
}

#[cfg(feature = "server-bindings")]
#[allow(unsafe_code)]
pub mod server {
    wit_bindgen::generate!({
        path: "wit",
        world: "server",
        pub_export_macro: true,
        export_macro_name: "export_server",
        default_bindings_module: "sbw_contract::server",
        with: { "superbwarfare:contract/types@0.3.0": crate::types },
        generate_all,
    });
}

#[cfg(feature = "client-bindings")]
pub use client::export_client;
#[cfg(feature = "server-bindings")]
pub use server::export_server;

pub mod limits;
pub mod session;
pub mod validate;
pub mod wire;

pub const PROTOCOL: types::Version = types::Version { major: 0, minor: 2 };
pub const TICK_RATE: u16 = 20;

/// Minimum host support for the first playable firearm, before optional vehicles.
pub const FIREARM_FEATURES: [types::Feature; 8] = [
    types::Feature::ItemState,
    types::Feature::AttackInput,
    types::Feature::ModChannel,
    types::Feature::Entities,
    types::Feature::Collision,
    types::Feature::Damage,
    types::Feature::WorldModels,
    types::Feature::WorldPersistence,
];
