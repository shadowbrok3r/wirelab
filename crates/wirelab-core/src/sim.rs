//! Electrical simulator: nodal analysis over the netlist.
//!
//! Two passes per solve: a "peak" pass (PWM at rail voltage) for diode/logic
//! state, and an "avg" pass (PWM at duty-scaled voltage) for analog reads.
//! [`step`] adds time: capacitors integrate between frames, and supplies,
//! chargers, diodes and touch electrodes settle as piecewise-linear devices.

use std::collections::{HashMap, HashSet};

use wirelab_proto::{HostMsg, PinMode};

use crate::board::BoardProfile;
use crate::circuit::{Circuit, CompId, Endpoint};
use crate::component::{ArcPulse, BoostMode, CompState, SimModel, TerminalRole, VisualState};
use crate::library::Library;
use crate::netlist::Netlist;

pub const RAIL_MV: f32 = 3300.0;
pub const LOGIC_THRESHOLD_MV: f32 = 1650.0;
const PULL_OHMS: f32 = 45_000.0;
const LED_ON_OHMS: f64 = 300.0;
const CONTACT_OHMS: f64 = 0.1;

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

/// Side of the simulated ST7735 panel in pixels.
pub const LCD_SIDE: u16 = 128;
/// Glyph cell of the firmware's FONT_6X10.
pub const LCD_GLYPH: (u16, u16) = (6, 10);
/// Ops kept per display log; the oldest after the base clear go first.
const LCD_MAX_OPS: usize = 1024;

impl LcdOp {
    /// Pixel bounds `[x0, y0, x1, y1)` clipped to the panel.
    pub fn bounds(&self) -> [u16; 4] {
        let clip = |v: u16| v.min(LCD_SIDE);
        match self {
            LcdOp::Clear(_) => [0, 0, LCD_SIDE, LCD_SIDE],
            LcdOp::Rect { x, y, w, h, .. } => {
                let (x, y) = (u16::from(*x), u16::from(*y));
                [clip(x), clip(y), clip(x + u16::from(*w)), clip(y + u16::from(*h))]
            }
            LcdOp::Text { x, y, text, .. } => {
                let (x, y) = (u16::from(*x), u16::from(*y));
                let cols = text.chars().count().min(usize::from(LCD_SIDE)) as u16;
                [clip(x), clip(y), clip(x + cols * LCD_GLYPH.0), clip(y + LCD_GLYPH.1)]
            }
        }
    }

    fn within(&self, outer: [u16; 4]) -> bool {
        let [x0, y0, x1, y1] = self.bounds();
        x0 >= outer[0] && y0 >= outer[1] && x1 <= outer[2] && y1 <= outer[3]
    }
}

/// Applies one host display message to a simulated op log; returns true when the log changed.
///
/// A fill drops the earlier ops it fully covers, so the log stays bounded for scripts that redraw
/// regions, and a full-panel fill restarts the log from a clear.
pub fn apply_lcd_msg(lcd: &mut Option<Vec<LcdOp>>, msg: &HostMsg) -> bool {
    match msg {
        HostMsg::LcdInit { .. } => {
            *lcd = Some(vec![LcdOp::Clear([0, 0, 0])]);
            true
        }
        HostMsg::LcdClear { rgb565 } => match lcd {
            Some(ops) => {
                *ops = vec![LcdOp::Clear(rgb888(*rgb565))];
                true
            }
            None => false,
        },
        HostMsg::LcdRect { x, y, w, h, rgb565 } => match lcd {
            Some(ops) => {
                let op = LcdOp::Rect { x: *x, y: *y, w: *w, h: *h, rgb: rgb888(*rgb565) };
                let area = op.bounds();
                if area[0] >= area[2] || area[1] >= area[3] {
                    return false;
                }
                if area == [0, 0, LCD_SIDE, LCD_SIDE] {
                    *ops = vec![LcdOp::Clear(rgb888(*rgb565))];
                } else {
                    let mut first = true;
                    ops.retain(|o| std::mem::take(&mut first) || !o.within(area));
                    push_capped(ops, op);
                }
                true
            }
            None => false,
        },
        HostMsg::LcdText { x, y, rgb565, text } => match lcd {
            Some(ops) => {
                push_capped(
                    ops,
                    LcdOp::Text { x: *x, y: *y, rgb: rgb888(*rgb565), text: text.to_string() },
                );
                true
            }
            None => false,
        },
        _ => false,
    }
}

fn push_capped(ops: &mut Vec<LcdOp>, op: LcdOp) {
    ops.push(op);
    if ops.len() > LCD_MAX_OPS {
        let keep_base = usize::from(matches!(ops.first(), Some(LcdOp::Clear(_))));
        let excess = ops.len() - LCD_MAX_OPS;
        ops.drain(keep_base..keep_base + excess);
    }
}

/// Two-terminal branch: a conductance with an optional series EMF.
#[derive(Clone, Copy, Debug)]
struct Element {
    a: usize,
    b: usize,
    siemens: f64,
    /// Series EMF in mV: current a->b = siemens * (va - vb - emf), in mA.
    emf: f64,
}

impl Element {
    fn r(a: usize, b: usize, siemens: f64) -> Self {
        Element { a, b, siemens, emf: 0.0 }
    }
}

struct Pull {
    net: usize,
    mv: f64,
    siemens: f64,
}

struct LedInst {
    comp: CompId,
    anode: usize,
    cathode: usize,
    forward_mv: f64,
}

struct RelayInst {
    comp: CompId,
    signal: usize,
    common: usize,
    no: usize,
    nc: usize,
}

struct CapInst {
    comp: CompId,
    pos: usize,
    neg: usize,
    farads: f64,
    esr: f64,
    rated: f64,
}

struct SupplyInst {
    comp: CompId,
    pos: usize,
    neg: usize,
    mv: f64,
    r_int: f64,
    i_max_a: f64,
}

struct BoostInst {
    comp: CompId,
    inp: usize,
    inn: usize,
    outp: usize,
    outn: usize,
    v_set: f64,
    i_max_a: f64,
    p_max_w: f64,
    vin_min: f64,
    vin_max: f64,
    eff: f64,
}

