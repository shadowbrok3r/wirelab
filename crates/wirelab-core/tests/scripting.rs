//! Wire verdicts and the per-component script host.

use wirelab_core::board::{BoardPin, BoardProfile, PinCaps, PinKind, Side};
use wirelab_core::circuit::{Circuit, CompId, Endpoint, PlacedComponent};
use wirelab_core::component::{CompState, ComponentDef, SimModel, TerminalDef, TerminalRole};
use wirelab_core::engine::{Engine, plan_setup};
use wirelab_core::library::Library;
use wirelab_core::netlist::{Netlist, WireVerdict, wire_verdict};
use wirelab_core::program::{Action, Program};
use wirelab_core::script::{ScriptHost, World, component_names, script_template};
use wirelab_proto::HostMsg;

fn mini_board() -> BoardProfile {
    let pin = |key: &str, kind, caps: PinCaps, idx| BoardPin {
        key: key.into(),
        label: key.into(),
        kind,
        side: Side::Left,
        index: idx,
        caps,
        adc: None,
        warning: None,
        tags: vec![],
    };
    BoardProfile {
        id: "test-board".into(),
        name: "Test Board".into(),
        chip: wirelab_proto::ChipKind::Esp32C3,
        description: String::new(),
        width_mm: 30.0,
        height_mm: 50.0,
        notes: vec![],
        specs: vec![],
        features: Default::default(),
        pins: vec![
            pin("GND1", PinKind::Gnd, PinCaps::empty(), 0),
            pin("GND2", PinKind::Gnd, PinCaps::empty(), 1),
            pin("3V3", PinKind::V3_3, PinCaps::empty(), 2),
            pin("5V", PinKind::V5, PinCaps::empty(), 3),
            pin(
                "GPIO2",
                PinKind::Gpio(2),
                PinCaps::DIGITAL_IN | PinCaps::DIGITAL_OUT | PinCaps::PWM,
                4,
            ),
            pin(
                "GPIO4",
                PinKind::Gpio(4),
                PinCaps::DIGITAL_IN | PinCaps::DIGITAL_OUT,
                5,
            ),
        ],
    }
}

fn mini_lib() -> Library {
    let mut lib = Library::default();
    let term = |id: &str, role| TerminalDef { id: id.into(), name: id.into(), role };
    let visual = |shape| wirelab_core::component::Visual {
        shape,
        color: [200, 200, 200],
        width_mm: 6.0,
        height_mm: 6.0,
    };
    lib.add_component(ComponentDef {
        id: "led".into(),
        name: "LED".into(),
        category: "output".into(),
        description: String::new(),
        terminals: vec![term("anode", TerminalRole::Anode), term("cathode", TerminalRole::Cathode)],
        visual: visual(wirelab_core::component::Shape::Led),
        sim: SimModel::Led { forward_mv: 2000 },
        actions: vec![],
        events: vec![],
        props: vec![],
    });
    lib.add_component(ComponentDef {
        id: "res220".into(),
        name: "220R".into(),
        category: "passive".into(),
        description: String::new(),
        terminals: vec![term("a", TerminalRole::A), term("b", TerminalRole::B)],
        visual: visual(wirelab_core::component::Shape::Resistor),
        sim: SimModel::Resistor { ohms: 220.0 },
        actions: vec![],
        events: vec![],
        props: vec![],
    });
    lib.add_component(ComponentDef {
        id: "button".into(),
        name: "Button".into(),
        category: "input".into(),
        description: String::new(),
        terminals: vec![term("a", TerminalRole::A), term("b", TerminalRole::B)],
        visual: visual(wirelab_core::component::Shape::PushButton),
        sim: SimModel::PushButton,
        actions: vec![],
        events: vec![],
        props: vec![],
    });
    lib
}

fn place(circuit: &mut Circuit, def_id: &str, label: &str, model: &SimModel) -> CompId {
    circuit.add_component(PlacedComponent {
        id: CompId(0),
        def_id: def_id.into(),
        pos: [0.0, 0.0],
        rotation: 0,
        label: label.into(),
        props: Default::default(),
        state: CompState::initial(model),
        script: None,
    })
}

fn board_ep(key: &str) -> Endpoint {
    Endpoint::BoardPin { key: key.into() }
}

fn term_ep(comp: CompId, terminal: &str) -> Endpoint {
    Endpoint::Terminal { comp, terminal: terminal.into() }
}

/// GPIO2 -> LED -> GND, GPIO4 -> button -> GND.
fn btn_led_circuit(lib: &Library) -> (Circuit, CompId, CompId) {
    let mut c = Circuit::new("test-board");
    let led = place(&mut c, "led", "led", &SimModel::Led { forward_mv: 2000 });
    let btn = place(&mut c, "button", "btn", &SimModel::PushButton);
    c.add_wire(board_ep("GPIO2"), term_ep(led, "anode"), [0; 3]);
    c.add_wire(term_ep(led, "cathode"), board_ep("GND1"), [0; 3]);
    c.add_wire(board_ep("GPIO4"), term_ep(btn, "a"), [0; 3]);
    c.add_wire(term_ep(btn, "b"), board_ep("GND2"), [0; 3]);
    let _ = lib;
    (c, led, btn)
}

