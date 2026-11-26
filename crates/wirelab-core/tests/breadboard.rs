//! Breadboard builds: seated leads, bussed strips, and the welder built on an
//! ElectroCookie half-size board matching its schematic net for net.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use wirelab_core::breadboard::seating;
use wirelab_core::circuit::{BbId, Circuit, CompId, Endpoint, PlacedBreadboard};
use wirelab_core::library::Library;
use wirelab_core::netlist::Netlist;
use wirelab_core::sim::{PinBank, PinDrive, PowerState, step};
use wirelab_proto::PinMode;

fn assets() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets")
}

fn lib() -> Library {
    let a = assets();
    Library::load(&a.join("boards"), &a.join("components")).expect("assets")
}

fn example(name: &str) -> Circuit {
    wirelab_core::project::Project::load(&assets().join("examples").join(name)).expect(name).circuit
}

fn label_of(c: &Circuit, ep: &Endpoint) -> Option<String> {
    match ep {
        Endpoint::Terminal { comp, terminal } => Some(format!("{}.{terminal}", c.components[comp].label)),
        Endpoint::BoardPin { key } => Some(key.clone()),
        Endpoint::Hole { .. } => None,
    }
}

/// Nets joining two or more part leads or board pins, holes dropped.
fn partition(c: &Circuit, lib: &Library) -> BTreeSet<BTreeSet<String>> {
    let board = lib.board(&c.board_id).unwrap();
    Netlist::build(c, board, lib)
        .nets
        .iter()
        .map(|n| n.endpoints.iter().filter_map(|e| label_of(c, e)).collect::<BTreeSet<_>>())
        .filter(|s| s.len() > 1)
        .collect()
}

#[test]
fn welder_on_electrocookie_matches_the_schematic() {
    let lib = lib();
    let schematic = example("21-cd-pulse-arc-welder.wirelab.json");
    let build = example("22-cd-welder-electrocookie.wirelab.json");
    assert_eq!(partition(&build, &lib), partition(&schematic, &lib));
}

#[test]
fn welder_leads_sit_in_the_build_holes() {
    let lib = lib();
    let c = example("22-cd-welder-electrocookie.wirelab.json");
    let s = seating(&c, lib.board(&c.board_id), &lib);
    assert!(s.misses.is_empty(), "leads missing holes: {:?}", s.misses);
    let got: BTreeMap<String, String> =
        s.seats.iter().filter_map(|x| Some((label_of(&c, &x.ep)?, x.hole_name.clone()))).collect();
    let want = [
        ("5V", "H5"),
        ("GND1", "H6"),
        ("3V3", "H7"),
        ("GPIO1", "H9"),
        ("GPIO5", "H13"),
        ("GPIO16", "C5"),
        ("GPIO19", "C11"),
        ("GPIO9", "C13"),
        ("R1.a", "D4"),
        ("R1.b", "D8"),
        ("R2.a", "E8"),
        ("R2.b", "E12"),
        ("R3.a", "D12"),
        ("R3.b", "D16"),
        ("R4.a", "E16"),
        ("R4.b", "E20"),
        ("C1.pos", "C23"),
        ("C1.neg", "C25"),
        ("C2.pos", "C28"),
        ("C2.neg", "C30"),
        ("R5.a", "I15"),
        ("R5.b", "I9"),
        ("D1.anode", "J9"),
        ("D1.cathode", "J7"),
    ];
    for (lead, hole) in want {
        assert_eq!(got.get(lead).map(String::as_str), Some(hole), "{lead}");
    }
    let leads = s.seats.iter().filter(|x| matches!(x.ep, Endpoint::Terminal { .. })).count();
    let pins = s.seats.iter().filter(|x| matches!(x.ep, Endpoint::BoardPin { .. })).count();
    assert_eq!((leads, pins), (16, 18));
    let board = lib.board(&c.board_id).unwrap();
    let nl = Netlist::build(&c, board, &lib);
    let lints = wirelab_core::validate::validate(&c, board, &lib, &nl);
    assert!(!lints.iter().any(|l| l.message.contains("unwired")), "{:?}", lints.iter().map(|l| &l.message).collect::<Vec<_>>());
    let (_msgs, bindings) = wirelab_core::engine::plan_setup(&c, board, &lib, &nl);
    assert!(bindings.warnings.is_empty(), "{:?}", bindings.warnings);
    let esp = BbId(2);
    for x in &s.seats {
        let on_esp = matches!(&x.ep, Endpoint::BoardPin { .. })
            || matches!(label_of(&c, &x.ep).as_deref(), Some(l) if l.starts_with("R5.") || l.starts_with("D1."));
        assert_eq!(x.bb == esp, on_esp, "{:?} in {}", x.ep, x.hole_name);
    }
}