struct DiodeInst {
    comp: CompId,
    anode: usize,
    cathode: usize,
    vf_mv: f64,
    r_on: f64,
}

struct ArcInst {
    comp: CompId,
    lead: usize,
    tip: usize,
    r_contact: f64,
    v_arc_mv: f64,
    touching: bool,
}

struct ResInst {
    comp: CompId,
    a: usize,
    b: usize,
    ohms: f64,
    watts: f64,
    v_max: f64,
}

/// Everything the solver needs from the circuit, gathered once per frame.
#[derive(Default)]
struct Parts {
    elements: Vec<Element>,
    /// Low-resistance passive links (net pairs), for supply-reach searches.
    links: Vec<(usize, usize)>,
    fixed_extra: Vec<(usize, f32)>,
    joins: Vec<(usize, usize)>,
    leds: Vec<LedInst>,
    relays: Vec<RelayInst>,
    caps: Vec<CapInst>,
    supplies: Vec<SupplyInst>,
    boosts: Vec<BoostInst>,
    diodes: Vec<DiodeInst>,
    arcs: Vec<ArcInst>,
    resistors: Vec<ResInst>,
}

/// Charger regulation state.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
enum BoostSt {
    #[default]
    Off,
    /// Enabled, output above the set point: not delivering.
    Idle,
    /// Regulating at the set voltage.
    Thevenin,
    /// Current/power limited, mA.
    Source(f64),
}

#[derive(Debug, Clone, Copy, Default)]
struct BoostDyn {
    st: BoostSt,
    i_in_ma: f64,
    i_out_ma: f64,
    /// Current limit was set by power, not the module's current cap.
    power_limited: bool,
    over_vin: bool,
}

/// Discrete device states that the iteration settles.
#[derive(Debug, Clone, Default)]
struct Topo {
    led_on: HashMap<CompId, bool>,
    relay_on: HashMap<CompId, bool>,
    diode_on: HashMap<CompId, bool>,
    /// 0 = not conducting, +1 = tip -> lead, -1 = lead -> tip.
    arc_dir: HashMap<CompId, i8>,
    boost: HashMap<CompId, BoostDyn>,
}

enum CapMode<'a> {
    /// DC operating point: capacitors are open.
    Open,
    /// Backward-Euler companion model with step `h` seconds.
    Companion { v: &'a HashMap<CompId, f64>, h: f64 },
}

/// Ohms of the charger's regulation loop when holding its set point.
const BOOST_REG_OHMS: f64 = 0.2;
/// Substep for slow dynamics (charging, bleeding), seconds.
const BASE_DT: f64 = 5e-4;
/// Substep while an electrode is striking, seconds.
const ARC_DT: f64 = 2e-6;
/// Fast substeps last this long after each touch, seconds.
const ARC_WINDOW_S: f64 = 1.5e-3;
/// Longest frame advanced in one call, seconds.
const MAX_FRAME_S: f64 = 0.25;
const MAX_SUBSTEPS: usize = 4000;
const SETTLE_ITERS: usize = 24;

/// Persistent analog state the transient simulator carries between frames.
#[derive(Debug, Clone, Default)]
pub struct PowerState {
    /// Capacitor voltages (excluding the ESR drop), volts.
    pub caps: HashMap<CompId, f64>,
    /// Simulated seconds advanced so far.
    pub time_s: f64,
    /// Last finished pulse per electrode and the time it ended.
    pub pulses: HashMap<CompId, (ArcPulse, f64)>,
    runs: HashMap<CompId, ArcRun>,
    topo: Option<Topo>,
}

#[derive(Debug, Clone, Copy, Default)]
struct ArcRun {
    active: bool,
    peak_ma: f64,
    joules: f64,
    width_s: f64,
    dcen: bool,
    touch_age_s: f64,
    was_touching: bool,
}

impl PowerState {
    /// Preset a capacitor's voltage, e.g. to start a test from a charged bank.
    pub fn set_cap_volts(&mut self, comp: CompId, volts: f64) {
        self.caps.insert(comp, volts);
    }

