//! Electrical simulator: resistive nodal analysis over the netlist.
//!
//! Two passes per solve: a "peak" pass (PWM at rail voltage) for diode/logic
//! state, and an "avg" pass (PWM at duty-scaled voltage) for analog reads.

use std::collections::{HashMap, HashSet};

use wirelab_proto::{HostMsg, PinMode};

use crate::board::BoardProfile;
use crate::circuit::{Circuit, CompId, Endpoint};
use crate::component::{CompState, SimModel, TerminalRole, VisualState};
use crate::library::Library;
use crate::netlist::Netlist;

pub const RAIL_MV: f32 = 3300.0;
pub const LOGIC_THRESHOLD_MV: f32 = 1650.0;
const PULL_OHMS: f32 = 45_000.0;
const LED_ON_OHMS: f32 = 300.0;
const CONTACT_OHMS: f32 = 0.1;
const RELAX_ITERS: usize = 80;

/// Commanded state of one GPIO, as the firmware would hold it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PinDrive {
    pub mode: PinMode,
    pub out_high: bool,
    pub duty: f32,
    pub freq_hz: f32,
}

impl Default for PinDrive {
    fn default() -> Self {
        PinDrive { mode: PinMode::Disabled, out_high: false, duty: 0.0, freq_hz: 0.0 }
    }
}

/// Shadow of the full GPIO bank; both the simulator and the GUI mirror use it.
#[derive(Debug, Clone, PartialEq)]
pub struct PinBank {
    pub pins: [PinDrive; 64],
}

impl Default for PinBank {
    fn default() -> Self {
        PinBank { pins: [PinDrive::default(); 64] }
    }
}

impl PinBank {
    pub fn get(&self, gpio: u8) -> PinDrive {
        self.pins.get(gpio as usize).copied().unwrap_or_default()
    }

    pub fn get_mut(&mut self, gpio: u8) -> Option<&mut PinDrive> {
        self.pins.get_mut(gpio as usize)
    }

