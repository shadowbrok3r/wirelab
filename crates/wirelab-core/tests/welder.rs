//! The CD welder example's power stage, its rev B controls (ARM switch, dump relay, DUMP button),
//! the firmware model and the exported documents.

use std::collections::HashMap;
use std::path::PathBuf;

use wirelab_core::circuit::{Circuit, CompId};
use wirelab_core::component::{CompState, VisualState};
use wirelab_core::export::{DocCtx, kicad, nets};
use wirelab_core::library::Library;
use wirelab_core::netlist::Netlist;
use wirelab_core::program::Action;
use wirelab_core::script::{ScriptHost, World};
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

/// The circuit with the ARM switch S1 thrown on or off.
fn armed(mut c: Circuit, on: bool) -> Circuit {
    let s1 = comp(&c, "S1");
    c.components.get_mut(&s1).unwrap().state = CompState::Toggle { on };
    c
}

/// K1 (GPIO19) and K2 (GPIO18) driven; the bank tap (GPIO1) and ARM sense (GPIO3) read analog.
fn bank(k1: bool, k2: bool) -> PinBank {
    let mut b = PinBank::default();
    *b.get_mut(19).unwrap() = PinDrive { mode: PinMode::Output, out_high: k1, ..PinDrive::default() };
    *b.get_mut(18).unwrap() = PinDrive { mode: PinMode::Output, out_high: k2, ..PinDrive::default() };
    for gpio in [1, 3] {
        *b.get_mut(gpio).unwrap() = PinDrive { mode: PinMode::Analog, ..PinDrive::default() };
    }
    b
}

fn charged(c: &Circuit, volts: f64) -> PowerState {
    let mut st = PowerState::default();
    for l in ["C1", "C2"] {
        st.set_cap_volts(comp(c, l), volts);
    }
    st
}

#[test]
fn bank_bleeds_with_the_302k_time_constant_and_reads_2_over_302() {
    let (lib, c) = setup();
    let board = lib.board(&c.board_id).unwrap();
    let nl = Netlist::build(&c, board, &lib);
    // K2 energized holds R6 off the bank, so only the R1-R4 divider bleeds it.
    let mut st = charged(&c, 300.0);
    let out = step(&c, board, &lib, &nl, &bank(false, true), &mut st, 0.001);
    let mv = f64::from(out.analog_mv[&1]);
    assert!((mv - 300_000.0 * 2.0 / 302.0).abs() < 3.0, "ADC sees {mv} mV");
    for _ in 0..5 {
        step(&c, board, &lib, &nl, &bank(false, true), &mut st, 0.2);
    }
    let v = st.cap_volts(comp(&c, "C1")).unwrap();
    let expect = 300.0 * (-1.001f64 / (302e3 * 190e-6)).exp();
    assert!((v - expect).abs() < 0.3, "after 1 s: {v} V, expected {expect} V");
}

#[test]
fn released_k2_dumps_the_bank_through_r6_within_a_second() {
    let (lib, c) = setup();
    let c = armed(c, true);
    let board = lib.board(&c.board_id).unwrap();
    let nl = Netlist::build(&c, board, &lib);
    let mut st = charged(&c, 300.0);
    let mut t = 0.0;
    let mut below_5v = None;
    while t < 1.0 {
        step(&c, board, &lib, &nl, &bank(false, false), &mut st, 0.01);
        t += 0.01;
        if below_5v.is_none() && st.cap_volts(comp(&c, "C1")).unwrap() < 5.0 {
            below_5v = Some(t);
        }
    }
    // tau = 1 k x 190 uF = 0.19 s: 300 V reaches 5 V after 0.19 x ln(60) = 0.78 s.
    let t5 = below_5v.expect("bank under 5 V within 1 s");
    assert!((0.7..0.85).contains(&t5), "bank under 5 V after {t5:.2} s");
    let v = st.cap_volts(comp(&c, "C2")).unwrap();
    assert!(v < 2.0, "after 1 s: {v} V");
}