#[test]
fn verdict_blocks_rail_shorts() {
    let board = mini_board();
    let lib = mini_lib();
    let circuit = Circuit::new("test-board");
    let nl = Netlist::build(&circuit, &board, &lib);
    assert!(matches!(
        wire_verdict(&nl, &board, &board_ep("GND1"), &board_ep("3V3"), &[]),
        WireVerdict::Blocked(_)
    ));
    assert!(matches!(
        wire_verdict(&nl, &board, &board_ep("3V3"), &board_ep("5V"), &[]),
        WireVerdict::Blocked(_)
    ));
    // The two ground pins are already one net.
    assert_eq!(
        wire_verdict(&nl, &board, &board_ep("GND1"), &board_ep("GND2"), &[]),
        WireVerdict::Redundant
    );
}

#[test]
fn verdict_allows_normal_wiring_and_resistive_bridges() {
    let board = mini_board();
    let lib = mini_lib();
    let mut circuit = Circuit::new("test-board");
    let res = place(&mut circuit, "res220", "", &SimModel::Resistor { ohms: 220.0 });
    circuit.add_wire(board_ep("3V3"), term_ep(res, "a"), [0; 3]);
    let nl = Netlist::build(&circuit, &board, &lib);
    // Rail to rail through a resistor is fine.
    assert_eq!(
        wire_verdict(&nl, &board, &term_ep(res, "b"), &board_ep("GND1"), &[]),
        WireVerdict::Ok
    );
    assert_eq!(
        wire_verdict(&nl, &board, &board_ep("GPIO4"), &board_ep("GND1"), &[]),
        WireVerdict::Ok
    );
}

#[test]
fn verdict_blocks_driven_output_to_rail() {
    let board = mini_board();
    let lib = mini_lib();
    let (circuit, _led, _btn) = btn_led_circuit(&lib);
    let nl = Netlist::build(&circuit, &board, &lib);
    let (_msgs, bindings) = plan_setup(&circuit, &board, &lib, &nl);
    let outs: Vec<u8> = bindings.outputs.values().map(|b| b.gpio).collect();
    assert!(outs.contains(&2));
    assert!(matches!(
        wire_verdict(&nl, &board, &board_ep("GPIO2"), &board_ep("3V3"), &outs),
        WireVerdict::Blocked(_)
    ));
    // Same wire while GPIO2 is not driven would merely be odd, not a short.
    assert_eq!(
        wire_verdict(&nl, &board, &board_ep("GPIO2"), &board_ep("3V3"), &[]),
        WireVerdict::Ok
    );
}

fn host_for(circuit: &mut Circuit, lib: &Library, comp: CompId, src: &str) -> (ScriptHost, Engine) {
    circuit.components.get_mut(&comp).unwrap().script = Some(src.into());
    let board = mini_board();
    let nl = Netlist::build(circuit, &board, lib);
    let (_msgs, bindings) = plan_setup(circuit, &board, lib, &nl);
    let mut host = ScriptHost::new();
    let fresh = host.sync(circuit, lib);
    assert!(fresh.contains(&comp), "script compiled: {:?}", host.errors);
    (host, Engine::new(Program::default(), bindings))
}

#[test]
fn script_toggles_led_on_press() {
    let lib = mini_lib();
    let (mut circuit, led, btn) = btn_led_circuit(&lib);
    let (mut host, mut engine) =
        host_for(&mut circuit, &lib, btn, "fn on_press() { led.toggle(); }");

    let actions = host.on_press(btn);
    assert_eq!(
        actions,
        vec![Action::CompAction { comp: led, action: "toggle".into(), params: Default::default() }]
    );
    let msgs = engine.run_script_actions(actions, 0);
    assert_eq!(msgs, vec![HostMsg::WriteDigital { pin: 2, high: true }]);
    let msgs = engine.run_script_actions(host.on_press(btn), 10);
    assert_eq!(msgs, vec![HostMsg::WriteDigital { pin: 2, high: false }]);
}

#[test]
fn script_state_persists_between_calls() {
    let lib = mini_lib();
    let (mut circuit, _led, btn) = btn_led_circuit(&lib);
    let (mut host, _engine) = host_for(
        &mut circuit,
        &lib,
        btn,
        "fn on_press() { this.n = (this.n ?? 0) + 1; log(this.n); }",
    );
    host.on_press(btn);
    host.on_press(btn);
    assert_eq!(host.take_logs(), vec!["[btn] 1".to_string(), "[btn] 2".to_string()]);
}

