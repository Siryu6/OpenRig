use lilv::{plugin::Plugin, World};

fn main() {
    let world = World::new();
    world.load_all();

    let show_names = false;

    let print = |plugin: Plugin| {
        if show_names {
            String::from(plugin.name().as_str().unwrap())
        } else {
            String::from(plugin.uri().as_uri().unwrap())
        }
    };

    let plugins = world
        .plugins()
        .iter()
        .filter(Plugin::verify)
        .map(print)
        .collect::<Vec<_>>();

    debug_assert_eq!(world.plugins().count(), plugins.len());

    for uri in plugins {
        println!("{}", uri);
    }
}