#[test]
fn charger_stays_dead_with_the_arm_switch_off() {
    let (lib, c) = setup();
    let c = armed(c, false);
    let board = lib.board(&c.board_id).unwrap();
    let nl = Netlist::build(&c, board, &lib);
    let mut st = PowerState::default();
    let mut out = step(&c, board, &lib, &nl, &bank(true, true), &mut st, 0.01);
    for _ in 0..100 {
        out = step(&c, board, &lib, &nl, &bank(true, true), &mut st, 0.01);
    }
    let v = st.cap_volts(comp(&c, "C1")).unwrap();
    assert!(v.abs() < 0.5, "S1 off, K1 closed: the bank must not charge, got {v} V");
    assert!(out.analog_mv[&3] < 50, "ARM sense reads {} mV with S1 off", out.analog_mv[&3]);
}

#[test]
fn armed_relays_charge_the_bank_to_the_charger_set_point() {
    let (lib, c) = setup();
    let c = armed(c, true);
    let board = lib.board(&c.board_id).unwrap();
    let nl = Netlist::build(&c, board, &lib);
    let mut st = PowerState::default();
    let mut t = 0.0;
    while t < 1.0 {
        step(&c, board, &lib, &nl, &bank(true, true), &mut st, 0.01);
        t += 0.01;
    }
    let v = st.cap_volts(comp(&c, "C1")).unwrap();
    assert!((v - 300.0).abs() < 1.0, "charger regulates at its pot setting: {v}");
    let out = step(&c, board, &lib, &nl, &bank(false, true), &mut st, 0.001);
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    // R7 47 k over R8 10 k: the 12 V supply reads 12 x 10/57 = 2.105 V.
    let arm = f64::from(out.analog_mv[&3]);
    assert!((arm - 12_000.0 * 10.0 / 57.0).abs() < 20.0, "ARM sense reads {arm} mV");
    assert_eq!(out.visuals.get(&comp(&c, "K2")), Some(&VisualState::RelayClosed(true)));
}

#[test]
fn touching_dumps_the_bank_electrode_negative() {
    let (lib, mut c) = setup();
    let board = lib.board(&c.board_id).unwrap();
    let nl = Netlist::build(&c, board, &lib);
    let pen = comp(&c, "PEN");
    c.components.get_mut(&pen).unwrap().state = CompState::Button { pressed: true };
    let mut st = charged(&c, 300.0);
    let out = step(&c, board, &lib, &nl, &bank(false, true), &mut st, 0.004);
    let Some(VisualState::Arc { pulse: Some(p), .. }) = out.visuals.get(&pen).copied() else {
        panic!("no pulse: {:?}", out.visuals.get(&pen));
    };
    assert!(p.dcen, "electrode on the negative side");
    assert!(p.peak_a > 500.0 && p.width_us < 1000.0, "{p:?}");
    assert!(st.cap_volts(comp(&c, "C1")).unwrap() < 20.0);
}

#[test]
fn welder_pins_bind_without_setup_warnings() {
    let (lib, c) = setup();
    let board = lib.board(&c.board_id).unwrap();
    let nl = Netlist::build(&c, board, &lib);
    let (_msgs, b) = wirelab_core::engine::plan_setup(&c, board, &lib, &nl);
    assert!(b.warnings.is_empty(), "{:?}", b.warnings);
    // The 10 k pull-downs reach GND, the ADC pins and each other; each relay keeps its own GPIO.
    assert_eq!(b.outputs.get(&comp(&c, "K1")).map(|o| o.gpio), Some(19));
    assert_eq!(b.outputs.get(&comp(&c, "K2")).map(|o| o.gpio), Some(18));
    let dump = b.inputs.get(&20).expect("DUMP button input");
    assert!(dump.comp == comp(&c, "S2") && dump.active_low);
}

#[test]
fn documents_cover_every_part_and_the_build_matches_the_netlist() {
    let (lib, c) = setup();
    let board = lib.board(&c.board_id).unwrap();
    let ctx = DocCtx::new("CD pulse-arc welder", &c, board, &lib);
    let sch = kicad::schematic(&ctx);
    for r in [
        "PS1", "K1", "K2", "U1", "U2", "C1", "C2", "R1", "R2", "R3", "R4", "R5", "R6", "R7", "R8", "R9", "R10", "D1",
        "S1", "S2", "PEN", "CLAMP",
    ] {
        assert!(sch.contains(&format!("(reference \"{r}\")")), "{r} missing from the schematic");
    }
    assert!(!sch.contains("(reference \"fw\")"), "script controller left out");
    let ops = nets::operating_points(&ctx);
    let md = nets::markdown(&ctx, &ops);
    assert!(md.contains("57.4 s") && md.contains("12 AWG"), "tables carry tau and gauges");
    let (relay, from, t) = ops.dump.clone().expect("dump scenario");
    assert!(relay == "K2" && from == 300.0 && (0.7..0.85).contains(&t), "dump {relay} from {from} V in {t} s");
    assert!(md.contains("Dump time 300 V -> 5 V (K2 released)"), "dump row in the tables");
    assert!(md.contains("| R6 | 1 kOhm Resistor | 1k | 10 W |"), "R6 carries its 10 W rating in the BOM");
    let perf = c.perf.as_ref().expect("perf layout");
    let ready = ops.ready.as_ref().map(|(_, o)| o.net_avg_mv.as_slice());
    let rep = wirelab_core::perf::check(perf, &ctx, ready);
    assert!(rep.shorts.is_empty(), "{:?}", rep.shorts);
    assert!(rep.missing.iter().all(|m| m.contains("not on the build yet")), "{:?}", rep.missing);
    assert!(rep.clearance.iter().any(|m| m.contains("+ rail")), "{:?}", rep.clearance);
}