#[test]
fn after_timer_fires_when_due() {
    let lib = mini_lib();
    let (mut circuit, led, btn) = btn_led_circuit(&lib);
    let (mut host, _engine) =
        host_for(&mut circuit, &lib, btn, "fn on_press() { after(100, || led.on()); }");
    host.set_world(World { now_ms: 1000, ..Default::default() });
    assert!(host.on_press(btn).is_empty());
    assert!(host.tick(1050).is_empty());
    let fired = host.tick(1150);
    assert_eq!(
        fired,
        vec![Action::CompAction { comp: led, action: "on".into(), params: Default::default() }]
    );
    assert!(host.tick(1300).is_empty(), "timer fires once");
}

#[test]
fn script_beep_continuation_survives_program_stop() {
    let lib = mini_lib();
    let (mut circuit, _led, btn) = btn_led_circuit(&lib);
    let (mut host, mut engine) =
        host_for(&mut circuit, &lib, btn, "fn on_press() { led.beep(100); }");
    let msgs = engine.run_script_actions(host.on_press(btn), 0);
    assert_eq!(msgs, vec![HostMsg::WriteDigital { pin: 2, high: true }]);
    // Stopping the rules program must not drop the "beep off" continuation.
    engine.stop();
    assert_eq!(engine.tick(50), vec![]);
    assert_eq!(engine.tick(120), vec![HostMsg::WriteDigital { pin: 2, high: false }]);
}

#[test]
fn on_reading_gates_small_changes() {
    let lib = mini_lib();
    let (mut circuit, _led, btn) = btn_led_circuit(&lib);
    let (mut host, _engine) =
        host_for(&mut circuit, &lib, btn, "fn on_reading(mv) { log(mv); }");
    host.on_reading(btn, 1000);
    host.on_reading(btn, 1003);
    host.on_reading(btn, 1020);
    assert_eq!(host.take_logs(), vec!["[btn] 1000".to_string(), "[btn] 1020".to_string()]);
}

#[test]
fn bad_scripts_surface_errors_without_panicking() {
    let lib = mini_lib();
    let (mut circuit, _led, btn) = btn_led_circuit(&lib);
    // Runtime error: unknown component name.
    let (mut host, _engine) =
        host_for(&mut circuit, &lib, btn, "fn on_press() { nosuch.on(); }");
    assert!(host.on_press(btn).is_empty());
    assert!(host.errors.contains_key(&btn));

    // Compile error: never becomes an instance.
    circuit.components.get_mut(&btn).unwrap().script = Some("fn on_press( {".into());
    let fresh = host.sync(&circuit, &lib);
    assert!(fresh.is_empty());
    assert!(!host.has_script(btn));
    assert!(host.errors.contains_key(&btn));
}

#[test]
fn templates_compile_for_every_component() {
    let lib = mini_lib();
    let mut circuit = Circuit::new("test-board");
    let led = place(&mut circuit, "led", "lamp", &SimModel::Led { forward_mv: 2000 });
    let btn = place(&mut circuit, "button", "btn", &SimModel::PushButton);
    let names = component_names(&circuit, &lib);
    for (id, def_id) in [(led, "led"), (btn, "button")] {
        let def = lib.component(def_id).unwrap().clone();
        let peers: Vec<String> =
            names.iter().filter(|(c, _)| **c != id).map(|(_, n)| n.clone()).collect();
        let tpl = script_template(&def, &names[&id], &peers);
        circuit.components.get_mut(&id).unwrap().script = Some(tpl);
    }
    let mut host = ScriptHost::new();
    let fresh = host.sync(&circuit, &lib);
    assert_eq!(fresh.len(), 2, "template errors: {:?}", host.errors);
    assert!(host.errors.is_empty());
}

#[test]
fn auto_wire_hooks_up_led_resistor_and_button() {
    let board = mini_board();
    let lib = mini_lib();
    let mut circuit = Circuit::new("test-board");
    let led = place(&mut circuit, "led", "led", &SimModel::Led { forward_mv: 2000 });
    let res = place(&mut circuit, "res220", "", &SimModel::Resistor { ohms: 220.0 });
    let btn = place(&mut circuit, "button", "btn", &SimModel::PushButton);

    let plan = wirelab_core::autowire::auto_wire(&circuit, &board, &lib, &[led, res, btn]);
    assert_eq!(plan.wires.len(), 5, "notes: {:?}", plan.notes);
    for (a, b) in plan.wires {
        circuit.add_wire(a, b, [0; 3]);
    }

    let nl = Netlist::build(&circuit, &board, &lib);
    let (_msgs, bindings) = plan_setup(&circuit, &board, &lib, &nl);
    let led_out = bindings.outputs.get(&led).expect("led bound as output");
    assert!(led_out.active_high);
    let (gpio, b) = bindings.input_for_comp(btn).expect("button bound as input");
    assert!(b.active_low);
    assert_ne!(gpio, led_out.gpio);
    // The series resistor sits between the GPIO and the anode.
    assert!(nl.net_of(&term_ep(led, "anode")).is_some());
}