    pub fn cap_volts(&self, comp: CompId) -> Option<f64> {
        self.caps.get(&comp).copied()
    }
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

fn comp_name(circuit: &Circuit, lib: &Library, comp: CompId) -> String {
    circuit
        .components
        .get(&comp)
        .map(|c| {
            if c.label.is_empty() {
                lib.component(&c.def_id).map(|d| d.name.clone()).unwrap_or_default()
            } else {
                c.label.clone()
            }
        })
        .unwrap_or_default()
}

fn prop(comp: &crate::circuit::PlacedComponent, key: &str, default: f64) -> f64 {
    comp.props.get(key).copied().unwrap_or(default)
}

fn collect(circuit: &Circuit, lib: &Library, nl: &Netlist) -> Parts {
    let mut p = Parts::default();
    for comp in circuit.components.values() {
        let Some(def) = lib.component(&comp.def_id) else { continue };
        let net_of_role = |role| role_net(nl, lib, circuit, comp.id, role);
        let pressed = matches!(comp.state, CompState::Button { pressed: true });
        match &def.sim {
            SimModel::Resistor { ohms } => {
                let ohms = prop(comp, "ohms", f64::from(*ohms));
                if let (Some(a), Some(b)) = (net_of_role(TerminalRole::A), net_of_role(TerminalRole::B)) {
                    p.elements.push(Element::r(a, b, 1.0 / ohms.max(1.0)));
                    p.resistors.push(ResInst {
                        comp: comp.id,
                        a,
                        b,
                        ohms: ohms.max(1.0),
                        watts: prop(comp, "watts", 0.25),
                        v_max: prop(comp, "v_max", 250.0),
                    });
                    if ohms < 1.0 {
                        p.links.push((a, b));
                    }
                }
            }
            SimModel::Conductor { ohms } => {
                let ohms = prop(comp, "ohms", f64::from(*ohms)).max(1e-5);
                if let (Some(a), Some(b)) = (net_of_role(TerminalRole::A), net_of_role(TerminalRole::B)) {
                    p.elements.push(Element::r(a, b, 1.0 / ohms));
                    p.links.push((a, b));
                }
            }
            SimModel::Photoresistor { dark_ohms, bright_ohms } => {
                let level = match comp.state {
                    CompState::Fraction { value } => value.clamp(0.0, 1.0),
                    _ => 0.5,
                };
                let ohms = dark_ohms * (bright_ohms / dark_ohms).powf(level);
                if let (Some(a), Some(b)) = (net_of_role(TerminalRole::A), net_of_role(TerminalRole::B)) {
                    p.elements.push(Element::r(a, b, 1.0 / f64::from(ohms.max(1.0))));
                }
            }
            SimModel::Potentiometer { ohms } => {
                let ohms = prop(comp, "ohms", f64::from(*ohms));
                let f = match comp.state {
                    CompState::Fraction { value } => f64::from(value.clamp(0.0, 1.0)),
                    _ => 0.5,
                };
                if let (Some(a), Some(b), Some(w)) = (
                    net_of_role(TerminalRole::EndA),
                    net_of_role(TerminalRole::EndB),
                    net_of_role(TerminalRole::Wiper),
                ) {
                    p.elements.push(Element::r(a, w, 1.0 / (ohms * f).max(1.0)));
                    p.elements.push(Element::r(w, b, 1.0 / (ohms * (1.0 - f)).max(1.0)));
                }
            }
            SimModel::PushButton => {
                if pressed
                    && let (Some(a), Some(b)) =
                        (net_of_role(TerminalRole::A), net_of_role(TerminalRole::B))
                {
                    p.joins.push((a, b));
                }
            }
            SimModel::ToggleSwitch => {
                if matches!(comp.state, CompState::Toggle { on: true })
                    && let (Some(a), Some(b)) =
                        (net_of_role(TerminalRole::A), net_of_role(TerminalRole::B))
                {
                    p.joins.push((a, b));
                }
            }
            SimModel::SlideSwitchSpdt => {
                let on = matches!(comp.state, CompState::Toggle { on: true });
                let side = if on { TerminalRole::B } else { TerminalRole::A };
                if let (Some(c), Some(s)) = (net_of_role(TerminalRole::Common), net_of_role(side)) {
                    p.joins.push((c, s));
                }
            }
            SimModel::Led { forward_mv } => {
                if let (Some(a), Some(c)) =
                    (net_of_role(TerminalRole::Anode), net_of_role(TerminalRole::Cathode))
                {
                    p.leds.push(LedInst {
                        comp: comp.id,
                        anode: a,
                        cathode: c,
                        forward_mv: f64::from(*forward_mv),
                    });
                }
            }
            SimModel::Buzzer { .. } => {
                if let (Some(s), Some(g)) =
                    (net_of_role(TerminalRole::Signal), net_of_role(TerminalRole::Gnd))
                {
                    p.elements.push(Element::r(s, g, 1.0 / 1000.0));
                }
            }
            SimModel::RelayModule => {
                if let (Some(sig), Some(com), Some(no), Some(nc)) = (
                    net_of_role(TerminalRole::Signal),
                    net_of_role(TerminalRole::Common),
                    net_of_role(TerminalRole::NormallyOpen),
                    net_of_role(TerminalRole::NormallyClosed),
                ) {
                    p.relays.push(RelayInst { comp: comp.id, signal: sig, common: com, no, nc });
                }
            }
            SimModel::DigitalSensor => {
                if powered(nl, lib, circuit, comp.id)
                    && let Some(sig) = net_of_role(TerminalRole::Signal)
                {
                    let high = matches!(comp.state, CompState::Toggle { on: true });
                    p.fixed_extra.push((sig, if high { RAIL_MV } else { 0.0 }));
                }
            }
            SimModel::AnalogSensor { min_mv, max_mv } => {
                if powered(nl, lib, circuit, comp.id)
                    && let Some(sig) = net_of_role(TerminalRole::Signal)
                {
                    let f = match comp.state {
                        CompState::Fraction { value } => value.clamp(0.0, 1.0),
                        _ => 0.5,
                    };
                    let span = f32::from(max_mv.saturating_sub(*min_mv));
                    p.fixed_extra.push((sig, f32::from(*min_mv) + f * span));
                }
            }
            SimModel::DcSupply { volts, r_int_ohms, i_max_a } => {
                if let (Some(pos), Some(neg)) =
                    (net_of_role(TerminalRole::Positive), net_of_role(TerminalRole::Negative))
                {
                    p.supplies.push(SupplyInst {
                        comp: comp.id,
                        pos,
                        neg,
                        mv: prop(comp, "volts", f64::from(*volts)) * 1000.0,
                        r_int: f64::from(*r_int_ohms).max(1e-4),
                        i_max_a: prop(comp, "i_max_a", f64::from(*i_max_a)),
                    });
                }
            }
            SimModel::HvBoost { v_set, i_max_a, p_max_w, vin_min, vin_max, efficiency } => {
                if let (Some(inp), Some(inn), Some(outp), Some(outn)) = (
                    net_of_role(TerminalRole::InPos),
                    net_of_role(TerminalRole::InNeg),
                    net_of_role(TerminalRole::OutPos),
                    net_of_role(TerminalRole::OutNeg),
                ) {
                    p.boosts.push(BoostInst {
                        comp: comp.id,
                        inp,
                        inn,
                        outp,
                        outn,
                        v_set: prop(comp, "v_set", f64::from(*v_set)),
                        i_max_a: f64::from(*i_max_a),
                        p_max_w: f64::from(*p_max_w),
                        vin_min: f64::from(*vin_min),
                        vin_max: f64::from(*vin_max),
                        eff: f64::from(*efficiency).clamp(0.1, 1.0),
                    });
                }
            }
            SimModel::Capacitor { farads, v_rated, esr_ohms } => {
                if let (Some(pos), Some(neg)) =
                    (net_of_role(TerminalRole::Positive), net_of_role(TerminalRole::Negative))
                {
                    p.caps.push(CapInst {
                        comp: comp.id,
                        pos,
                        neg,
                        farads: prop(comp, "farads", f64::from(*farads)).max(1e-12),
                        esr: prop(comp, "esr_ohms", f64::from(*esr_ohms)).max(1e-4),
                        rated: f64::from(*v_rated),
                    });
                }
            }
            SimModel::Diode { forward_mv, on_ohms } => {
                if let (Some(a), Some(c)) =
                    (net_of_role(TerminalRole::Anode), net_of_role(TerminalRole::Cathode))
                {
                    p.diodes.push(DiodeInst {
                        comp: comp.id,
                        anode: a,
                        cathode: c,
                        vf_mv: f64::from(*forward_mv),
                        r_on: f64::from(*on_ohms).max(1e-3),
                    });
                }
            }
            SimModel::ArcContact { contact_ohms, v_arc } => {
                if let (Some(lead), Some(tip)) =
                    (net_of_role(TerminalRole::A), net_of_role(TerminalRole::B))
                {
                    p.arcs.push(ArcInst {
                        comp: comp.id,
                        lead,
                        tip,
                        r_contact: prop(comp, "contact_ohms", f64::from(*contact_ohms)).max(1e-4),
                        v_arc_mv: prop(comp, "v_arc", f64::from(*v_arc)) * 1000.0,
                        touching: pressed,
                    });
                }
            }
            SimModel::Servo | SimModel::Generic => {}
        }
    }
    p
}

struct Ctx<'a> {
    n: usize,
    nl: &'a Netlist,
    board: &'a BoardProfile,
    bank: &'a PinBank,
}