/// The fw script fed a synthetic bank and ARM supply, ticked every 10 ms.
struct Firmware {
    host: ScriptHost,
    fw: CompId,
    k1: CompId,
    k2: CompId,
    now: u64,
    bank_v: f64,
    supply_v: f64,
    /// Bank held where it is, as with a welded K1 feeding the charger.
    hold: bool,
    k1_on: bool,
    k2_on: bool,
    logs: Vec<String>,
}

impl Firmware {
    fn boot() -> Firmware {
        let (lib, c) = setup();
        let board = lib.board(&c.board_id).unwrap();
        let mut host = ScriptHost::new();
        host.set_board(board.chip.name(), &board.specs, board.features.rgb_led_gpio);
        host.sync(&c, &lib);
        assert!(host.errors.is_empty(), "{:?}", host.errors);
        let mut f = Firmware {
            host,
            fw: comp(&c, "fw"),
            k1: comp(&c, "K1"),
            k2: comp(&c, "K2"),
            now: 1000,
            bank_v: 0.0,
            supply_v: 0.0,
            hold: false,
            k1_on: true,
            k2_on: true,
            logs: Vec::new(),
        };
        f.host.set_world(f.world());
        let a = f.host.on_start(f.fw);
        f.apply(&a);
        assert!(!f.k1_on && !f.k2_on, "both relays released at boot: {a:?}");
        f
    }

    fn world(&self) -> World {
        let bank_mv = (self.bank_v * 1986.8 / 300.0).round() as u16;
        let arm_mv = (self.supply_v * 10.0 / 57.0 * 1000.0).round() as u16;
        World { now_ms: self.now, pin_analog_mv: HashMap::from([(1, bank_mv), (3, arm_mv)]), ..World::default() }
    }

    fn apply(&mut self, actions: &[Action]) {
        for a in actions {
            if let Action::CompAction { comp, action, .. } = a {
                let on = action == "on";
                if *comp == self.k1 {
                    assert!(!on || self.k2_on, "K1 closed while K2 releases the dump at {} ms", self.now);
                    self.k1_on = on;
                } else if *comp == self.k2 {
                    assert!(!on || self.bank_v < 5.0, "K2 energized at {} V", self.bank_v);
                    self.k2_on = on;
                }
            }
        }
        self.logs.extend(self.host.take_logs());
    }

    /// One 10 ms loop; the bank climbs at `rate` V/s through K1 and dumps with tau 0.19 s through a released K2.
    fn tick(&mut self, rate: f64) {
        self.now += 10;
        if !self.hold {
            if self.k1_on {
                self.bank_v += rate * 0.01;
            } else if !self.k2_on {
                self.bank_v *= (-0.01f64 / 0.19).exp();
            }
        }
        self.host.set_world(self.world());
        let a = self.host.tick(self.now);
        self.apply(&a);
    }

    fn run(&mut self, ms: u64, rate: f64) {
        let end = self.now + ms;
        while self.now < end {
            self.tick(rate);
        }
    }

    /// Ticks until `done` holds and returns the time; panics after `ms`.
    fn run_until(&mut self, ms: u64, rate: f64, done: impl Fn(&Firmware) -> bool) -> u64 {
        let end = self.now + ms;
        while self.now < end {
            self.tick(rate);
            if done(self) {
                return self.now;
            }
        }
        panic!("condition not met within {ms} ms; log: {:#?}", self.logs);
    }