#[test]
fn auto_wire_skips_already_wired_components() {
    let board = mini_board();
    let lib = mini_lib();
    let (mut circuit, led, btn) = btn_led_circuit(&lib);
    let plan = wirelab_core::autowire::auto_wire(&circuit, &board, &lib, &[led, btn]);
    assert!(plan.wires.is_empty(), "already-wired parts must be left alone");
    assert_eq!(plan.notes.iter().filter(|n| n.contains("skipped")).count(), 2);
    let before = circuit.wires.len();
    for (a, b) in plan.wires {
        circuit.add_wire(a, b, [0; 3]);
    }
    assert_eq!(circuit.wires.len(), before);
}

#[test]
fn splice_inserts_resistor_in_series() {
    let board = mini_board();
    let lib = mini_lib();
    let mut circuit = Circuit::new("test-board");
    let led = place(&mut circuit, "led", "led", &SimModel::Led { forward_mv: 2000 });
    let direct = circuit.add_wire(board_ep("GPIO2"), term_ep(led, "anode"), [9, 9, 9]);
    circuit.add_wire(term_ep(led, "cathode"), board_ep("GND1"), [0; 3]);

    let res = place(&mut circuit, "res220", "", &SimModel::Resistor { ohms: 220.0 });
    assert!(circuit.splice_component(direct, res, "a", "b"));

    assert_eq!(circuit.wires.len(), 3, "one wire became two");
    assert!(!circuit.wires.contains_key(&direct), "original wire is gone");
    // Both halves keep the original color.
    assert_eq!(
        circuit.wires.values().filter(|w| w.color == [9, 9, 9]).count(),
        2
    );
    // Electrically: LED still binds to GPIO2, now through the resistor.
    let nl = Netlist::build(&circuit, &board, &lib);
    let (_msgs, bindings) = plan_setup(&circuit, &board, &lib, &nl);
    assert_eq!(bindings.outputs.get(&led).map(|b| b.gpio), Some(2));
    assert_ne!(
        nl.net_of(&board_ep("GPIO2")),
        nl.net_of(&term_ep(led, "anode")),
        "resistor separates the nets"
    );
}

#[test]
fn parallel_resistor_and_overcurrent_are_caught() {
    use wirelab_core::sim::{PinBank, solve};
    use wirelab_core::validate::validate;
    let board = mini_board();
    let lib = mini_lib();
    let mut circuit = Circuit::new("test-board");
    let led = place(&mut circuit, "led", "led", &SimModel::Led { forward_mv: 2000 });
    let res = place(&mut circuit, "res220", "", &SimModel::Resistor { ohms: 220.0 });
    // LED wired directly; resistor wired ACROSS it (parallel — useless).
    circuit.add_wire(board_ep("GPIO2"), term_ep(led, "anode"), [0; 3]);
    circuit.add_wire(term_ep(led, "cathode"), board_ep("GND1"), [0; 3]);
    circuit.add_wire(term_ep(res, "a"), term_ep(led, "anode"), [0; 3]);
    circuit.add_wire(term_ep(res, "b"), term_ep(led, "cathode"), [0; 3]);

    let nl = Netlist::build(&circuit, &board, &lib);
    let lints = validate(&circuit, &board, &lib, &nl);
    assert!(
        lints.iter().any(|l| l.message.contains("PARALLEL")),
        "parallel lint fires: {lints:?}"
    );
    // And the series-resistor lint still fires — parallel doesn't protect.
    assert!(lints.iter().any(|l| l.message.contains("series resistor")));

    // Live: drive the pin high; the solver reports realistic overcurrent.
    let mut bank = PinBank::default();
    bank.apply(&wirelab_proto::HostMsg::SetPinMode { pin: 2, mode: wirelab_proto::PinMode::Output });
    bank.apply(&wirelab_proto::HostMsg::WriteDigital { pin: 2, high: true });
    let out = solve(&circuit, &board, &lib, &nl, &bank);
    assert!(
        out.warnings.iter().any(|w| w.contains("mA")),
        "current warning expected: {:?}",
        out.warnings
    );
    assert!(out.source_ma.get(&2).is_some_and(|ma| *ma > 10.0));
}

#[test]
fn wire_across_own_terminals_is_flagged() {
    use wirelab_core::validate::validate;
    let board = mini_board();
    let lib = mini_lib();
    let mut circuit = Circuit::new("test-board");
    let led = place(&mut circuit, "led", "led", &SimModel::Led { forward_mv: 2000 });
    circuit.add_wire(term_ep(led, "anode"), term_ep(led, "cathode"), [0; 3]);
    let nl = Netlist::build(&circuit, &board, &lib);
    let lints = validate(&circuit, &board, &lib, &nl);
    assert!(
        lints.iter().any(|l| l.message.contains("bypass")),
        "bypass lint fires: {lints:?}"
    );
}