fn build_network(parts: &Parts, topo: &Topo, caps: &CapMode) -> (Vec<Element>, Vec<(usize, f64)>) {
    let mut elems = parts.elements.clone();
    let mut inj: Vec<(usize, f64)> = Vec::new();
    for l in &parts.leds {
        if topo.led_on.get(&l.comp).copied().unwrap_or(false) {
            elems.push(Element::r(l.anode, l.cathode, 1.0 / LED_ON_OHMS));
        }
    }
    for r in &parts.relays {
        let on = topo.relay_on.get(&r.comp).copied().unwrap_or(false);
        let contact = if on { r.no } else { r.nc };
        elems.push(Element::r(r.common, contact, 1.0 / CONTACT_OHMS));
    }
    for s in &parts.supplies {
        elems.push(Element { a: s.pos, b: s.neg, siemens: 1.0 / s.r_int, emf: s.mv });
    }
    if let CapMode::Companion { v, h } = caps {
        for c in &parts.caps {
            let vc = v.get(&c.comp).copied().unwrap_or(0.0);
            let r = c.esr + h / c.farads;
            elems.push(Element { a: c.pos, b: c.neg, siemens: 1.0 / r, emf: vc * 1000.0 });
        }
    }
    for d in &parts.diodes {
        if topo.diode_on.get(&d.comp).copied().unwrap_or(false) {
            elems.push(Element { a: d.anode, b: d.cathode, siemens: 1.0 / d.r_on, emf: d.vf_mv });
        }
    }
    for a in &parts.arcs {
        match topo.arc_dir.get(&a.comp).copied().unwrap_or(0) {
            1 => elems.push(Element { a: a.tip, b: a.lead, siemens: 1.0 / a.r_contact, emf: a.v_arc_mv }),
            -1 => elems.push(Element { a: a.lead, b: a.tip, siemens: 1.0 / a.r_contact, emf: a.v_arc_mv }),
            _ => {}
        }
    }
    for b in &parts.boosts {
        let d = topo.boost.get(&b.comp).copied().unwrap_or_default();
        match d.st {
            BoostSt::Thevenin => elems.push(Element {
                a: b.outp,
                b: b.outn,
                siemens: 1.0 / BOOST_REG_OHMS,
                emf: b.v_set * 1000.0,
            }),
            BoostSt::Source(ma) => {
                inj.push((b.outp, ma));
                inj.push((b.outn, -ma));
            }
            BoostSt::Off | BoostSt::Idle => {}
        }
        if d.i_in_ma != 0.0 {
            inj.push((b.inp, -d.i_in_ma));
            inj.push((b.inn, d.i_in_ma));
        }
    }
    (elems, inj)
}

/// Current limit of the supply feeding `net` through closed contacts and links, amps.
fn supply_limit(parts: &Parts, topo: &Topo, net: usize) -> f64 {
    let mut adj: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut link = |a: usize, b: usize| {
        adj.entry(a).or_default().push(b);
        adj.entry(b).or_default().push(a);
    };
    for &(a, b) in parts.links.iter().chain(&parts.joins) {
        link(a, b);
    }
    for r in &parts.relays {
        let on = topo.relay_on.get(&r.comp).copied().unwrap_or(false);
        link(r.common, if on { r.no } else { r.nc });
    }
    let mut seen: HashSet<usize> = HashSet::from([net]);
    let mut frontier = vec![net];
    while let Some(n) = frontier.pop() {
        if let Some(next) = adj.get(&n) {
            for &m in next {
                if seen.insert(m) {
                    frontier.push(m);
                }
            }
        }
    }
    parts
        .supplies
        .iter()
        .filter(|s| seen.contains(&s.pos))
        .map(|s| s.i_max_a)
        .fold(f64::INFINITY, f64::min)
}

fn volts_between(v: &[Option<f64>], a: usize, b: usize) -> Option<f64> {
    Some(v[a]? - v[b]?)
}

