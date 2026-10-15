use lilv::{node::Node, plugin::Plugin, port::FloatRanges, World};

struct Nodes {
    control_class: Node,
    event_class: Node,
    group_pred: Node,
    label_pred: Node,
    preset_class: Node,
    designation_pred: Node,
    supports_event_pred: Node,
}

fn print_port(p: &Plugin, index: usize, port_ranges: &FloatRanges, nodes: &Nodes) {
    let port = p.port_by_index(index);

    println!("\n\tPort {}:", index);

    if port.is_none() {
        println!("\t\tERROR: Illegal/nonexistent port");
        return;
    }

    let port = port.unwrap();

    print!("\t\tType:        ");

    for (i, value) in port.classes().iter().enumerate() {
        if i != 0 {
            print!("\n\t\t             ");
        }
        print!("{}", value.as_uri().unwrap());
    }

    if port.is_a(&nodes.event_class) {
        let supported = port.value(&nodes.supports_event_pred);
        if supported.count() > 0 {
            println!("\n\t\tSupported events:\n");
            for value in supported {
                println!("\t\t\t{}", value.as_uri().unwrap());
            }
        }
    }

    let points = port.scale_points();
    println!("\n\t\tScale Points:");
    for point in points {
        println!(
            "\t\t\t{} = \"{}\"",
            point.value().as_str().unwrap(),
            point.label().as_str().unwrap(),
        );
    }

    println!(
        "\n\t\tSymbol:      {}",
        port.symbol().unwrap().as_str().unwrap(),
    );

    println!(
        "\t\tName:        {}",
        port.name().unwrap().as_str().unwrap(),
    );

    let groups = port.value(&nodes.group_pred);
    if let Some(group) = groups.iter().next() {
        println!("\t\tGroup:       {}", group.as_str().unwrap(),);
    }

    let designations = port.value(&nodes.designation_pred);
    if let Some(designation) = designations.iter().next() {
        println!("\t\tDesignation: {}", designation.as_str().unwrap(),);
    }

    if port.is_a(&nodes.control_class) {
        let (min, max, def) = (port_ranges.min, port_ranges.max, port_ranges.default);

        if !min.is_nan() {
            println!("\t\tMinimum:     {}", min);
        }

        if !max.is_nan() {
            println!("\t\tMaximum:     {}", max);
        }

        if !def.is_nan() {
            println!("\t\tDefault:     {}", def);
        }

        let properties = port.properties();
        for (i, property) in properties.iter().enumerate() {
            if i != 0 {
                print!("\t\t             ");
            }
            println!("{}", property.as_uri().unwrap());
        }
        println!();
    }
}

#[allow(clippy::too_many_lines)]
fn print_plugin(world: &World, p: &Plugin, nodes: &Nodes) {
    println!("{}\n", p.uri().as_uri().unwrap());
    println!("\tName:              {}", p.name().as_str().unwrap());
    println!(
        "\tClass:             {}",
        p.class().label().as_str().unwrap()
    );

    if let Some(val) = p.author_name() {
        println!("\tAuthor:            {}", val.as_str().unwrap());
    }

    if let Some(val) = p.author_email() {
        println!("\tAuthor Email:      {}", val.as_str().unwrap());
    }

    if let Some(val) = p.author_homepage() {
        println!("\tAuthor Homepage:   {}", val.as_uri().unwrap());
    }

    if let Some(latency_port) = p.latency_port_index() {
        println!(
            "\tHas latency:       yes, reported by port {}",
            latency_port
        );
    } else {
        println!("\tHas latency:       no");
    }

    println!("\tBundle:            {}", p.bundle_uri().as_uri().unwrap());
    println!(
        "\tBinary:            {}",
        p.library_uri().map_or("<none>".to_string(), |node| node
            .as_uri()
            .unwrap()
            .to_string())
    );

    if let Some(uis) = p.uis() {
        println!("\tUIs:");

        for ui in uis {
            println!("\t\t{}", ui.uri().as_uri().unwrap());

            for tyep in ui.classes() {
                println!("\t\t\tClass:  {}", tyep.as_uri().unwrap());
            }

            println!(
                "\t\t\tBinary: {}",
                ui.binary_uri().unwrap().as_uri().unwrap()
            );
            println!(
                "\t\t\tBundle: {}",
                ui.bundle_uri().unwrap().as_uri().unwrap()
            );
        }
    }

    print!("\tData URIs:         ");

    for (i, uri) in p.data_uris().iter().enumerate() {
        if i != 0 {
            print!("\n\t                   ");
        }

        print!("{}", uri.as_uri().unwrap());
    }

    println!();

    let features = p.required_features();
    print!("\tRequired Features: ");

    for (i, feature) in features.iter().enumerate() {
        if i != 0 {
            print!("\n\t                   ");
        }
        print!("{}", feature.as_uri().unwrap());
    }
    println!();

    let features = p.optional_features();
    print!("\tOptional Features: ");

    for (i, feature) in features.iter().enumerate() {
        if i != 0 {
            print!("\n\t                   ");
        }
        print!("{}", feature.as_uri().unwrap());
    }
    println!();

    if let Some(data) = p.extension_data() {
        print!("\tExtension Data:    ");

        for (i, d) in data.iter().enumerate() {
            if i != 0 {
                print!("\n\t                   ");
            }
            print!("{}", d.as_uri().unwrap());
        }
        println!();
    }

    if let Some(presets) = p.related(Some(&nodes.preset_class)) {
        if presets.count() != 0 {
            println!("\tPresets: ");

            for preset in presets {
                world.load_resource(&preset).unwrap();

                let titles = world.find_nodes(Some(&preset), &nodes.label_pred, None);
                if titles.count() > 0 {
                    if let Some(title) = titles.iter().next() {
                        println!("\t         {}", title.as_str().unwrap());
                    } else {
                        println!("\t         <{}>", preset.as_uri().unwrap());
                    }
                } else {
                    println!("\t         <{}>", preset.as_uri().unwrap());
                }
            }
        }
    }

    let num_ports = p.ports_count();
    let port_ranges = p.port_ranges_float();
    assert_eq!(num_ports, port_ranges.len());
    for (i, pr) in port_ranges.iter().enumerate() {
        print_port(p, i, pr, nodes);
    }
}

fn main() {
    let w = World::new();
    w.load_all();

    let nodes = Nodes {
        control_class: w.new_uri("http://lv2plug.in/ns/lv2core#ControlPort"),
        event_class: w.new_uri("http://lv2plug.in/ns/ext/atom#AtomPort"),
        group_pred: w.new_uri("http://lv2plug.in/ns/ext/port-groups#group"),
        label_pred: w.new_uri("http://www.w3.org/2000/01/rdf-schema#label"),
        preset_class: w.new_uri("http://lv2plug.in/ns/ext/presets#Preset"),
        designation_pred: w.new_uri("http://lv2plug.in/ns/lv2core#designation"),
        supports_event_pred: w.new_uri("http://lv2plug.in/ns/ext/atom#supportsEvent"),
    };

    for p in w.plugins().iter().filter(Plugin::verify) {
        print_plugin(&w, &p, &nodes);
    }
}