#[test]
fn led_lint_carries_a_working_fix() {
    use wirelab_core::validate::{LintFix, Severity, validate};
    let board = mini_board();
    let lib = mini_lib();
    let mut circuit = Circuit::new("test-board");
    let led = place(&mut circuit, "led", "led", &SimModel::Led { forward_mv: 2000 });
    circuit.add_wire(board_ep("GPIO2"), term_ep(led, "anode"), [0; 3]);
    circuit.add_wire(term_ep(led, "cathode"), board_ep("GND1"), [0; 3]);

    let nl = Netlist::build(&circuit, &board, &lib);
    let lints = validate(&circuit, &board, &lib, &nl);
    let lint = lints
        .iter()
        .find(|l| l.message.contains("series resistor"))
        .expect("resistor lint fires");
    assert_eq!(lint.severity, Severity::Warning);
    assert_eq!(lint.comps, vec![led], "highlights the LED");
    let Some(LintFix::SpliceResistor { wire, ohms, .. }) = &lint.fix else {
        panic!("fix attached: {:?}", lint.fix);
    };
    // (3.3 - 2.0) / 6 mA ≈ 217 Ω.
    assert!((200.0..240.0).contains(ohms), "computed {ohms}");

    // Applying the fix silences the lint.
    let res = place(&mut circuit, "res220", "", &SimModel::Resistor { ohms: 220.0 });
    assert!(circuit.splice_component(*wire, res, "a", "b"));
    let nl = Netlist::build(&circuit, &board, &lib);
    let lints = validate(&circuit, &board, &lib, &nl);
    assert!(
        !lints.iter().any(|l| l.message.contains("series resistor")),
        "{lints:?}"
    );
}

#[test]
fn junction_dot_merges_nets() {
    let board = mini_board();
    let mut lib = mini_lib();
    lib.add_component(ComponentDef {
        id: "dot".into(),
        name: "Junction".into(),
        category: "Routing".into(),
        description: String::new(),
        terminals: vec![TerminalDef {
            id: "a".into(),
            name: "Node".into(),
            role: TerminalRole::A,
        }],
        visual: wirelab_core::component::Visual {
            shape: wirelab_core::component::Shape::Dot,
            color: [150, 150, 170],
            width_mm: 2.4,
            height_mm: 2.4,
        },
        sim: SimModel::Generic,
        actions: vec![],
        events: vec![],
        props: vec![],
    });
    let mut circuit = Circuit::new("test-board");
    let led = place(&mut circuit, "led", "led", &SimModel::Led { forward_mv: 2000 });
    let dot = place(&mut circuit, "dot", "", &SimModel::Generic);
    circuit.add_wire(board_ep("GPIO2"), term_ep(dot, "a"), [0; 3]);
    circuit.add_wire(term_ep(dot, "a"), term_ep(led, "anode"), [0; 3]);
    circuit.add_wire(term_ep(led, "cathode"), board_ep("GND1"), [0; 3]);

    let nl = Netlist::build(&circuit, &board, &lib);
    assert_eq!(
        nl.net_of(&board_ep("GPIO2")),
        nl.net_of(&term_ep(led, "anode")),
        "routing dot must be electrically transparent"
    );
    let (_msgs, bindings) = plan_setup(&circuit, &board, &lib, &nl);
    assert!(bindings.outputs.contains_key(&led), "LED binds through the dot");
}

#[test]
fn pin_groups_cover_rails_caps_and_tags() {
    let mut board = mini_board();
    // GND1 + GND2 form the ground group.
    let (label, keys) = board.pin_group("GND1").expect("gnd group");
    assert_eq!(label, "ground");
    assert_eq!(keys, vec!["GND1".to_string(), "GND2".to_string()]);

    // Tag two pins "fspi" and give one an ADC cap: hovering GPIO2 lights both
    // fspi pins; hovering the plain-caps pin still groups via derived tags.
    for pin in &mut board.pins {
        match pin.key.as_str() {
            "GPIO2" => {
                pin.tags = vec!["fspi".into()];
                pin.caps |= PinCaps::ADC;
                pin.adc = Some((1, 1));
            }
            "GPIO4" => pin.tags = vec!["fspi".into()],
            _ => {}
        }
    }
    let (label, keys) = board.pin_group("GPIO2").expect("gpio group");
    assert!(label.contains("fspi") && label.contains("adc"), "label: {label}");
    assert!(keys.contains(&"GPIO2".to_string()) && keys.contains(&"GPIO4".to_string()));
    // A pin with no tags and no group-worthy caps has no group.
    assert!(board.pin_group("3V3").is_some());
    for pin in &mut board.pins {
        if pin.key == "GPIO4" {
            pin.tags.clear();
        }
    }
    assert!(board.pin_group("GPIO4").is_none());
}