/// Re-evaluates discrete device states from a solution; true when any changed.
fn update_topo(parts: &Parts, topo: &mut Topo, v: &[Option<f64>], freeze_logic: bool) -> bool {
    let mut changed = false;
    if !freeze_logic {
        for l in &parts.leds {
            let on = volts_between(v, l.anode, l.cathode).is_some_and(|d| d > l.forward_mv - 400.0);
            if topo.led_on.insert(l.comp, on) != Some(on) {
                changed = true;
            }
        }
        for r in &parts.relays {
            let on = v[r.signal].is_some_and(|x| x > f64::from(LOGIC_THRESHOLD_MV));
            if topo.relay_on.insert(r.comp, on) != Some(on) {
                changed = true;
            }
        }
    }
    for d in &parts.diodes {
        let was = topo.diode_on.get(&d.comp).copied().unwrap_or(false);
        let across = volts_between(v, d.anode, d.cathode);
        let on = match across {
            None => false,
            Some(x) if was => (x - d.vf_mv) / d.r_on >= -1e-6,
            Some(x) => x > d.vf_mv,
        };
        if on != was {
            topo.diode_on.insert(d.comp, on);
            changed = true;
        }
    }
    for a in &parts.arcs {
        let was = topo.arc_dir.get(&a.comp).copied().unwrap_or(0);
        let across = volts_between(v, a.tip, a.lead);
        let dir = match across {
            Some(x) if a.touching => {
                if was == 0 {
                    if x > a.v_arc_mv {
                        1
                    } else if x < -a.v_arc_mv {
                        -1
                    } else {
                        0
                    }
                } else if (f64::from(was) * x - a.v_arc_mv) / a.r_contact >= 0.0 {
                    was
                } else {
                    0
                }
            }
            _ => 0,
        };
        if dir != was {
            topo.arc_dir.insert(a.comp, dir);
            changed = true;
        }
    }
    for b in &parts.boosts {
        let mut d = topo.boost.get(&b.comp).copied().unwrap_or_default();
        let before = d;
        let vin = volts_between(v, b.inp, b.inn).unwrap_or(0.0) / 1000.0;
        let vout_mv = volts_between(v, b.outp, b.outn).unwrap_or(0.0);
        let vout = vout_mv / 1000.0;
        d.over_vin = vin > b.vin_max;
        let enabled = vin >= b.vin_min && !d.over_vin;
        if !enabled {
            d = BoostDyn { over_vin: d.over_vin, ..BoostDyn::default() };
        } else {
            let src_a = supply_limit(parts, topo, b.inp);
            let p_avail = b.p_max_w.min(b.eff * vin * src_a);
            let i_lim_by_p = p_avail / vout.max(1.0);
            d.power_limited = i_lim_by_p < b.i_max_a;
            let i_lim_ma = b.i_max_a.min(i_lim_by_p) * 1000.0;
            let set_mv = b.v_set * 1000.0;
            let i_thev = (set_mv - vout_mv) / BOOST_REG_OHMS;
            d.st = match d.st {
                BoostSt::Off | BoostSt::Thevenin => {
                    if i_thev > i_lim_ma * 1.0005 {
                        BoostSt::Source(i_lim_ma)
                    } else if i_thev < -1e-3 {
                        BoostSt::Idle
                    } else {
                        BoostSt::Thevenin
                    }
                }
                BoostSt::Source(prev) => {
                    if vout_mv >= set_mv {
                        BoostSt::Thevenin
                    } else if (prev - i_lim_ma).abs() > 1e-4 * i_lim_ma + 1e-3 {
                        BoostSt::Source(i_lim_ma)
                    } else {
                        BoostSt::Source(prev)
                    }
                }
                BoostSt::Idle => {
                    if vout_mv < set_mv - 1.0 { BoostSt::Thevenin } else { BoostSt::Idle }
                }
            };
            d.i_out_ma = match d.st {
                BoostSt::Thevenin => i_thev.clamp(0.0, i_lim_ma),
                BoostSt::Source(ma) => ma,
                _ => 0.0,
            };
            let p_out = vout.max(0.0) * d.i_out_ma / 1000.0;
            d.i_in_ma = p_out / b.eff / vin.max(0.5) * 1000.0;
        }
        let moved = d.st != before.st
            || (d.i_in_ma - before.i_in_ma).abs() > 1e-4 * before.i_in_ma.abs() + 1e-3;
        topo.boost.insert(b.comp, d);
        if moved {
            changed = true;
        }
    }
    changed
}

/// Iterates the network until every discrete state is consistent.
fn settle(
    ctx: &Ctx,
    parts: &Parts,
    topo: &mut Topo,
    caps: &CapMode,
    peak: bool,
    freeze_logic: bool,
    warnings: &mut Vec<String>,
) -> (Vec<Option<f64>>, Vec<Element>) {
    let mut v = Vec::new();
    let mut elems = Vec::new();
    for _ in 0..SETTLE_ITERS {
        let (e, inj) = build_network(parts, topo, caps);
        let mut w = Vec::new();
        v = relax(ctx, &e, &parts.fixed_extra, &parts.joins, &inj, peak, &mut w);
        *warnings = w;
        elems = e;
        if !update_topo(parts, topo, &v, freeze_logic) {
            break;
        }
    }
    (v, elems)
}

/// One DC solve: capacitors open, no time advance.
pub fn solve(
    circuit: &Circuit,
    board: &BoardProfile,
    lib: &Library,
    nl: &Netlist,
    bank: &PinBank,
) -> SimOutput {
    let parts = collect(circuit, lib, nl);
    let ctx = Ctx { n: nl.nets.len(), nl, board, bank };
    let mut topo = Topo::default();
    finish(&ctx, circuit, lib, &parts, &mut topo, &CapMode::Open, None)
}