    /// Apply the pin-affecting subset of the host protocol.
    pub fn apply(&mut self, msg: &HostMsg) {
        match *msg {
            HostMsg::Reset => *self = PinBank::default(),
            HostMsg::SetPinMode { pin, mode } => {
                if let Some(p) = self.get_mut(pin) {
                    *p = PinDrive { mode, ..PinDrive::default() };
                }
            }
            HostMsg::WriteDigital { pin, high } => {
                if let Some(p) = self.get_mut(pin) {
                    p.out_high = high;
                }
            }
            HostMsg::SetPwm { pin, freq_hz, duty_permille } => {
                if let Some(p) = self.get_mut(pin) {
                    p.mode = PinMode::Pwm;
                    p.freq_hz = freq_hz as f32;
                    p.duty = f32::from(duty_permille.min(1000)) / 1000.0;
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SimOutput {
    /// Per-net potential from the peak pass; `None` = floating.
    pub net_peak_mv: Vec<Option<f32>>,
    /// Per-net potential from the duty-averaged pass.
    pub net_avg_mv: Vec<Option<f32>>,
    /// Level seen by every input-mode GPIO.
    pub digital: HashMap<u8, bool>,
    /// Millivolts seen by every analog-mode GPIO.
    pub analog_mv: HashMap<u8, u16>,
    pub visuals: HashMap<CompId, VisualState>,
    pub warnings: Vec<String>,
    /// Last color commanded on the board's addressable LED, if any.
    pub rgb: Option<[u8; 3]>,
    /// Estimated current sourced/sunk per driven GPIO, in mA.
    pub source_ma: HashMap<u8, f32>,
    /// Replayable draw ops for the simulated ST7735 (None = not initialized).
    pub lcd: Option<Vec<LcdOp>>,
}

/// One simulated display operation (colors are 8-bit RGB).
#[derive(Debug, Clone, PartialEq)]
pub enum LcdOp {
    Clear([u8; 3]),
    Rect { x: u8, y: u8, w: u8, h: u8, rgb: [u8; 3] },
    Text { x: u8, y: u8, rgb: [u8; 3], text: String },
}

/// Unpack the display's RGB565 back to 8-bit RGB for drawing.
pub fn rgb888(v: u16) -> [u8; 3] {
    [((v >> 11) as u8 & 0x1f) << 3, ((v >> 5) as u8 & 0x3f) << 2, (v as u8 & 0x1f) << 3]
}

#[derive(Clone, Copy)]
struct Element {
    a: usize,
    b: usize,
    siemens: f32,
}

struct Pull {
    net: usize,
    mv: f32,
    siemens: f32,
}

struct LedInst {
    comp: CompId,
    anode: usize,
    cathode: usize,
    forward_mv: f32,
}

struct RelayInst {
    comp: CompId,
    signal: usize,
    common: usize,
    no: usize,
    nc: usize,
}

fn term_net(nl: &Netlist, comp: CompId, def_terminal: &str) -> Option<usize> {
    nl.net_of(&Endpoint::Terminal { comp, terminal: def_terminal.to_string() })
}

fn role_net(
    nl: &Netlist,
    lib: &Library,
    circuit: &Circuit,
    comp: CompId,
    role: TerminalRole,
) -> Option<usize> {
    let def = lib.component(&circuit.components.get(&comp)?.def_id)?;
    let t = def.terminal_by_role(role)?;
    term_net(nl, comp, &t.id)
}

pub fn solve(
    circuit: &Circuit,
    board: &BoardProfile,
    lib: &Library,
    nl: &Netlist,
    bank: &PinBank,
) -> SimOutput {
    let n = nl.nets.len();
    let mut out = SimOutput {
        net_peak_mv: vec![None; n],
        net_avg_mv: vec![None; n],
        ..SimOutput::default()
    };

    // Static resistive elements and powered sources from components.
    let mut elements: Vec<Element> = Vec::new();
    let pulls: Vec<Pull> = Vec::new();
    let mut fixed_extra: Vec<(usize, f32)> = Vec::new();
    let mut joins: Vec<(usize, usize)> = Vec::new();
    let mut leds: Vec<LedInst> = Vec::new();
    let mut relays: Vec<RelayInst> = Vec::new();

    for comp in circuit.components.values() {
        let Some(def) = lib.component(&comp.def_id) else { continue };
        let net_of_role = |role| role_net(nl, lib, circuit, comp.id, role);
        match &def.sim {
            SimModel::Resistor { ohms } => {
                let ohms = comp.props.get("ohms").map(|v| *v as f32).unwrap_or(*ohms);
                if let (Some(a), Some(b)) = (net_of_role(TerminalRole::A), net_of_role(TerminalRole::B)) {
                    elements.push(Element { a, b, siemens: 1.0 / ohms.max(1.0) });
                }
            }
            SimModel::Photoresistor { dark_ohms, bright_ohms } => {
                let level = match comp.state {
                    CompState::Fraction { value } => value.clamp(0.0, 1.0),
                    _ => 0.5,
                };
                let ohms = dark_ohms * (bright_ohms / dark_ohms).powf(level);
                if let (Some(a), Some(b)) = (net_of_role(TerminalRole::A), net_of_role(TerminalRole::B)) {
                    elements.push(Element { a, b, siemens: 1.0 / ohms.max(1.0) });
                }
            }
            SimModel::Potentiometer { ohms } => {
                let ohms = comp.props.get("ohms").map(|v| *v as f32).unwrap_or(*ohms);
                let f = match comp.state {
                    CompState::Fraction { value } => value.clamp(0.0, 1.0),
                    _ => 0.5,
                };
                if let (Some(a), Some(b), Some(w)) = (
                    net_of_role(TerminalRole::EndA),
                    net_of_role(TerminalRole::EndB),
                    net_of_role(TerminalRole::Wiper),
                ) {
                    elements.push(Element { a, b: w, siemens: 1.0 / (ohms * f).max(1.0) });
                    elements.push(Element { a: w, b, siemens: 1.0 / (ohms * (1.0 - f)).max(1.0) });
                }
            }
            SimModel::PushButton => {
                if matches!(comp.state, CompState::Button { pressed: true })
                    && let (Some(a), Some(b)) =
                        (net_of_role(TerminalRole::A), net_of_role(TerminalRole::B))
                    {
                        joins.push((a, b));
                    }
            }
            SimModel::ToggleSwitch => {
                if matches!(comp.state, CompState::Toggle { on: true })
                    && let (Some(a), Some(b)) =
                        (net_of_role(TerminalRole::A), net_of_role(TerminalRole::B))
                    {
                        joins.push((a, b));
                    }
            }
            SimModel::SlideSwitchSpdt => {
                let on = matches!(comp.state, CompState::Toggle { on: true });
                let side = if on { TerminalRole::B } else { TerminalRole::A };
                if let (Some(c), Some(s)) = (net_of_role(TerminalRole::Common), net_of_role(side)) {
                    joins.push((c, s));
                }
            }
            SimModel::Led { forward_mv } => {
                if let (Some(a), Some(c)) =
                    (net_of_role(TerminalRole::Anode), net_of_role(TerminalRole::Cathode))
                {
                    leds.push(LedInst {
                        comp: comp.id,
                        anode: a,
                        cathode: c,
                        forward_mv: f32::from(*forward_mv),
                    });
                }
            }
            SimModel::Buzzer { .. } => {
                if let (Some(s), Some(g)) =
                    (net_of_role(TerminalRole::Signal), net_of_role(TerminalRole::Gnd))
                {
                    elements.push(Element { a: s, b: g, siemens: 1.0 / 1000.0 });
                }
            }
            SimModel::RelayModule => {
                if let (Some(sig), Some(com), Some(no), Some(nc)) = (
                    net_of_role(TerminalRole::Signal),
                    net_of_role(TerminalRole::Common),
                    net_of_role(TerminalRole::NormallyOpen),
                    net_of_role(TerminalRole::NormallyClosed),
                ) {
                    relays.push(RelayInst { comp: comp.id, signal: sig, common: com, no, nc });
                }
            }
            SimModel::DigitalSensor => {
                if powered(nl, lib, circuit, comp.id)
                    && let Some(sig) = net_of_role(TerminalRole::Signal) {
                        let high = matches!(comp.state, CompState::Toggle { on: true });
                        fixed_extra.push((sig, if high { RAIL_MV } else { 0.0 }));
                    }
            }
            SimModel::AnalogSensor { min_mv, max_mv } => {
                if powered(nl, lib, circuit, comp.id)
                    && let Some(sig) = net_of_role(TerminalRole::Signal) {
                        let f = match comp.state {
                            CompState::Fraction { value } => value.clamp(0.0, 1.0),
                            _ => 0.5,
                        };
                        let span = f32::from(max_mv.saturating_sub(*min_mv));
                        fixed_extra.push((sig, f32::from(*min_mv) + f * span));
                    }
            }
            SimModel::Servo | SimModel::Generic => {}
        }
    }

    // Topology iteration: LED conduction and relay contacts feed back.
    let mut led_on: HashMap<CompId, bool> = leds.iter().map(|l| (l.comp, false)).collect();
    let mut relay_on: HashMap<CompId, bool> = relays.iter().map(|r| (r.comp, false)).collect();
    let mut peak = vec![None; n];
    for _iter in 0..5 {
        let mut elems: Vec<Element> = elements.clone();
        for l in &leds {
            if led_on[&l.comp] {
                elems.push(Element { a: l.anode, b: l.cathode, siemens: 1.0 / LED_ON_OHMS });
            }
        }
        for r in &relays {
            let contact = if relay_on[&r.comp] { r.no } else { r.nc };
            elems.push(Element { a: r.common, b: contact, siemens: 1.0 / CONTACT_OHMS });
        }
        let mut iter_warnings = Vec::new();
        peak = relax(n, nl, board, bank, &elems, &pulls, &fixed_extra, &joins, true, &mut iter_warnings);
        out.warnings = iter_warnings;

        let mut changed = false;
        for l in &leds {
            let on = match (peak[l.anode], peak[l.cathode]) {
                (Some(va), Some(vc)) => va - vc > l.forward_mv - 400.0,
                _ => false,
            };
            if led_on[&l.comp] != on {
                led_on.insert(l.comp, on);
                changed = true;
            }
        }
        for r in &relays {
            let on = peak[r.signal].is_some_and(|v| v > LOGIC_THRESHOLD_MV);
            if relay_on[&r.comp] != on {
                relay_on.insert(r.comp, on);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Final avg pass with settled topology.
    let mut elems: Vec<Element> = elements.clone();
    for l in &leds {
        if led_on[&l.comp] {
            elems.push(Element { a: l.anode, b: l.cathode, siemens: 1.0 / LED_ON_OHMS });
        }
    }
    for r in &relays {
        let contact = if relay_on[&r.comp] { r.no } else { r.nc };
        elems.push(Element { a: r.common, b: contact, siemens: 1.0 / CONTACT_OHMS });
    }
    let mut avg_warn = Vec::new();
    let avg = relax(n, nl, board, bank, &elems, &pulls, &fixed_extra, &joins, false, &mut avg_warn);
    out.warnings.extend(avg_warn);

    // Real electrical rules: estimate branch currents from the solved
    // voltages (mV x S = mA) and flag overcurrent.
    for pin in &board.pins {
        let Some(gpio) = pin.kind.gpio() else { continue };
        if !bank.get(gpio).mode.is_output() {
            continue;
        }
        let Some(net) = nl.net_of(&Endpoint::BoardPin { key: pin.key.clone() }) else {
            continue;
        };
        let Some(v) = avg[net] else { continue };
        let mut ma = 0.0f32;
        for e in &elems {
            if e.a == net {
                ma += (v - avg[e.b].unwrap_or(v)) * e.siemens;
            } else if e.b == net {
                ma += (v - avg[e.a].unwrap_or(v)) * e.siemens;
            }
        }
        out.source_ma.insert(gpio, ma);
        if ma.abs() > 30.0 {
            out.warnings.push(format!(
                "GPIO{gpio} would source ~{:.0} mA — beyond the pin's absolute limit (~40 mA); add resistance",
                ma.abs()
            ));
        } else if ma.abs() > 15.0 {
            out.warnings.push(format!(
                "GPIO{gpio} sourcing ~{:.0} mA — above the recommended ~10 mA per pin",
                ma.abs()
            ));
        }
    }
    // LEDs: a real diode has almost no resistance past its forward drop, so
    // estimate the true current, not the tame simulator model's.
    for l in &leds {
        if !led_on[&l.comp] {
            continue;
        }
        let (Some(va), Some(vc)) = (avg[l.anode], avg[l.cathode]) else { continue };
        let est_ma = (va - vc - l.forward_mv).max(0.0) / 25.0;
        if est_ma > 20.0 {
            let name = circuit
                .components
                .get(&l.comp)
                .map(|c| {
                    if c.label.is_empty() {
                        lib.component(&c.def_id).map(|d| d.name.clone()).unwrap_or_default()
                    } else {
                        c.label.clone()
                    }
                })
                .unwrap_or_default();
            out.warnings.push(format!(
                "{name}: a real LED here would pass ~{est_ma:.0} mA (rating ~20 mA) — it needs more series resistance"
            ));
        }
    }

    // Pin readings.
    for pin in &board.pins {
        let Some(gpio) = pin.kind.gpio() else { continue };
        let drive = bank.get(gpio);
        let Some(net) = nl.net_of(&Endpoint::BoardPin { key: pin.key.clone() }) else { continue };
        if drive.mode.is_input() {
            let level = match avg[net] {
                Some(v) => v > LOGIC_THRESHOLD_MV,
                None => matches!(drive.mode, PinMode::InputPullUp),
            };
            out.digital.insert(gpio, level);
        } else if drive.mode == PinMode::Analog {
            let mv = avg[net].unwrap_or(0.0).clamp(0.0, 3300.0);
            out.analog_mv.insert(gpio, mv as u16);
        } else if drive.mode.is_output() {
            let level = match drive.mode {
                PinMode::Pwm => drive.duty > 0.5,
                _ => drive.out_high,
            };
            out.digital.insert(gpio, level);
        }
    }

    // Component visuals.
    for comp in circuit.components.values() {
        let Some(def) = lib.component(&comp.def_id) else { continue };
        let net_of_role = |role| role_net(nl, lib, circuit, comp.id, role);
        let vis = match &def.sim {
            SimModel::Led { .. } => {
                let (Some(a), Some(c)) =
                    (net_of_role(TerminalRole::Anode), net_of_role(TerminalRole::Cathode))
                else {
                    out.visuals.insert(comp.id, VisualState::Inert);
                    continue;
                };
                let base = if led_on[&comp.id] {
                    let across = match (peak[a], peak[c]) {
                        (Some(va), Some(vc)) => va - vc,
                        _ => 0.0,
                    };
                    // mV across / ohms = mA; ~8 mA reads as full brightness.
                    ((across / LED_ON_OHMS) / 8.0).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let duty = pwm_duty_on_reach(nl, bank, a).or(pwm_duty_on_reach(nl, bank, c));
                VisualState::LedBrightness(base * duty.unwrap_or(1.0))
            }
            SimModel::Servo => {
                let Some(sig) = net_of_role(TerminalRole::Signal) else {
                    out.visuals.insert(comp.id, VisualState::Inert);
                    continue;
                };
                match pwm_on_reach(nl, bank, sig) {
                    Some((duty, freq)) if freq > 20.0 && freq < 400.0 => {
                        let pulse_ms = duty * 1000.0 / freq;
                        let angle = ((pulse_ms - 0.5) / 2.0).clamp(0.0, 1.0) * 180.0;
                        VisualState::ServoAngle(angle)
                    }
                    _ => VisualState::Inert,
                }
            }
            SimModel::Buzzer { active } => {
                let (Some(s), Some(g)) =
                    (net_of_role(TerminalRole::Signal), net_of_role(TerminalRole::Gnd))
                else {
                    out.visuals.insert(comp.id, VisualState::Inert);
                    continue;
                };
                let across = match (peak[s], peak[g]) {
                    (Some(vs), Some(vg)) => vs - vg,
                    _ => 0.0,
                };
                if *active {
                    if across > 2000.0 {
                        VisualState::BuzzerOn { freq_hz: 2300.0 }
                    } else {
                        VisualState::Inert
                    }
                } else {
                    match pwm_on_reach(nl, bank, s) {
                        Some((duty, freq)) if duty > 0.0 && freq > 0.0 && across > 1000.0 => {
                            VisualState::BuzzerOn { freq_hz: freq }
                        }
                        _ => VisualState::Inert,
                    }
                }
            }
            SimModel::RelayModule => VisualState::RelayClosed(relay_on[&comp.id]),
            _ => VisualState::Inert,
        };
        out.visuals.insert(comp.id, vis);
    }

    out.net_peak_mv = peak;
    out.net_avg_mv = avg;
    out
}

/// True when the component's Vcc/Gnd terminals reach a supply and ground.
fn powered(nl: &Netlist, lib: &Library, circuit: &Circuit, comp: CompId) -> bool {
    let vcc = role_net(nl, lib, circuit, comp, TerminalRole::Vcc);
    let gnd = role_net(nl, lib, circuit, comp, TerminalRole::Gnd);
    match (vcc, gnd) {
        (Some(v), Some(g)) => {
            let vr = nl.reach_rails(v);
            let gr = nl.reach_rails(g);
            (vr.v33 || vr.v5) && gr.gnd
        }
        _ => false,
    }
}

fn pwm_on_reach(nl: &Netlist, bank: &PinBank, net: usize) -> Option<(f32, f32)> {
    for &g in nl.reach_gpios(net) {
        let d = bank.get(g);
        if d.mode == PinMode::Pwm {
            return Some((d.duty, d.freq_hz));
        }
    }
    None
}

fn pwm_duty_on_reach(nl: &Netlist, bank: &PinBank, net: usize) -> Option<f32> {
    pwm_on_reach(nl, bank, net).map(|(d, _)| d)
}

/// Gauss-Seidel relaxation over supernodes; returns per-net potentials.
#[allow(clippy::too_many_arguments)]
fn relax(
    n: usize,
    nl: &Netlist,
    board: &BoardProfile,
    bank: &PinBank,
    elements: &[Element],
    extra_pulls: &[Pull],
    fixed_extra: &[(usize, f32)],
    joins: &[(usize, usize)],
    peak: bool,
    warnings: &mut Vec<String>,
) -> Vec<Option<f32>> {
    // Supernode union across ideal joins (closed switches).
    let mut super_of: Vec<usize> = (0..n).collect();
    fn find(super_of: &mut Vec<usize>, i: usize) -> usize {
        if super_of[i] != i {
            let r = find(super_of, super_of[i]);
            super_of[i] = r;
            r
        } else {
            i
        }
    }
    for &(a, b) in joins {
        let (ra, rb) = (find(&mut super_of, a), find(&mut super_of, b));
        if ra != rb {
            super_of[rb] = ra;
        }
    }
    let root: Vec<usize> = (0..n).map(|i| find(&mut super_of, i)).collect();

    // Fixed potentials: rails and driven GPIO outputs.
    let mut fixed: HashMap<usize, f32> = HashMap::new();
    let mut set_fixed = |fixed: &mut HashMap<usize, f32>, node: usize, mv: f32, what: &str| {
        if let Some(prev) = fixed.get(&node) {
            if (prev - mv).abs() > 1.0 {
                warnings.push(format!("short circuit: {what} conflicts with another driver"));
            }
        } else {
            fixed.insert(node, mv);
        }
    };
    for net in &nl.nets {
        let node = root[net.id];
        if net.rails.gnd {
            set_fixed(&mut fixed, node, 0.0, "GND rail");
        }
        if net.rails.v33 {
            set_fixed(&mut fixed, node, 3300.0, "3V3 rail");
        }
        if net.rails.v5 {
            set_fixed(&mut fixed, node, 5000.0, "5V rail");
        }
    }
    let mut pulls: Vec<Pull> = extra_pulls
        .iter()
        .map(|p| Pull { net: root[p.net], mv: p.mv, siemens: p.siemens })
        .collect();
    for pin in &board.pins {
        let Some(gpio) = pin.kind.gpio() else { continue };
        let Some(net) = nl.net_of(&Endpoint::BoardPin { key: pin.key.clone() }) else { continue };
        let node = root[net];
        let d = bank.get(gpio);
        match d.mode {
            PinMode::Output => {
                let mv = if d.out_high { RAIL_MV } else { 0.0 };
                set_fixed(&mut fixed, node, mv, &format!("GPIO{gpio} output"));
            }
            PinMode::OutputOpenDrain => {
                if !d.out_high {
                    set_fixed(&mut fixed, node, 0.0, &format!("GPIO{gpio} open-drain"));
                }
            }
            PinMode::Pwm => {
                let mv = if peak {
                    if d.duty > 0.0 { RAIL_MV } else { 0.0 }
                } else {
                    RAIL_MV * d.duty
                };
                set_fixed(&mut fixed, node, mv, &format!("GPIO{gpio} PWM"));
            }
            PinMode::InputPullUp => {
                pulls.push(Pull { net: node, mv: RAIL_MV, siemens: 1.0 / PULL_OHMS })
            }
            PinMode::InputPullDown => {
                pulls.push(Pull { net: node, mv: 0.0, siemens: 1.0 / PULL_OHMS })
            }
            _ => {}
        }
    }
    for &(net, mv) in fixed_extra {
        set_fixed(&mut fixed, root[net], mv, "sensor output");
    }

    // Adjacency in supernode space.
    let mut adj: HashMap<usize, Vec<(usize, f32)>> = HashMap::new();
    for e in elements {
        let (a, b) = (root[e.a], root[e.b]);
        if a == b {
            continue;
        }
        adj.entry(a).or_default().push((b, e.siemens));
        adj.entry(b).or_default().push((a, e.siemens));
    }
    let mut pull_g: HashMap<usize, Vec<(f32, f32)>> = HashMap::new();
    for p in &pulls {
        pull_g.entry(p.net).or_default().push((p.mv, p.siemens));
    }

    // A node participates when a path of elements connects it to any source.
    let mut grounded: HashSet<usize> = fixed.keys().copied().collect();
    grounded.extend(pull_g.keys().copied());
    let mut frontier: Vec<usize> = grounded.iter().copied().collect();
    while let Some(node) = frontier.pop() {
        if let Some(nbrs) = adj.get(&node) {
            for &(other, _) in nbrs {
                if grounded.insert(other) {
                    frontier.push(other);
                }
            }
        }
    }

    let mut v: HashMap<usize, f32> = HashMap::new();
    for &node in &grounded {
        v.insert(node, fixed.get(&node).copied().unwrap_or(0.0));
    }
    for _ in 0..RELAX_ITERS {
        for &node in &grounded {
            if fixed.contains_key(&node) {
                continue;
            }
            let mut num = 0.0f32;
            let mut den = 0.0f32;
            if let Some(nbrs) = adj.get(&node) {
                for &(other, g) in nbrs {
                    if let Some(&vo) = v.get(&other) {
                        num += g * vo;
                        den += g;
                    }
                }
            }
            if let Some(ps) = pull_g.get(&node) {
                for &(mv, g) in ps {
                    num += g * mv;
                    den += g;
                }
            }
            if den > 0.0 {
                v.insert(node, num / den);
            }
        }
    }

    (0..n)
        .map(|i| {
            let node = root[i];
            if grounded.contains(&node) { v.get(&node).copied() } else { None }
        })
        .collect()
}