#[test]
fn uart_script_api_round_trips() {
    let lib = mini_lib();
    let (mut circuit, _led, btn) = btn_led_circuit(&lib);
    let (mut host, mut engine) = host_for(
        &mut circuit,
        &lib,
        btn,
        "fn on_start() { uart(4, 5, 115200); uart_send(\"hi\\n\"); }\n\
         fn on_uart(line) { log(`got ${line}`); }",
    );
    let actions = host.on_start(btn);
    assert!(actions.iter().any(|a| matches!(a, Action::UartConfig { tx: 4, rx: 5, baud: 115200 })));
    let msgs = engine.run_script_actions(actions, 0);
    assert!(msgs.iter().any(|m| matches!(m, HostMsg::UartConfig { .. })));
    assert!(msgs.iter().any(|m| matches!(m, HostMsg::UartWrite { data } if data.as_slice() == b"hi\n")));
    host.on_uart(btn, "pong");
    assert!(host.take_logs().iter().any(|l| l.contains("got pong")));
}

#[test]
fn spi_i2c_script_api_round_trips() {
    let lib = mini_lib();
    let (mut circuit, _led, btn) = btn_led_circuit(&lib);
    let (mut host, mut engine) = host_for(
        &mut circuit,
        &lib,
        btn,
        "fn on_start() {\n\
             spi_setup(6, 7, 2, 1000);\n\
             spi_xfer(8, [0x9f, 0, 0]);\n\
             i2c_setup(0, 1, 400);\n\
             i2c_read(0x76, 0xd0, 1);\n\
         }\n\
         fn on_spi(data) { log(`spi ${data.len} bytes`); }\n\
         fn on_i2c(addr, data) { log(`i2c ${addr}: ${data[0]}`); }",
    );
    let actions = host.on_start(btn);
    let msgs = engine.run_script_actions(actions, 0);
    assert!(msgs.iter().any(|m| matches!(m, HostMsg::SpiConfig { sck: 6, mosi: 7, miso: 2, .. })));
    assert!(
        msgs.iter().any(|m| matches!(m, HostMsg::SpiTransfer { cs: 8, data } if data.as_slice() == [0x9f, 0, 0]))
    );
    assert!(msgs.iter().any(|m| matches!(m, HostMsg::I2cRead { addr: 0x76, reg: 0xd0, len: 1 })));
    // Replies flow back into the callbacks.
    host.on_spi(btn, &[1, 2, 3]);
    host.on_i2c(btn, 0x76, &[0x60]);
    let logs = host.take_logs();
    assert!(logs.iter().any(|l| l.contains("spi 3 bytes")), "{logs:?}");
    assert!(logs.iter().any(|l| l.contains("i2c 118: 96")), "{logs:?}");
}

#[test]
fn names_are_sanitized_and_deduped() {
    let lib = mini_lib();
    let mut circuit = Circuit::new("test-board");
    let a = place(&mut circuit, "led", "Red LED!", &SimModel::Led { forward_mv: 2000 });
    let b = place(&mut circuit, "led", "Red LED!", &SimModel::Led { forward_mv: 2000 });
    let c = place(&mut circuit, "led", "", &SimModel::Led { forward_mv: 2000 });
    let names = component_names(&circuit, &lib);
    assert_eq!(names[&a], "red_led");
    assert_eq!(names[&b], "red_led_2");
    assert_eq!(names[&c], "led");
}

// ---- flow graphs ----

use wirelab_core::flow::{self, CmpOp, FlowGraph, FlowNode, FlowWire, NodeKind};
use wirelab_core::script::FLOW_ID;

fn fnode(kind: NodeKind) -> FlowNode {
    FlowNode { kind, pos: [0.0, 0.0] }
}

fn flow_host(circuit: &mut Circuit, lib: &Library, graph: &FlowGraph) -> ScriptHost {
    let code = flow::compile(graph).expect("flow compiles");
    let mut host = ScriptHost::new();
    host.sync(circuit, lib);
    assert!(host.set_flow_script(Some(&code)), "flow script compiled: {:?}", host.errors);
    host.on_start(FLOW_ID);
    host
}