#[test]
fn welder_controls_land_in_the_spec_holes() {
    let c = example("22-cd-welder-electrocookie.wirelab.json");
    let (hv, esp) = (BbId(1), BbId(2));
    let landing = |lead: &str| -> Vec<(BbId, String)> {
        c.wires
            .values()
            .filter_map(|w| match (label_of(&c, &w.a).as_deref() == Some(lead), &w.a, &w.b) {
                (true, _, Endpoint::Hole { bb, hole }) => Some((*bb, hole.clone())),
                (false, Endpoint::Hole { bb, hole }, _) if label_of(&c, &w.b).as_deref() == Some(lead) => {
                    Some((*bb, hole.clone()))
                }
                _ => None,
            })
            .collect()
    };
    let want = [
        ("K1.signal", esp, "A11"),
        ("K1.vcc", esp, "J5"),
        ("K1.gnd", esp, "I6"),
        ("K2.signal", esp, "A12"),
        ("K2.vcc", esp, "I5"),
        ("K2.gnd", hv, "-A13"),
        ("K2.common", hv, "+A27"),
        ("R6.b", hv, "-A27"),
        ("S2.a", esp, "A10"),
        ("S2.b", hv, "-A14"),
        ("R8.a", esp, "J11"),
    ];
    for (lead, bb, hole) in want {
        assert_eq!(landing(lead), vec![(bb, hole.to_string())], "{lead}");
    }
    assert_eq!(c.display_rotation, 180, "the picture reads upright with the board USB-up");
}

#[test]
fn welder_build_charges_reads_the_tap_and_dumps() {
    let lib = lib();
    let mut c = example("22-cd-welder-electrocookie.wirelab.json");
    let id = |c: &Circuit, label: &str| c.components.values().find(|x| x.label == label).unwrap().id;
    let s1 = id(&c, "S1");
    c.components.get_mut(&s1).unwrap().state = wirelab_core::component::CompState::Toggle { on: true };
    let board = lib.board(&c.board_id).unwrap();
    let nl = Netlist::build(&c, board, &lib);
    // K1 (GPIO19) closed and K2 (GPIO18) energized to hold R6 off the bank.
    let mut bank = PinBank::default();
    for gpio in [18, 19] {
        *bank.get_mut(gpio).unwrap() = PinDrive { mode: PinMode::Output, out_high: true, ..PinDrive::default() };
    }
    for gpio in [1, 3] {
        *bank.get_mut(gpio).unwrap() = PinDrive { mode: PinMode::Analog, ..PinDrive::default() };
    }
    let mut st = PowerState::default();
    for _ in 0..100 {
        step(&c, board, &lib, &nl, &bank, &mut st, 0.01);
    }
    let c1 = id(&c, "C1");
    let v = st.cap_volts(c1).unwrap();
    assert!((v - 300.0).abs() < 1.0, "bank reached {v} V");
    bank.get_mut(19).unwrap().out_high = false;
    let out = step(&c, board, &lib, &nl, &bank, &mut st, 0.001);
    let mv = f64::from(out.analog_mv[&1]);
    assert!((mv - v * 1000.0 * 2.0 / 302.0).abs() < 5.0, "ADC sees {mv} mV at {v} V");
    let arm = f64::from(out.analog_mv[&3]);
    assert!((arm - 12_000.0 * 10.0 / 57.0).abs() < 20.0, "ARM sense in J11 reads {arm} mV");
    // K2 released: R6 between +A27 and -A27 drains the bank under 5 V within a second.
    bank.get_mut(18).unwrap().out_high = false;
    for _ in 0..100 {
        step(&c, board, &lib, &nl, &bank, &mut st, 0.01);
    }
    let v = st.cap_volts(c1).unwrap();
    assert!(v < 5.0, "bank still at {v} V one second after K2 released");
}