/// Advances the transient simulation by `dt_s` seconds and reports the end state.
///
/// Capacitors integrate with backward Euler; substeps shrink to microseconds while an
/// electrode is striking so the discharge pulse is resolved.
pub fn step(
    circuit: &Circuit,
    board: &BoardProfile,
    lib: &Library,
    nl: &Netlist,
    bank: &PinBank,
    state: &mut PowerState,
    dt_s: f64,
) -> SimOutput {
    let parts = collect(circuit, lib, nl);
    let ctx = Ctx { n: nl.nets.len(), nl, board, bank };
    state.caps.retain(|id, _| parts.caps.iter().any(|c| c.comp == *id));
    for c in &parts.caps {
        state.caps.entry(c.comp).or_insert(0.0);
    }
    let mut topo = state.topo.take().unwrap_or_default();
    let mut scratch = Vec::new();
    settle(&ctx, &parts, &mut topo, &CapMode::Companion { v: &state.caps, h: BASE_DT }, true, false, &mut scratch);

    let mut remaining = dt_s.clamp(0.0, MAX_FRAME_S);
    let mut steps = 0;
    while remaining > 1e-9 && steps < MAX_SUBSTEPS {
        steps += 1;
        let mut fast = false;
        for a in &parts.arcs {
            let run = state.runs.entry(a.comp).or_default();
            if a.touching && !run.was_touching {
                run.touch_age_s = 0.0;
            }
            run.was_touching = a.touching;
            if a.touching && (run.touch_age_s < ARC_WINDOW_S || (run.active && run.peak_ma > 2000.0)) {
                fast = true;
            }
        }
        let h = if fast { ARC_DT } else { BASE_DT }.min(remaining);
        let (v, _) = settle(
            &ctx,
            &parts,
            &mut topo,
            &CapMode::Companion { v: &state.caps, h },
            false,
            true,
            &mut scratch,
        );
        for c in &parts.caps {
            let vc = state.caps[&c.comp];
            let Some(across_mv) = volts_between(&v, c.pos, c.neg) else { continue };
            let i_a = (across_mv / 1000.0 - vc) / (c.esr + h / c.farads);
            state.caps.insert(c.comp, vc + i_a * h / c.farads);
        }
        for a in &parts.arcs {
            let dir = topo.arc_dir.get(&a.comp).copied().unwrap_or(0);
            let run = state.runs.entry(a.comp).or_default();
            if a.touching {
                run.touch_age_s += h;
            }
            let across = volts_between(&v, a.tip, a.lead).unwrap_or(0.0) * f64::from(dir);
            let i_ma = if dir != 0 { ((across - a.v_arc_mv) / a.r_contact).max(0.0) } else { 0.0 };
            if i_ma > 1.0 {
                if !run.active {
                    run.active = true;
                    run.peak_ma = 0.0;
                    run.joules = 0.0;
                    run.width_s = 0.0;
                    run.dcen = dir > 0;
                }
                run.peak_ma = run.peak_ma.max(i_ma);
                run.joules += across / 1000.0 * i_ma / 1000.0 * h;
                if i_ma >= 0.1 * run.peak_ma {
                    run.width_s += h;
                }
            } else if run.active {
                run.active = false;
                // A trickle through a stuck contact is not a weld pulse.
                if run.joules > 1e-4 && run.peak_ma >= 5000.0 {
                    let pulse = ArcPulse {
                        peak_a: (run.peak_ma / 1000.0) as f32,
                        joules: run.joules as f32,
                        width_us: (run.width_s * 1e6) as f32,
                        dcen: run.dcen,
                    };
                    state.pulses.insert(a.comp, (pulse, state.time_s + h));
                }
            }
        }
        state.time_s += h;
        remaining -= h;
    }

    let out = finish(
        &ctx,
        circuit,
        lib,
        &parts,
        &mut topo,
        &CapMode::Companion { v: &state.caps, h: 1e-6 },
        Some(state),
    );
    state.topo = Some(topo);
    out
}