#[test]
fn flow_press_toggle_set_led() {
    let lib = mini_lib();
    let (mut circuit, led, btn) = btn_led_circuit(&lib);
    let graph = FlowGraph {
        nodes: vec![
            fnode(NodeKind::OnPress { comp: "btn".into() }),
            fnode(NodeKind::Toggle),
            fnode(NodeKind::SetComp { comp: "led".into() }),
        ],
        wires: vec![
            FlowWire { from: (0, 0), to: (1, 0) },
            FlowWire { from: (1, 0), to: (2, 0) },
        ],
    };
    let mut host = flow_host(&mut circuit, &lib, &graph);

    let on = |a: &[Action], verb: &str| {
        a.iter().any(|x| matches!(x, Action::CompAction { comp, action, .. } if *comp == led && action == verb))
    };
    let first = host.on_press(btn);
    assert!(on(&first, "on"), "first press turns the led on: {first:?}");
    let second = host.on_press(btn);
    assert!(on(&second, "off"), "second press turns it off: {second:?}");
}

#[test]
fn flow_reading_threshold_drives_level() {
    let lib = mini_lib();
    let (mut circuit, led, btn) = btn_led_circuit(&lib);
    let graph = FlowGraph {
        nodes: vec![
            fnode(NodeKind::OnReading { comp: "btn".into() }),
            fnode(NodeKind::Compare { op: CmpOp::Gt, value: 1500.0 }),
            fnode(NodeKind::SetComp { comp: "led".into() }),
        ],
        wires: vec![
            FlowWire { from: (0, 0), to: (1, 0) },
            FlowWire { from: (1, 0), to: (2, 0) },
        ],
    };
    let mut host = flow_host(&mut circuit, &lib, &graph);
    let on = |a: &[Action], verb: &str| {
        a.iter().any(|x| matches!(x, Action::CompAction { comp, action, .. } if *comp == led && action == verb))
    };
    assert!(on(&host.on_reading(btn, 2000), "on"));
    assert!(on(&host.on_reading(btn, 300), "off"));
}

#[test]
fn flow_delay_fires_via_ticks() {
    let lib = mini_lib();
    let (mut circuit, led, btn) = btn_led_circuit(&lib);
    let graph = FlowGraph {
        nodes: vec![
            fnode(NodeKind::OnPress { comp: "btn".into() }),
            fnode(NodeKind::Delay { ms: 50.0 }),
            fnode(NodeKind::Toggle),
            fnode(NodeKind::SetComp { comp: "led".into() }),
        ],
        wires: vec![
            FlowWire { from: (0, 0), to: (1, 0) },
            FlowWire { from: (1, 0), to: (2, 0) },
            FlowWire { from: (2, 0), to: (3, 0) },
        ],
    };
    let mut host = flow_host(&mut circuit, &lib, &graph);
    host.tick(0);
    let press = host.on_press(btn);
    assert!(press.is_empty(), "nothing happens until the delay expires: {press:?}");
    let early = host.tick(20);
    assert!(early.is_empty(), "still pending: {early:?}");
    let fired = host.tick(80);
    assert!(
        fired.iter().any(|x| matches!(x, Action::CompAction { comp, action, .. } if *comp == led && action == "on")),
        "delayed pulse toggles the led: {fired:?}"
    );
}

#[test]
fn flow_compile_rejects_bad_graphs() {
    // Unpicked component.
    let g = FlowGraph { nodes: vec![fnode(NodeKind::OnPress { comp: String::new() })], wires: vec![] };
    assert!(flow::compile(&g).is_err());

    // Type mismatch: pulse into a bool input.
    let g = FlowGraph {
        nodes: vec![fnode(NodeKind::OnPress { comp: "btn".into() }), fnode(NodeKind::Not)],
        wires: vec![FlowWire { from: (0, 0), to: (1, 0) }],
    };
    let errs = flow::compile(&g).unwrap_err();
    assert!(errs.iter().any(|e| e.msg.contains("type mismatch")), "{errs:?}");

    // A cycle.
    let g = FlowGraph {
        nodes: vec![fnode(NodeKind::Not), fnode(NodeKind::Not)],
        wires: vec![
            FlowWire { from: (0, 0), to: (1, 0) },
            FlowWire { from: (1, 0), to: (0, 0) },
        ],
    };
    let errs = flow::compile(&g).unwrap_err();
    assert!(errs.iter().any(|e| e.msg.contains("cycle")), "{errs:?}");
}

#[test]
fn flow_survives_sync_and_clears() {
    let lib = mini_lib();
    let (mut circuit, _led, btn) = btn_led_circuit(&lib);
    let graph = FlowGraph {
        nodes: vec![
            fnode(NodeKind::OnPress { comp: "btn".into() }),
            fnode(NodeKind::Toggle),
            fnode(NodeKind::SetComp { comp: "led".into() }),
        ],
        wires: vec![
            FlowWire { from: (0, 0), to: (1, 0) },
            FlowWire { from: (1, 0), to: (2, 0) },
        ],
    };
    let mut host = flow_host(&mut circuit, &lib, &graph);
    // A circuit re-sync (no scripted components) must keep the flow alive.
    host.sync(&circuit, &lib);
    assert!(!host.on_press(btn).is_empty());
    // Clearing removes it.
    host.set_flow_script(None);
    assert!(host.on_press(btn).is_empty());
}

