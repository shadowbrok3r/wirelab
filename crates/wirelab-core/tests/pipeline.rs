//! End-to-end core pipeline: wiring -> netlist -> bindings -> sim -> engine.

use wirelab_core::board::{BoardPin, BoardProfile, PinCaps, PinKind, Side};
use wirelab_core::circuit::{Circuit, CompId, Endpoint, PlacedComponent};
use wirelab_core::component::{
    CompState, ComponentDef, SimModel, TerminalDef, TerminalRole, VisualState,
};
use wirelab_core::engine::{Engine, plan_setup};
use wirelab_core::library::Library;
use wirelab_core::netlist::Netlist;
use wirelab_core::program::{Action, Program, Rule, Trigger};
use wirelab_core::sim::{PinBank, solve};
use wirelab_proto::{DeviceMsg, EventEdge, HostMsg, PinMode};

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
            pin("3V3", PinKind::V3_3, PinCaps::empty(), 1),
            pin(
                "GPIO2",
                PinKind::Gpio(2),
                PinCaps::DIGITAL_IN | PinCaps::DIGITAL_OUT | PinCaps::PWM,
                2,
            ),
            pin(
                "GPIO4",
                PinKind::Gpio(4),
                PinCaps::DIGITAL_IN | PinCaps::DIGITAL_OUT,
                3,
            ),
            pin("GPIO5", PinKind::Gpio(5), PinCaps::DIGITAL_IN | PinCaps::ADC, 4),
        ],
    }
}

fn mini_lib() -> Library {
    let mut lib = Library::default();
    let term = |id: &str, role| TerminalDef { id: id.into(), name: id.into(), role };
    lib.add_component(ComponentDef {
        id: "led".into(),
        name: "LED".into(),
        category: "output".into(),
        description: String::new(),
        terminals: vec![term("anode", TerminalRole::Anode), term("cathode", TerminalRole::Cathode)],
        visual: wirelab_core::component::Visual {
            shape: wirelab_core::component::Shape::Led,
            color: [255, 40, 40],
            width_mm: 5.0,
            height_mm: 5.0,
        },
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
        visual: wirelab_core::component::Visual {
            shape: wirelab_core::component::Shape::Resistor,
            color: [200, 180, 120],
            width_mm: 8.0,
            height_mm: 3.0,
        },
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
        visual: wirelab_core::component::Visual {
            shape: wirelab_core::component::Shape::PushButton,
            color: [60, 60, 60],
            width_mm: 6.0,
            height_mm: 6.0,
        },
        sim: SimModel::PushButton,
        actions: vec![],
        events: vec![],
        props: vec![],
    });
    lib.add_component(ComponentDef {
        id: "pot".into(),
        name: "Pot".into(),
        category: "input".into(),
        description: String::new(),
        terminals: vec![
            term("enda", TerminalRole::EndA),
            term("endb", TerminalRole::EndB),
            term("wiper", TerminalRole::Wiper),
        ],
        visual: wirelab_core::component::Visual {
            shape: wirelab_core::component::Shape::Potentiometer,
            color: [40, 80, 200],
            width_mm: 10.0,
            height_mm: 10.0,
        },
        sim: SimModel::Potentiometer { ohms: 10_000.0 },
        actions: vec![],
        events: vec![],
        props: vec![],
    });
    lib
}