    fn pin(&mut self, gpio: u8, high: bool) {
        let a = self.host.on_pin(self.fw, gpio, high);
        self.apply(&a);
    }

    fn count(&self, text: &str) -> usize {
        self.logs.iter().filter(|l| l.contains(text)).count()
    }
}

#[test]
fn firmware_model_arms_dumps_latches_and_faults() {
    let mut f = Firmware::boot();
    f.run(1500, 0.0);
    assert!(!f.k1_on && !f.k2_on, "disarmed while the ARM switch is off");

    // ARM on: K2 opens the dump first, K1 closes at least 60 ms later.
    f.supply_v = 12.0;
    let t_k2 = f.run_until(200, 0.0, |f| f.k2_on);
    let t_k1 = f.run_until(200, 0.0, |f| f.k1_on);
    assert!(t_k1 - t_k2 >= 60, "K1 closed {} ms after K2", t_k1 - t_k2);
    assert!(f.count("SAFE -> ARMING") == 1 && f.count("ARMING -> CHARGE") == 1, "{:#?}", f.logs);

    // A 600 V/s charge stops just under the 240 V set point; K2 keeps holding.
    f.run_until(1000, 600.0, |f| !f.k1_on);
    assert!(f.count("CHARGE -> READY") == 1 && f.k2_on, "{:#?}", f.logs);
    assert!((225.0..240.0).contains(&f.bank_v), "READY at {} V", f.bank_v);

    // DUMP: disarm, K2 releases ~20 ms later, SAFE once the bank stays under 5 V for 200 ms.
    f.pin(20, false);
    let t_dump = f.run_until(20, 0.0, |f| f.count("READY -> DUMPING") == 1);
    let t_rel = f.run_until(100, 0.0, |f| !f.k2_on);
    assert!((20..=30).contains(&(t_rel - t_dump)), "K2 released {} ms after the disarm", t_rel - t_dump);
    f.pin(20, true);
    f.run_until(2000, 0.0, |f| f.count("DUMPING -> SAFE") == 1);
    assert!(f.bank_v < 5.0, "SAFE at {} V", f.bank_v);

    // The DUMP latch holds it disarmed with the switch still on, until the switch is cycled.
    f.run(500, 0.0);
    assert!(!f.k2_on && f.count("SAFE -> ARMING") == 1, "latched: {:#?}", f.logs);
    f.supply_v = 0.0;
    f.run(100, 0.0);
    f.supply_v = 12.0;
    f.run_until(200, 0.0, |f| f.k1_on);
    assert_eq!(f.count("SAFE -> ARMING"), 2);

    // ARM off while charging: K1 opens at once, K2 releases, the bank dumps to SAFE.
    f.supply_v = 0.0;
    f.run_until(50, 300.0, |f| !f.k1_on);
    f.run_until(50, 0.0, |f| !f.k2_on);
    f.run_until(2000, 0.0, |f| f.count("DUMPING -> SAFE") == 2);

    // Over 310 V latches a fault; it clears when the switch goes off with the bank under 310 V.
    f.hold = true;
    f.bank_v = 318.0;
    f.run_until(20, 0.0, |f| f.count("FAULT: OVER 310 V") == 1);
    f.supply_v = 12.0;
    f.run(100, 0.0);
    assert!(!f.k2_on && f.count("-> ARMING") == 2, "no arming in FAULT: {:#?}", f.logs);
    f.bank_v = 100.0;
    f.supply_v = 0.0;
    f.run_until(100, 0.0, |f| f.count("fault cleared") == 1);
    assert_eq!(f.count("FAULT -> DUMPING"), 1);

    // A dump that does not fall (K1 welded, charger still fed) faults DUMP STALLED: "switch ARM off".
    f.run_until(1500, 0.0, |f| f.count("FAULT: DUMP STALLED") == 1);

    // BOOT held 1 s clears the fault; a short press steps the set point.
    f.pin(9, false);
    f.run_until(1200, 0.0, |f| f.count("fault cleared") == 2);
    f.pin(9, true);
    assert_eq!(f.count("set point"), 0, "a long press does not step the set point");
    f.pin(9, false);
    f.run(100, 0.0);
    f.pin(9, true);
    assert_eq!(f.count("set point 280.0 V"), 1, "{:#?}", f.logs);
}