#[test]
fn flow_state_snapshot_exposes_node_values() {
    let lib = mini_lib();
    let (mut circuit, _led, btn) = btn_led_circuit(&lib);
    let graph = FlowGraph {
        nodes: vec![
            fnode(NodeKind::OnPress { comp: "btn".into() }),
            fnode(NodeKind::Toggle),
            fnode(NodeKind::SetComp { comp: "led".into() }),
        ],
        wires: vec![
            FlowWire { from: (0, 0), to: (1, 0) },
            FlowWire { from: (1, 0), to: (2, 0) },
        ],
    };
    let mut host = flow_host(&mut circuit, &lib, &graph);
    let get = |host: &ScriptHost, key: &str| {
        host.flow_state().into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
    };
    // on_start initialized the toggle's cached output.
    assert_eq!(get(&host, "n1_0").as_deref(), Some("off"));
    host.on_press(btn);
    assert_eq!(get(&host, "n1_0").as_deref(), Some("on"));
}

#[test]
fn board_messaging_emits_and_receives() {
    let lib = mini_lib();
    let (mut circuit, _led, btn) = btn_led_circuit(&lib);
    let (mut host, _engine) = host_for(
        &mut circuit,
        &lib,
        btn,
        "fn on_press() { send_board(\"garage\", \"open\"); }\n\
         fn on_board_msg(from, text) { log(`${from} says ${text}`); }",
    );

    // send_board queues a host-routed action, never a device command.
    let actions = host.on_press(btn);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::BoardMsg { to, text } if to == "garage" && text == "open"
        )),
        "{actions:?}"
    );

    // And the callback receives cross-board mail with the sender's name.
    host.on_board_msg(btn, "house", "hello");
    let logs = host.take_logs();
    assert!(logs.iter().any(|l| l.contains("house says hello")), "{logs:?}");
}

#[test]
fn flow_board_msg_gate_pattern_toggles_on_matching_text() {
    let lib = mini_lib();
    let (mut circuit, led, _btn) = btn_led_circuit(&lib);
    // board msg → gate(pulse), text → "= open" → gate enable; gate → toggle led.
    let graph = FlowGraph {
        nodes: vec![
            fnode(NodeKind::OnBoardMsg { from_board: String::new() }),
            fnode(NodeKind::TextEquals { value: "open".into() }),
            fnode(NodeKind::Gate),
            fnode(NodeKind::ToggleComp { comp: "led".into() }),
        ],
        wires: vec![
            FlowWire { from: (0, 0), to: (2, 0) }, // recv pulse → gate in
            FlowWire { from: (0, 1), to: (1, 0) }, // text → equals
            FlowWire { from: (1, 0), to: (2, 1) }, // equals → gate enable
            FlowWire { from: (2, 0), to: (3, 0) }, // gate → toggle
        ],
    };
    let mut host = flow_host(&mut circuit, &lib, &graph);
    let toggles = |a: &[Action]| {
        a.iter().any(|x| matches!(
            x,
            Action::CompAction { comp, action, .. } if *comp == led && action == "toggle"
        ))
    };
    // Wrong text: the gate stays shut.
    let no = host.on_board_msg(FLOW_ID, "house", "close");
    assert!(!toggles(&no), "{no:?}");
    // Matching text toggles the LED.
    let yes = host.on_board_msg(FLOW_ID, "house", "open");
    assert!(toggles(&yes), "{yes:?}");
}

#[test]
fn flow_board_msg_from_filter_and_send_board() {
    let lib = mini_lib();
    let (mut circuit, _led, btn) = btn_led_circuit(&lib);
    let graph = FlowGraph {
        nodes: vec![
            fnode(NodeKind::OnBoardMsg { from_board: "garage".into() }),
            fnode(NodeKind::Log { label: "gate".into() }),
            fnode(NodeKind::OnPress { comp: "btn".into() }),
            fnode(NodeKind::SendBoard { board: "garage".into(), text: "open".into() }),
        ],
        wires: vec![
            FlowWire { from: (0, 0), to: (1, 0) },
            FlowWire { from: (2, 0), to: (3, 0) },
        ],
    };
    let mut host = flow_host(&mut circuit, &lib, &graph);
    // The from-filter drops other senders…
    host.on_board_msg(FLOW_ID, "porch", "x");
    assert!(host.take_logs().is_empty());
    // …and passes the named one.
    host.on_board_msg(FLOW_ID, "garage", "x");
    assert!(host.take_logs().iter().any(|l| l.contains("gate")));
    // A press emits the routable BoardMsg action.
    let actions = host.on_press(btn);
    assert!(
        actions.iter().any(|a| matches!(
            a,
            Action::BoardMsg { to, text } if to == "garage" && text == "open"
        )),
        "{actions:?}"
    );
}
