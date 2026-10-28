use std::{collections::BTreeSet, path::Path};
use wit_parser::{Resolve, WorldItem};

#[test]
fn client_world_has_no_server_mutation_capabilities() {
    let mut resolve = Resolve::default();
    let (package, _) = resolve
        .push_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("wit"))
        .unwrap();
    let package = &resolve.packages[package];
    let client = &resolve.worlds[package.worlds["client"]];
    let server = &resolve.worlds[package.worlds["server"]];
    fn interface_names<'a>(
        resolve: &Resolve,
        items: impl Iterator<Item = &'a WorldItem>,
    ) -> BTreeSet<String> {
        items
            .filter_map(|item| match item {
                WorldItem::Interface { id, .. } => resolve.interfaces[*id].name.clone(),
                _ => None,
            })
            .collect()
    }
    assert!(interface_names(&resolve, client.imports.values()).contains("client-host"));
    assert!(!interface_names(&resolve, client.imports.values()).contains("server-host"));
    assert!(interface_names(&resolve, server.imports.values()).contains("server-host"));
    assert!(!interface_names(&resolve, server.imports.values()).contains("client-host"));
    assert!(interface_names(&resolve, client.exports.values()).contains("client-events"));
    assert!(interface_names(&resolve, server.exports.values()).contains("server-events"));

    let host = &resolve.interfaces[package.interfaces["server-host"]];
    for name in [
        "commit-inventory",
        "spawn",
        "despawn",
        "update-entity",
        "damage",
        "sweep-world",
    ] {
        assert!(host.functions.contains_key(name), "{name}");
    }
    let events = &resolve.interfaces[package.interfaces["server-events"]];
    let receive = &events.functions["receive"];
    assert_eq!(receive.params[0].name, "player");
    assert_eq!(receive.params[1].name, "packet");
}
