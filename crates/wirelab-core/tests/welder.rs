//! The CD welder example's power stage and its exported documents.

use std::path::PathBuf;

use wirelab_core::circuit::{Circuit, CompId};
use wirelab_core::component::{CompState, VisualState};
use wirelab_core::export::{DocCtx, kicad, nets};
use wirelab_core::library::Library;
use wirelab_core::netlist::Netlist;
use wirelab_core::sim::{PinBank, PinDrive, PowerState, step};
use wirelab_proto::PinMode;

fn setup() -> (Library, Circuit) {
    let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    let lib = Library::load(&assets.join("boards"), &assets.join("components")).expect("assets");
    let project = wirelab_core::project::Project::load(&assets.join("examples/21-cd-pulse-arc-welder.wirelab.json"))
        .expect("example");
    (lib, project.circuit)
}

fn comp(c: &Circuit, label: &str) -> CompId {
    c.components.values().find(|x| x.label == label).map(|x| x.id).expect(label)
}

fn bank(relay: bool) -> PinBank {
    let mut b = PinBank::default();
    *b.get_mut(19).unwrap() = PinDrive { mode: PinMode::Output, out_high: relay, ..PinDrive::default() };
    *b.get_mut(1).unwrap() = PinDrive { mode: PinMode::Analog, ..PinDrive::default() };
    b
}

#[test]
fn bank_bleeds_with_the_302k_time_constant_and_reads_2_over_302() {
    let (lib, c) = setup();
    let board = lib.board(&c.board_id).unwrap();
    let nl = Netlist::build(&c, board, &lib);
    let mut st = PowerState::default();
    for l in ["C1", "C2"] {
        st.set_cap_volts(comp(&c, l), 300.0);
    }
    let out = step(&c, board, &lib, &nl, &bank(false), &mut st, 0.001);
    let mv = f64::from(out.analog_mv[&1]);
    assert!((mv - 300_000.0 * 2.0 / 302.0).abs() < 3.0, "ADC sees {mv} mV");
    for _ in 0..5 {
        step(&c, board, &lib, &nl, &bank(false), &mut st, 0.2);
    }
    let v = st.cap_volts(comp(&c, "C1")).unwrap();
    let expect = 300.0 * (-1.001f64 / (302e3 * 190e-6)).exp();
    assert!((v - expect).abs() < 0.3, "after 1 s: {v} V, expected {expect} V");
}

#[test]
fn relay_charges_the_bank_to_the_charger_set_point() {
    let (lib, c) = setup();
    let board = lib.board(&c.board_id).unwrap();
    let nl = Netlist::build(&c, board, &lib);
    let mut st = PowerState::default();
    let mut t = 0.0;
    while t < 1.0 {
        step(&c, board, &lib, &nl, &bank(true), &mut st, 0.01);
        t += 0.01;
    }
    let v = st.cap_volts(comp(&c, "C1")).unwrap();
    assert!((v - 300.0).abs() < 1.0, "charger regulates at its pot setting: {v}");
    let out = step(&c, board, &lib, &nl, &bank(false), &mut st, 0.001);
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
}

#[test]
fn touching_dumps_the_bank_electrode_negative() {
    let (lib, mut c) = setup();
    let board = lib.board(&c.board_id).unwrap();
    let nl = Netlist::build(&c, board, &lib);
    let pen = comp(&c, "PEN");
    c.components.get_mut(&pen).unwrap().state = CompState::Button { pressed: true };
    let mut st = PowerState::default();
    for l in ["C1", "C2"] {
        st.set_cap_volts(comp(&c, l), 300.0);
    }
    let out = step(&c, board, &lib, &nl, &bank(false), &mut st, 0.004);
    let Some(VisualState::Arc { pulse: Some(p), .. }) = out.visuals.get(&pen).copied() else {
        panic!("no pulse: {:?}", out.visuals.get(&pen));
    };
    assert!(p.dcen, "electrode on the negative side");
    assert!(p.peak_a > 500.0 && p.width_us < 1000.0, "{p:?}");
    assert!(st.cap_volts(comp(&c, "C1")).unwrap() < 20.0);
}

#[test]
fn documents_cover_every_part_and_the_build_matches_the_netlist() {
    let (lib, c) = setup();
    let board = lib.board(&c.board_id).unwrap();
    let ctx = DocCtx::new("CD pulse-arc welder", &c, board, &lib);
    let sch = kicad::schematic(&ctx);
    for r in ["PS1", "K1", "U1", "U2", "C1", "C2", "R1", "R2", "R3", "R4", "R5", "D1", "PEN", "CLAMP"] {
        assert!(sch.contains(&format!("(reference \"{r}\")")), "{r} missing from the schematic");
    }
    assert!(!sch.contains("(reference \"fw\")"), "script controller left out");
    let ops = nets::operating_points(&ctx);
    let md = nets::markdown(&ctx, &ops);
    assert!(md.contains("57.4 s") && md.contains("12 AWG"), "tables carry tau and gauges");
    let perf = c.perf.as_ref().expect("perf layout");
    let ready = ops.ready.as_ref().map(|(_, o)| o.net_avg_mv.as_slice());
    let rep = wirelab_core::perf::check(perf, &ctx, ready);
    assert!(rep.shorts.is_empty(), "{:?}", rep.shorts);
    assert!(rep.missing.iter().all(|m| m.contains("not on the build yet")), "{:?}", rep.missing);
    assert!(rep.clearance.iter().any(|m| m.contains("+ rail")), "{:?}", rep.clearance);
}