#[test]
fn firmware_model_latches_follow_the_arm_switch_level() {
    // ARM already on at power-up: disarmed until the switch reads OFF and then ON.
    let mut f = Firmware::boot();
    f.supply_v = 12.0;
    f.run(2000, 0.0);
    assert!(!f.k2_on && f.count("-> ARMING") == 0, "armed with ARM on at boot: {:#?}", f.logs);
    f.supply_v = 0.0;
    f.run(100, 0.0);
    f.supply_v = 12.0;
    f.run_until(200, 0.0, |f| f.k1_on);

    // DUMP pressed and released with ARM off: the next ARM ON arms.
    f.supply_v = 0.0;
    f.run_until(2000, 0.0, |f| f.count("DUMPING -> SAFE") == 1);
    f.pin(20, false);
    f.run(50, 0.0);
    f.pin(20, true);
    f.run(100, 0.0);
    f.supply_v = 12.0;
    f.run_until(300, 0.0, |f| f.k1_on);
    assert_eq!(f.count("SAFE -> ARMING"), 2, "{:#?}", f.logs);

    // A held DUMP keeps it disarmed through an ARM cycle; released with ARM on it stays latched.
    f.pin(20, false);
    f.run_until(2000, 0.0, |f| f.count("DUMPING -> SAFE") == 2);
    f.supply_v = 0.0;
    f.run(100, 0.0);
    f.supply_v = 12.0;
    f.run(300, 0.0);
    f.pin(20, true);
    f.run(300, 0.0);
    assert!(!f.k2_on && f.count("SAFE -> ARMING") == 2, "armed after a held DUMP: {:#?}", f.logs);
    f.supply_v = 0.0;
    f.run(100, 0.0);
    f.supply_v = 12.0;
    f.run_until(300, 0.0, |f| f.k1_on);
    assert_eq!(f.count("SAFE -> ARMING"), 3);

    // A fault cleared by a BOOT hold with ARM on stays disarmed until the switch is cycled.
    f.hold = true;
    f.bank_v = 318.0;
    f.run_until(20, 0.0, |f| f.count("FAULT: OVER 310 V") == 1);
    f.hold = false;
    f.bank_v = 0.0;
    f.pin(9, false);
    f.run_until(1200, 0.0, |f| f.count("fault cleared") == 1);
    f.pin(9, true);
    f.run(500, 0.0);
    assert!(!f.k2_on && f.count("SAFE -> ARMING") == 3, "armed right after a BOOT clear: {:#?}", f.logs);
    f.supply_v = 0.0;
    f.run(100, 0.0);
    f.supply_v = 12.0;
    f.run_until(300, 0.0, |f| f.k1_on);
    assert_eq!(f.count("SAFE -> ARMING"), 4);
}

#[test]
fn firmware_model_stop_lands_at_the_set_point_after_the_relay_release() {
    // Still rising through K1's 10 ms release, the bank lands within one sample step under 237 V.
    for rate in [300.0, 600.0, 1000.0] {
        let mut f = Firmware::boot();
        f.run(1100, 0.0);
        f.supply_v = 12.0;
        f.run_until(300, 0.0, |f| f.k1_on);
        f.run_until(2000, rate, |f| !f.k1_on);
        assert!(f.count("CHARGE -> READY") == 1, "{:#?}", f.logs);
        let landed = f.bank_v + rate * 0.010;
        assert!((237.0 - rate * 0.01 - 0.5..=237.5).contains(&landed), "{rate} V/s: lands at {landed} V");
    }
}

#[test]
fn firmware_model_boot_window_counts_an_early_arm_as_on_at_boot() {
    // ARM switched on 100 ms after power-up counts as on at boot: latched until cycled.
    let mut f = Firmware::boot();
    f.run(100, 0.0);
    f.supply_v = 12.0;
    f.run(1500, 0.0);
    assert!(!f.k2_on && f.count("-> ARMING") == 0, "armed inside the boot window: {:#?}", f.logs);
    f.supply_v = 0.0;
    f.run(50, 0.0);
    f.supply_v = 12.0;
    f.run_until(300, 0.0, |f| f.k1_on);

    // ARM off through the first second, then on: K2 and then K1 follow at once.
    let mut f = Firmware::boot();
    f.run(1100, 0.0);
    f.supply_v = 12.0;
    let t = f.run_until(300, 0.0, |f| f.k1_on);
    assert!(t <= 1000 + 1100 + 120, "K1 closed {} ms after ARM on", t - 2100);
}