#[test]
fn strips_join_leads_and_neighbouring_strips_stay_apart() {
    let lib = lib();
    let mut c = Circuit::new("esp32-c3-devkitm-1");
    let bb = c.add_breadboard(PlacedBreadboard {
        id: BbId(0),
        profile: "electrocookie-mini".into(),
        pos: [0.0, 0.0],
        rotation: 0,
        label: String::new(),
    });
    let e5 = wirelab_core::breadboard::hole_endpoint_world(&c, &lib, bb, "E5").unwrap();
    let h5 = wirelab_core::breadboard::hole_endpoint_world(&c, &lib, bb, "H5").unwrap();
    let place = |c: &mut Circuit, def: &str, at: [f32; 2]| -> CompId {
        c.add_component(wirelab_core::circuit::PlacedComponent {
            id: CompId(0),
            def_id: def.into(),
            pos: at,
            rotation: 0,
            label: String::new(),
            props: Default::default(),
            state: wirelab_core::component::CompState::None,
            script: None,
        })
    };
    // A 1k from E5 across the channel to G5; an LED laid along row 5 in H5-I5.
    let r = place(&mut c, "resistor-1k", [e5[0] + 2.0 * 2.54, e5[1]]);
    let led = place(&mut c, "led-red", [h5[0] + 1.27, h5[1]]);
    let board = lib.board(&c.board_id).unwrap();
    let s = seating(&c, Some(board), &lib);
    let hole_of = |comp: CompId, t: &str| {
        s.seat_of(&Endpoint::Terminal { comp, terminal: t.into() }).map(|x| x.hole_name.clone())
    };
    assert_eq!(hole_of(r, "a").as_deref(), Some("E5"));
    assert_eq!(hole_of(r, "b").as_deref(), Some("G5"));
    assert_eq!(hole_of(led, "anode").as_deref(), Some("H5"));
    assert_eq!(hole_of(led, "cathode").as_deref(), Some("I5"));
    let nl = Netlist::build(&c, board, &lib);
    let net = |comp: CompId, t: &str| nl.net_of(&Endpoint::Terminal { comp, terminal: t.into() }).unwrap();
    assert_ne!(net(r, "a"), net(r, "b"), "the channel separates A-E from F-J");
    assert_eq!(net(r, "b"), net(led, "anode"), "G5 and H5 share the F-J strip");
    assert_eq!(net(led, "anode"), net(led, "cathode"), "an LED laid along one strip is shorted");
}

#[test]
fn c6_lcd_straddles_the_channel_and_its_pins_join_their_strips() {
    let lib = lib();
    let mut c = Circuit::new("esp32-c6-lcd-147");
    let bb = c.add_breadboard(PlacedBreadboard {
        id: BbId(0),
        profile: "electrocookie-half".into(),
        pos: [0.0, 0.0],
        rotation: 90,
        label: String::new(),
    });
    let hole = |c: &Circuit, name: &str| wirelab_core::breadboard::hole_endpoint_world(c, &lib, bb, name).unwrap();
    let board = lib.board(&c.board_id).unwrap();
    assert_eq!(board.pins.len(), 18);
    // GPIO5, the left pin farthest from the USB-C, goes in C10.
    let c10 = hole(&c, "C10");
    c.board_pos = [c10[0] - (board.width_mm / 2.0 - 8.89), c10[1] - (board.height_mm - 11.31 - 8.0 * 2.54)];
    // A 1k standing along line E from E12 to E16.
    let (e12, e16) = (hole(&c, "E12"), hole(&c, "E16"));
    let r = c.add_component(wirelab_core::circuit::PlacedComponent {
        id: CompId(0),
        def_id: "resistor-1k".into(),
        pos: [(e12[0] + e16[0]) / 2.0, (e12[1] + e16[1]) / 2.0],
        rotation: 90,
        label: String::new(),
        props: Default::default(),
        state: wirelab_core::component::CompState::None,
        script: None,
    });
    let s = seating(&c, Some(board), &lib);
    assert!(s.misses.is_empty(), "pins missing holes: {:?}", s.misses);
    let pin_hole = |key: &str| {
        s.seat_of(&Endpoint::BoardPin { key: key.into() }).map(|x| x.hole_name.clone()).unwrap_or_default()
    };
    let want = [
        ("GPIO5", "C10"),
        ("GPIO0", "C15"),
        ("3V3", "C16"),
        ("GND1", "C17"),
        ("5V", "C18"),
        ("GPIO9", "H10"),
        ("GPIO18", "H11"),
        ("GPIO23", "H14"),
        ("GPIO16", "H18"),
    ];
    for (key, h) in want {
        assert_eq!(pin_hole(key), h, "{key}");
    }
    assert_eq!(s.seats.iter().filter(|x| matches!(x.ep, Endpoint::BoardPin { .. })).count(), 18);
    let nl = Netlist::build(&c, board, &lib);
    let pin = |key: &str| nl.net_of(&Endpoint::BoardPin { key: key.into() }).unwrap();
    let lead = |t: &str| nl.net_of(&Endpoint::Terminal { comp: r, terminal: t.into() }).unwrap();
    assert_eq!(lead("a"), pin("GPIO3"), "E12 shares strip 12 with GPIO3 in C12");
    assert_eq!(lead("b"), pin("3V3"), "E16 shares strip 16 with 3V3 in C16");
    assert_ne!(pin("GPIO3"), pin("GPIO2"));
    assert_ne!(pin("GPIO5"), pin("GPIO9"), "the channel keeps the two headers apart");
}