/// Peak and averaged passes, pin readings, visuals and electrical warnings.
fn finish(
    ctx: &Ctx,
    circuit: &Circuit,
    lib: &Library,
    parts: &Parts,
    topo: &mut Topo,
    caps: &CapMode,
    state: Option<&PowerState>,
) -> SimOutput {
    let (nl, board, bank) = (ctx.nl, ctx.board, ctx.bank);
    let n = ctx.n;
    let mut out = SimOutput {
        net_peak_mv: vec![None; n],
        net_avg_mv: vec![None; n],
        ..SimOutput::default()
    };

    let mut peak_warn = Vec::new();
    let (peak, _) = settle(ctx, parts, topo, caps, true, false, &mut peak_warn);
    out.warnings = peak_warn;
    let mut avg_warn = Vec::new();
    let (avg, elems) = settle(ctx, parts, topo, caps, false, true, &mut avg_warn);
    out.warnings.extend(avg_warn);

    let led_on = |id: CompId| topo.led_on.get(&id).copied().unwrap_or(false);
    let relay_on = |id: CompId| topo.relay_on.get(&id).copied().unwrap_or(false);

    // Estimated per-GPIO source current (mV x S = mA).
    for pin in &board.pins {
        let Some(gpio) = pin.kind.gpio() else { continue };
        if !bank.get(gpio).mode.is_output() {
            continue;
        }
        let Some(net) = nl.net_of(&Endpoint::BoardPin { key: pin.key.clone() }) else {
            continue;
        };
        let Some(v) = avg[net] else { continue };
        let mut ma = 0.0f64;
        for e in &elems {
            if e.a == net {
                ma += (v - avg[e.b].unwrap_or(v) - e.emf) * e.siemens;
            } else if e.b == net {
                ma += (v - avg[e.a].unwrap_or(v) + e.emf) * e.siemens;
            }
        }
        let ma = ma as f32;
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
    // LEDs: a real diode has almost no resistance past its forward drop.
    for l in &parts.leds {
        if !led_on(l.comp) {
            continue;
        }
        let Some(across) = volts_between(&avg, l.anode, l.cathode) else { continue };
        let est_ma = (across - l.forward_mv).max(0.0) / 25.0;
        if est_ma > 20.0 {
            let name = comp_name(circuit, lib, l.comp);
            out.warnings.push(format!(
                "{name}: a real LED here would pass ~{est_ma:.0} mA (rating ~20 mA) — it needs more series resistance"
            ));
        }
    }
    // Resistor dissipation and working voltage.
    for r in &parts.resistors {
        let Some(across) = volts_between(&avg, r.a, r.b) else { continue };
        let volts = across.abs() / 1000.0;
        let watts = volts * volts / r.ohms;
        let name = comp_name(circuit, lib, r.comp);
        if watts > r.watts {
            out.warnings.push(format!(
                "{name}: dissipates {watts:.2} W, above its {:.2} W rating",
                r.watts
            ));
        }
        if volts > r.v_max {
            out.warnings.push(format!(
                "{name}: {volts:.0} V across it, above its {:.0} V working-voltage rating",
                r.v_max
            ));
        }
    }

    // Pin readings and pin overvoltage.
    for pin in &board.pins {
        let Some(net) = nl.net_of(&Endpoint::BoardPin { key: pin.key.clone() }) else { continue };
        if let Some(mv) = avg[net] {
            let limit = match pin.kind {
                crate::board::PinKind::Gpio(_) | crate::board::PinKind::V3_3 => 3600.0,
                crate::board::PinKind::V5 => 5500.0,
                _ => f64::INFINITY,
            };
            if mv > limit + 1.0 || mv < -300.0 {
                out.warnings.push(format!(
                    "board pin {} sees {:.2} V — outside its {:.1} V absolute maximum",
                    pin.label,
                    mv / 1000.0,
                    limit / 1000.0
                ));
            }
        }
        let Some(gpio) = pin.kind.gpio() else { continue };
        let drive = bank.get(gpio);
        if drive.mode.is_input() {
            let level = match avg[net] {
                Some(v) => v > f64::from(LOGIC_THRESHOLD_MV),
                None => matches!(drive.mode, PinMode::InputPullUp),
            };
            out.digital.insert(gpio, level);
        } else if drive.mode == PinMode::Analog {
            let mv = avg[net].unwrap_or(0.0).clamp(0.0, 3300.0);
            out.analog_mv.insert(gpio, mv.round() as u16);
        } else if drive.mode.is_output() {
            let level = match drive.mode {
                PinMode::Pwm => drive.duty > 0.5,
                _ => drive.out_high,
            };
            out.digital.insert(gpio, level);
        }
    }

    // Power-stage readings and warnings.
    let cap_volts = |c: &CapInst| -> f64 {
        match state {
            Some(s) => s.caps.get(&c.comp).copied().unwrap_or(0.0),
            None => volts_between(&avg, c.pos, c.neg).unwrap_or(0.0) / 1000.0,
        }
    };
    for c in &parts.caps {
        let volts = cap_volts(c);
        out.visuals.insert(c.comp, VisualState::Capacitor { volts: volts as f32, rated: c.rated as f32 });
        if volts > c.rated {
            out.warnings.push(format!(
                "{}: {volts:.0} V exceeds its {:.0} V rating",
                comp_name(circuit, lib, c.comp),
                c.rated
            ));
        } else if volts < -1.5 {
            out.warnings.push(format!(
                "{}: reverse-charged to {volts:.1} V — polarized capacitor wired backwards",
                comp_name(circuit, lib, c.comp)
            ));
        }
    }
    for s in &parts.supplies {
        let vout = volts_between(&avg, s.pos, s.neg).unwrap_or(0.0);
        let amps = (s.mv - vout) / s.r_int / 1000.0;
        out.visuals.insert(
            s.comp,
            VisualState::Supply { volts: (vout / 1000.0) as f32, amps: amps as f32 },
        );
        if amps > s.i_max_a * 1.02 {
            out.warnings.push(format!(
                "{}: drawing {amps:.1} A, above its {:.1} A rating",
                comp_name(circuit, lib, s.comp),
                s.i_max_a
            ));
        }
    }
    let mut feeding: Vec<String> = Vec::new();
    for b in &parts.boosts {
        let d = topo.boost.get(&b.comp).copied().unwrap_or_default();
        let vout = volts_between(&avg, b.outp, b.outn).unwrap_or(0.0) / 1000.0;
        let mode = match d.st {
            BoostSt::Off => BoostMode::Off,
            BoostSt::Idle | BoostSt::Thevenin => BoostMode::Regulating,
            BoostSt::Source(_) if d.power_limited => BoostMode::ConstantPower,
            BoostSt::Source(_) => BoostMode::ConstantCurrent,
        };
        out.visuals.insert(
            b.comp,
            VisualState::Boost { mode, volts: vout as f32, amps: (d.i_out_ma / 1000.0) as f32 },
        );
        if d.over_vin {
            out.warnings.push(format!(
                "{}: input above its {:.0} V maximum",
                comp_name(circuit, lib, b.comp),
                b.vin_max
            ));
        }
        if d.i_out_ma > 1.0 {
            feeding.push(comp_name(circuit, lib, b.comp));
        }
    }
    for d in &parts.diodes {
        let on = topo.diode_on.get(&d.comp).copied().unwrap_or(false);
        let amps = if on {
            volts_between(&avg, d.anode, d.cathode)
                .map(|x| ((x - d.vf_mv) / d.r_on / 1000.0).max(0.0))
                .unwrap_or(0.0)
        } else {
            0.0
        };
        out.visuals.insert(d.comp, VisualState::Diode { conducting: on, amps: amps as f32 });
    }
    for a in &parts.arcs {
        let dir = topo.arc_dir.get(&a.comp).copied().unwrap_or(0);
        let amps = if dir != 0 {
            volts_between(&avg, a.tip, a.lead)
                .map(|x| ((x * f64::from(dir) - a.v_arc_mv) / a.r_contact / 1000.0).max(0.0))
                .unwrap_or(0.0)
        } else {
            0.0
        };
        let (pulse, since) = match state.and_then(|s| s.pulses.get(&a.comp).map(|p| (p, s.time_s))) {
            Some(((p, t_end), now)) => (Some(*p), (now - t_end).max(0.0) as f32),
            None => (None, f32::INFINITY),
        };
        out.visuals.insert(a.comp, VisualState::Arc { amps: amps as f32, pulse, since_pulse_s: since });
        let name = comp_name(circuit, lib, a.comp);
        if a.touching && !feeding.is_empty() {
            out.warnings.push(format!(
                "{name} touches the work while {} is charging — open the relay before welding (re-arc / sticking)",
                feeding.join(", ")
            ));
        }
        if let Some(p) = pulse
            && !p.dcen
            && since < 5.0
        {
            out.warnings.push(format!(
                "{name}: last pulse ran electrode-positive (DCEP); the electrode should be on the negative side"
            ));
        }
    }

    // Component visuals for the logic-level parts.
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
                let base = if led_on(comp.id) {
                    let across = volts_between(&peak, a, c).unwrap_or(0.0);
                    ((across / LED_ON_OHMS) / 8.0).clamp(0.0, 1.0) as f32
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
                let across = volts_between(&peak, s, g).unwrap_or(0.0);
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
            SimModel::RelayModule => VisualState::RelayClosed(relay_on(comp.id)),
            SimModel::DcSupply { .. }
            | SimModel::HvBoost { .. }
            | SimModel::Capacitor { .. }
            | SimModel::Diode { .. }
            | SimModel::ArcContact { .. } => {
                out.visuals.entry(comp.id).or_insert(VisualState::Inert);
                continue;
            }
            _ => VisualState::Inert,
        };
        out.visuals.insert(comp.id, vis);
    }

    let to_f32 = |v: &[Option<f64>]| v.iter().map(|x| x.map(|y| y as f32)).collect::<Vec<_>>();
    out.net_peak_mv = to_f32(&peak);
    out.net_avg_mv = to_f32(&avg);
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

/// Direct nodal solve over supernodes; returns per-net potentials in mV.
#[allow(clippy::too_many_arguments)]
fn relax(
    ctx: &Ctx,
    elements: &[Element],
    fixed_extra: &[(usize, f32)],
    joins: &[(usize, usize)],
    injections: &[(usize, f64)],
    peak: bool,
    warnings: &mut Vec<String>,
) -> Vec<Option<f64>> {
    let (n, nl, board, bank) = (ctx.n, ctx.nl, ctx.board, ctx.bank);
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
    let mut fixed: HashMap<usize, f64> = HashMap::new();
    let mut set_fixed = |fixed: &mut HashMap<usize, f64>, node: usize, mv: f64, what: &str| {
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
    let mut pulls: Vec<Pull> = Vec::new();
    for pin in &board.pins {
        let Some(gpio) = pin.kind.gpio() else { continue };
        let Some(net) = nl.net_of(&Endpoint::BoardPin { key: pin.key.clone() }) else { continue };
        let node = root[net];
        let d = bank.get(gpio);
        let rail = f64::from(RAIL_MV);
        match d.mode {
            PinMode::Output => {
                let mv = if d.out_high { rail } else { 0.0 };
                set_fixed(&mut fixed, node, mv, &format!("GPIO{gpio} output"));
            }
            PinMode::OutputOpenDrain => {
                if !d.out_high {
                    set_fixed(&mut fixed, node, 0.0, &format!("GPIO{gpio} open-drain"));
                }
            }
            PinMode::Pwm => {
                let mv = if peak {
                    if d.duty > 0.0 { rail } else { 0.0 }
                } else {
                    rail * f64::from(d.duty)
                };
                set_fixed(&mut fixed, node, mv, &format!("GPIO{gpio} PWM"));
            }
            PinMode::InputPullUp => {
                pulls.push(Pull { net: node, mv: rail, siemens: 1.0 / f64::from(PULL_OHMS) })
            }
            PinMode::InputPullDown => {
                pulls.push(Pull { net: node, mv: 0.0, siemens: 1.0 / f64::from(PULL_OHMS) })
            }
            _ => {}
        }
    }
    for &(net, mv) in fixed_extra {
        set_fixed(&mut fixed, root[net], f64::from(mv), "sensor output");
    }

    // Reachability: a node participates when elements connect it to a source.
    let mut adj: HashMap<usize, Vec<usize>> = HashMap::new();
    for e in elements {
        let (a, b) = (root[e.a], root[e.b]);
        if a == b {
            continue;
        }
        adj.entry(a).or_default().push(b);
        adj.entry(b).or_default().push(a);
    }
    let mut grounded: HashSet<usize> = fixed.keys().copied().collect();
    grounded.extend(pulls.iter().map(|p| p.net));
    let mut frontier: Vec<usize> = grounded.iter().copied().collect();
    while let Some(node) = frontier.pop() {
        if let Some(nbrs) = adj.get(&node) {
            for &other in nbrs {
                if grounded.insert(other) {
                    frontier.push(other);
                }
            }
        }
    }

    let mut unknown: Vec<usize> = grounded.iter().filter(|k| !fixed.contains_key(k)).copied().collect();
    unknown.sort_unstable();
    let idx: HashMap<usize, usize> = unknown.iter().enumerate().map(|(i, &k)| (k, i)).collect();
    let m = unknown.len();
    let mut a = vec![0.0f64; m * m];
    let mut rhs = vec![0.0f64; m];
    for e in elements {
        let (ra, rb) = (root[e.a], root[e.b]);
        if ra == rb || !grounded.contains(&ra) {
            continue;
        }
        let g = e.siemens;
        let ia = idx.get(&ra).copied();
        let ib = idx.get(&rb).copied();
        let va = fixed.get(&ra).copied().unwrap_or(0.0);
        let vb = fixed.get(&rb).copied().unwrap_or(0.0);
        if let Some(i) = ia {
            a[i * m + i] += g;
            rhs[i] += g * e.emf;
            match ib {
                Some(j) => a[i * m + j] -= g,
                None => rhs[i] += g * vb,
            }
        }
        if let Some(j) = ib {
            a[j * m + j] += g;
            rhs[j] -= g * e.emf;
            match ia {
                Some(i) => a[j * m + i] -= g,
                None => rhs[j] += g * va,
            }
        }
    }
    for p in &pulls {
        if let Some(&i) = idx.get(&p.net) {
            a[i * m + i] += p.siemens;
            rhs[i] += p.siemens * p.mv;
        }
    }
    for &(net, ma) in injections {
        if let Some(&i) = idx.get(&root[net]) {
            rhs[i] += ma;
        }
    }
    let x = gauss(&mut a, &mut rhs, m);

    let mut v: HashMap<usize, f64> = fixed.clone();
    for (i, &node) in unknown.iter().enumerate() {
        v.insert(node, x[i]);
    }
    (0..n)
        .map(|i| {
            let node = root[i];
            if grounded.contains(&node) { v.get(&node).copied() } else { None }
        })
        .collect()
}

/// Gaussian elimination with partial pivoting on a dense row-major system.
fn gauss(a: &mut [f64], b: &mut [f64], m: usize) -> Vec<f64> {
    for col in 0..m {
        let mut piv = col;
        for r in col + 1..m {
            if a[r * m + col].abs() > a[piv * m + col].abs() {
                piv = r;
            }
        }
        if a[piv * m + col].abs() < 1e-300 {
            continue;
        }
        if piv != col {
            for k in 0..m {
                a.swap(col * m + k, piv * m + k);
            }
            b.swap(col, piv);
        }
        let d = a[col * m + col];
        for r in col + 1..m {
            let f = a[r * m + col] / d;
            if f == 0.0 {
                continue;
            }
            for k in col..m {
                a[r * m + k] -= f * a[col * m + k];
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = vec![0.0f64; m];
    for col in (0..m).rev() {
        let d = a[col * m + col];
        if d.abs() < 1e-300 {
            continue;
        }
        let mut s = b[col];
        for k in col + 1..m {
            s -= a[col * m + k] * x[k];
        }
        x[col] = s / d;
    }
    x
}