fn place(circuit: &mut Circuit, def_id: &str, model: &SimModel) -> CompId {
    circuit.add_component(PlacedComponent {
        id: CompId(0),
        def_id: def_id.into(),
        pos: [0.0, 0.0],
        rotation: 0,
        label: String::new(),
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

/// GPIO2 -> 220R -> LED -> GND, button between GPIO4 and GND.
fn button_led_circuit(lib: &Library) -> (Circuit, CompId, CompId) {
    let mut c = Circuit::new("test-board");
    let led = place(&mut c, "led", &SimModel::Led { forward_mv: 2000 });
    let res = place(&mut c, "res220", &SimModel::Resistor { ohms: 220.0 });
    let btn = place(&mut c, "button", &SimModel::PushButton);
    let _ = lib;
    c.add_wire(board_ep("GPIO2"), term_ep(res, "a"), [255, 0, 0]);
    c.add_wire(term_ep(res, "b"), term_ep(led, "anode"), [255, 0, 0]);
    c.add_wire(term_ep(led, "cathode"), board_ep("GND1"), [0, 0, 0]);
    c.add_wire(board_ep("GPIO4"), term_ep(btn, "a"), [0, 200, 0]);
    c.add_wire(term_ep(btn, "b"), board_ep("GND1"), [0, 0, 0]);
    (c, led, btn)
}

#[test]
fn bindings_derive_from_wiring() {
    let board = mini_board();
    let lib = mini_lib();
    let (circuit, led, btn) = button_led_circuit(&lib);
    let nl = Netlist::build(&circuit, &board, &lib);
    let (msgs, bindings) = plan_setup(&circuit, &board, &lib, &nl);

    let out = bindings.outputs.get(&led).expect("LED bound");
    assert_eq!(out.gpio, 2);
    assert!(out.active_high);

    let (gpio, input) = bindings.input_for_comp(btn).expect("button bound");
    assert_eq!(gpio, 4);
    assert!(input.active_low);

    assert!(msgs.contains(&HostMsg::SetPinMode { pin: 2, mode: PinMode::Output }));
    assert!(msgs.contains(&HostMsg::SetPinMode { pin: 4, mode: PinMode::InputPullUp }));
    assert!(bindings.warnings.is_empty(), "unexpected warnings: {:?}", bindings.warnings);
}

#[test]
fn sim_lights_led_and_reads_button() {
    let board = mini_board();
    let lib = mini_lib();
    let (mut circuit, led, btn) = button_led_circuit(&lib);
    let nl = Netlist::build(&circuit, &board, &lib);

    let mut bank = PinBank::default();
    for msg in plan_setup(&circuit, &board, &lib, &nl).0 {
        bank.apply(&msg);
    }

    // LED off, button released: pull-up wins.
    let out = solve(&circuit, &board, &lib, &nl, &bank);
    match out.visuals[&led] {
        VisualState::LedBrightness(b) => assert!(b < 0.05, "expected dark LED, got {b}"),
        ref v => panic!("unexpected visual {v:?}"),
    }
    assert_eq!(out.digital.get(&4), Some(&true));

    // Drive the LED pin high.
    bank.apply(&HostMsg::WriteDigital { pin: 2, high: true });
    let out = solve(&circuit, &board, &lib, &nl, &bank);
    match out.visuals[&led] {
        VisualState::LedBrightness(b) => assert!(b > 0.5, "expected lit LED, got {b}"),
        ref v => panic!("unexpected visual {v:?}"),
    }

    // Press the button: net pulled hard to GND.
    circuit.components.get_mut(&btn).unwrap().state = CompState::Button { pressed: true };
    let out = solve(&circuit, &board, &lib, &nl, &bank);
    assert_eq!(out.digital.get(&4), Some(&false));
}

#[test]
fn pot_divider_reads_midscale() {
    let board = mini_board();
    let lib = mini_lib();
    let mut circuit = Circuit::new("test-board");
    let pot = place(&mut circuit, "pot", &SimModel::Potentiometer { ohms: 10_000.0 });
    circuit.add_wire(term_ep(pot, "enda"), board_ep("3V3"), [255, 0, 0]);
    circuit.add_wire(term_ep(pot, "endb"), board_ep("GND1"), [0, 0, 0]);
    circuit.add_wire(term_ep(pot, "wiper"), board_ep("GPIO5"), [0, 0, 255]);
    let nl = Netlist::build(&circuit, &board, &lib);
    let (msgs, bindings) = plan_setup(&circuit, &board, &lib, &nl);
    assert_eq!(bindings.analog.get(&pot), Some(&5));
    assert!(msgs.contains(&HostMsg::SetPinMode { pin: 5, mode: PinMode::Analog }));

    let mut bank = PinBank::default();
    for msg in &msgs {
        bank.apply(msg);
    }
    let out = solve(&circuit, &board, &lib, &nl, &bank);
    let mv = *out.analog_mv.get(&5).expect("adc reading");
    assert!((1500..=1800).contains(&mv), "expected ~1650mV, got {mv}");

    circuit.components.get_mut(&pot).unwrap().state = CompState::Fraction { value: 1.0 };
    let out = solve(&circuit, &board, &lib, &nl, &bank);
    let mv = *out.analog_mv.get(&5).expect("adc reading");
    assert!(!(200..=3100).contains(&mv), "wiper at an end should read a rail, got {mv}");
}

#[test]
fn engine_toggles_led_on_button_press() {
    let board = mini_board();
    let lib = mini_lib();
    let (circuit, led, btn) = button_led_circuit(&lib);
    let nl = Netlist::build(&circuit, &board, &lib);
    let (_msgs, bindings) = plan_setup(&circuit, &board, &lib, &nl);

    let program = Program {
        rules: vec![Rule {
            name: "toggle on press".into(),
            enabled: true,
            trigger: Trigger::CompEvent { comp: btn, event: "pressed".into() },
            actions: vec![Action::CompAction {
                comp: led,
                action: "toggle".into(),
                params: Default::default(),
            }],
        }],
    };
    let mut engine = Engine::new(program, bindings);
    let mut cmds = engine.start(0);

    // Pressed = falling edge on an active-low button.
    let press = DeviceMsg::Event { millis: 10, pin: 4, edge: EventEdge::Falling };
    cmds.extend(engine.handle_device(10, &press));
    assert!(cmds.contains(&HostMsg::WriteDigital { pin: 2, high: true }), "cmds: {cmds:?}");

    let release = DeviceMsg::Event { millis: 20, pin: 4, edge: EventEdge::Rising };
    let cmds = engine.handle_device(20, &release);
    assert!(cmds.is_empty(), "release should not fire: {cmds:?}");

    let press2 = DeviceMsg::Event { millis: 30, pin: 4, edge: EventEdge::Falling };
    let cmds = engine.handle_device(30, &press2);
    assert!(cmds.contains(&HostMsg::WriteDigital { pin: 2, high: false }), "cmds: {cmds:?}");
}

#[test]
fn wait_actions_resume_on_tick() {
    let board = mini_board();
    let lib = mini_lib();
    let (circuit, led, _btn) = button_led_circuit(&lib);
    let nl = Netlist::build(&circuit, &board, &lib);
    let (_msgs, bindings) = plan_setup(&circuit, &board, &lib, &nl);

    let program = Program {
        rules: vec![Rule {
            name: "pulse at start".into(),
            enabled: true,
            trigger: Trigger::OnStart,
            actions: vec![
                Action::CompAction { comp: led, action: "on".into(), params: Default::default() },
                Action::Wait { ms: 100 },
                Action::CompAction { comp: led, action: "off".into(), params: Default::default() },
            ],
        }],
    };
    let mut engine = Engine::new(program, bindings);
    let cmds = engine.start(0);
    assert!(cmds.contains(&HostMsg::WriteDigital { pin: 2, high: true }));
    assert!(!cmds.contains(&HostMsg::WriteDigital { pin: 2, high: false }));

    assert!(engine.tick(50).is_empty());
    let cmds = engine.tick(120);
    assert!(cmds.contains(&HostMsg::WriteDigital { pin: 2, high: false }));
}
