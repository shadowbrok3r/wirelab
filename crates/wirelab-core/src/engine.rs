//! Live session planning and the rules-engine runtime.
//!
//! `plan_setup` derives pin configuration straight from the wiring; `Engine`
//! turns device events into rule firings and host commands.

use std::collections::{HashMap, VecDeque};

use wirelab_proto::{Behavior, DeviceMsg, EventEdge, HostMsg, PinMode};

use crate::board::BoardProfile;
use crate::circuit::{Circuit, CompId};
use crate::component::{SimModel, TerminalRole};
use crate::library::Library;
use crate::netlist::Netlist;
use crate::program::{Action, Program, Rule, Trigger};
use crate::sim::PinBank;

pub const TELEMETRY_MS: u16 = 50;
pub const ANALOG_WATCH_MS: u16 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutKind {
    Led,
    Buzzer { active: bool },
    Relay,
    Servo,
    Generic,
}

#[derive(Debug, Clone, Copy)]
pub struct OutBinding {
    pub gpio: u8,
    pub active_high: bool,
    pub kind: OutKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InKind {
    Button,
    Toggle,
    DigitalSensor,
}

#[derive(Debug, Clone, Copy)]
pub struct InBinding {
    pub comp: CompId,
    pub active_low: bool,
    pub kind: InKind,
}

/// How placed components map onto GPIOs, derived from the netlist.
#[derive(Debug, Clone, Default)]
pub struct Bindings {
    pub outputs: HashMap<CompId, OutBinding>,
    pub inputs: HashMap<u8, InBinding>,
    pub analog: HashMap<CompId, u8>,
    pub warnings: Vec<String>,
}

impl Bindings {
    pub fn input_for_comp(&self, comp: CompId) -> Option<(u8, InBinding)> {
        self.inputs.iter().find(|(_, b)| b.comp == comp).map(|(g, b)| (*g, *b))
    }

    /// Primary GPIO associated with a component, for display.
    pub fn gpio_of(&self, comp: CompId) -> Option<u8> {
        self.outputs
            .get(&comp)
            .map(|b| b.gpio)
            .or_else(|| self.analog.get(&comp).copied())
            .or_else(|| self.input_for_comp(comp).map(|(g, _)| g))
    }
}

/// Derive bindings and the setup command sequence from the wiring itself.
pub fn plan_setup(
    circuit: &Circuit,
    board: &BoardProfile,
    lib: &Library,
    nl: &Netlist,
) -> (Vec<HostMsg>, Bindings) {
    let mut b = Bindings::default();
    let mut modes: HashMap<u8, PinMode> = HashMap::new();
    let claim = |b: &mut Bindings, modes: &mut HashMap<u8, PinMode>, gpio: u8, mode: PinMode, who: &str| {
        if let Some(prev) = modes.get(&gpio)
            && *prev != mode {
                b.warnings.push(format!(
                    "GPIO{gpio} claimed as {mode:?} by {who} but already {prev:?}"
                ));
                return false;
            }
        modes.insert(gpio, mode);
        true
    };

    let role_net = |comp: CompId, role: TerminalRole| -> Option<usize> {
        let def = lib.component(&circuit.components.get(&comp)?.def_id)?;
        let t = def.terminal_by_role(role)?;
        nl.net_of(&crate::circuit::Endpoint::Terminal { comp, terminal: t.id.clone() })
    };

    for comp in circuit.components.values() {
        let Some(def) = lib.component(&comp.def_id) else { continue };
        let name = if comp.label.is_empty() { def.name.clone() } else { comp.label.clone() };
        match &def.sim {
            SimModel::Led { .. } => {
                let a = role_net(comp.id, TerminalRole::Anode);
                let c = role_net(comp.id, TerminalRole::Cathode);
                let (Some(a), Some(c)) = (a, c) else { continue };
                let ga = nl.reach_gpios(a).to_vec();
                let gc = nl.reach_gpios(c).to_vec();
                let pick = if !ga.is_empty() && nl.reach_rails(c).gnd {
                    Some((ga[0], true))
                } else if !gc.is_empty() && (nl.reach_rails(a).v33 || nl.reach_rails(a).v5) {
                    Some((gc[0], false))
                } else if !ga.is_empty() {
                    b.warnings.push(format!("{name}: cathode has no path to GND"));
                    Some((ga[0], true))
                } else if !gc.is_empty() {
                    b.warnings.push(format!("{name}: anode has no path to a supply"));
                    Some((gc[0], false))
                } else {
                    b.warnings.push(format!("{name}: not connected to any GPIO"));
                    None
                };
                if let Some((gpio, active_high)) = pick
                    && claim(&mut b, &mut modes, gpio, PinMode::Output, &name) {
                        b.outputs
                            .insert(comp.id, OutBinding { gpio, active_high, kind: OutKind::Led });
                    }
            }
            SimModel::Buzzer { active } => {
                let Some(s) = role_net(comp.id, TerminalRole::Signal) else { continue };
                if let Some(&gpio) = nl.reach_gpios(s).first()
                    && claim(&mut b, &mut modes, gpio, PinMode::Output, &name) {
                        b.outputs.insert(
                            comp.id,
                            OutBinding {
                                gpio,
                                active_high: true,
                                kind: OutKind::Buzzer { active: *active },
                            },
                        );
                    }
            }
            SimModel::RelayModule => {
                let Some(s) = role_net(comp.id, TerminalRole::Signal) else { continue };
                if let Some(&gpio) = nl.reach_gpios(s).first()
                    && claim(&mut b, &mut modes, gpio, PinMode::Output, &name) {
                        b.outputs.insert(
                            comp.id,
                            OutBinding { gpio, active_high: true, kind: OutKind::Relay },
                        );
                    }
            }
            SimModel::Servo => {
                let Some(s) = role_net(comp.id, TerminalRole::Signal) else { continue };
                if let Some(&gpio) = nl.reach_gpios(s).first()
                    && claim(&mut b, &mut modes, gpio, PinMode::Pwm, &name) {
                        b.outputs.insert(
                            comp.id,
                            OutBinding { gpio, active_high: true, kind: OutKind::Servo },
                        );
                    }
            }
            SimModel::PushButton | SimModel::ToggleSwitch => {
                let a = role_net(comp.id, TerminalRole::A);
                let bb = role_net(comp.id, TerminalRole::B);
                let (Some(a), Some(bb)) = (a, bb) else { continue };
                let (sense, other) = if !nl.nets[a].gpios.is_empty() { (a, bb) } else { (bb, a) };
                let Some(&gpio) = nl.nets[sense].gpios.first() else {
                    b.warnings.push(format!("{name}: not connected to any GPIO"));
                    continue;
                };
                let rails = nl.reach_rails(other);
                let (mode, active_low) = if rails.gnd {
                    (PinMode::InputPullUp, true)
                } else if rails.v33 || rails.v5 {
                    (PinMode::InputPullDown, false)
                } else {
                    b.warnings.push(format!("{name}: far side reaches no rail, assuming GND"));
                    (PinMode::InputPullUp, true)
                };
                if claim(&mut b, &mut modes, gpio, mode, &name) {
                    let kind = if matches!(def.sim, SimModel::PushButton) {
                        InKind::Button
                    } else {
                        InKind::Toggle
                    };
                    b.inputs.insert(gpio, InBinding { comp: comp.id, active_low, kind });
                }
            }
            SimModel::SlideSwitchSpdt => {
                let Some(c) = role_net(comp.id, TerminalRole::Common) else { continue };
                let Some(&gpio) = nl.nets[c].gpios.first() else { continue };
                let b_side = role_net(comp.id, TerminalRole::B);
                let active_low = b_side.map(|s| nl.reach_rails(s).gnd).unwrap_or(false);
                let mode = if active_low { PinMode::InputPullUp } else { PinMode::InputPullDown };
                if claim(&mut b, &mut modes, gpio, mode, &name) {
                    b.inputs
                        .insert(gpio, InBinding { comp: comp.id, active_low, kind: InKind::Toggle });
                }
            }
            SimModel::DigitalSensor => {
                let Some(s) = role_net(comp.id, TerminalRole::Signal) else { continue };
                if let Some(&gpio) = nl.nets[s].gpios.first()
                    && claim(&mut b, &mut modes, gpio, PinMode::Input, &name) {
                        b.inputs.insert(
                            gpio,
                            InBinding {
                                comp: comp.id,
                                active_low: false,
                                kind: InKind::DigitalSensor,
                            },
                        );
                    }
            }
            SimModel::Potentiometer { .. } => {
                let Some(w) = role_net(comp.id, TerminalRole::Wiper) else { continue };
                if let Some(&gpio) = nl.nets[w].gpios.first()
                    && claim(&mut b, &mut modes, gpio, PinMode::Analog, &name) {
                        b.analog.insert(comp.id, gpio);
                    }
            }
            SimModel::Photoresistor { .. } => {
                for role in [TerminalRole::A, TerminalRole::B] {
                    if let Some(net) = role_net(comp.id, role)
                        && let Some(&gpio) = nl.nets[net].gpios.first() {
                            if claim(&mut b, &mut modes, gpio, PinMode::Analog, &name) {
                                b.analog.insert(comp.id, gpio);
                            }
                            break;
                        }
                }
            }
            SimModel::AnalogSensor { .. } => {
                let Some(s) = role_net(comp.id, TerminalRole::Signal) else { continue };
                if let Some(&gpio) = nl.nets[s].gpios.first()
                    && claim(&mut b, &mut modes, gpio, PinMode::Analog, &name) {
                        b.analog.insert(comp.id, gpio);
                    }
            }
            SimModel::Resistor { .. } | SimModel::Generic => {}
        }
    }

    // Board sanity: warn when something claims an input-only pin as output.
    for (&gpio, &mode) in &modes {
        if mode.is_output()
            && let Some(pin) = board.gpio_pin(gpio)
                && pin.caps.contains(crate::board::PinCaps::INPUT_ONLY) {
                    b.warnings.push(format!("GPIO{gpio} is input-only but wired as an output"));
                }
    }

    let mut msgs = vec![HostMsg::Reset];
    let mut ordered: Vec<_> = modes.iter().collect();
    ordered.sort_by_key(|(gpio, _)| **gpio);
    for (&gpio, &mode) in ordered {
        msgs.push(HostMsg::SetPinMode { pin: gpio, mode });
        if mode == PinMode::Analog {
            msgs.push(HostMsg::WatchAnalog { pin: gpio, interval_ms: ANALOG_WATCH_MS });
        }
    }
    msgs.push(HostMsg::SetTelemetry { interval_ms: TELEMETRY_MS });
    (msgs, b)
}

#[derive(Debug)]
struct PendingActions {
    due_ms: u64,
    rule_idx: usize,
    actions: VecDeque<Action>,
}

/// One rule firing, kept for GUI highlighting.
#[derive(Debug, Clone, Copy)]
pub struct Firing {
    pub rule_idx: usize,
    pub at_ms: u64,
}

/// Sentinel rule index for actions injected by component scripts.
pub const SCRIPT_RULE_IDX: usize = usize::MAX;

#[derive(Debug, Default)]
pub struct Engine {
    pub program: Program,
    pub bindings: Bindings,
    out_shadow: PinBank,
    pending: Vec<PendingActions>,
    every_due: HashMap<usize, u64>,
    analog_latch: HashMap<usize, bool>,
    behavior_slots: [Option<CompId>; wirelab_proto::BEHAVIOR_SLOTS],
    pub running: bool,
    pub firings: VecDeque<Firing>,
    pub log: Vec<String>,
}

impl Engine {
    pub fn new(program: Program, bindings: Bindings) -> Self {
        Engine { program, bindings, ..Engine::default() }
    }

    pub fn set_bindings(&mut self, bindings: Bindings) {
        self.bindings = bindings;
    }

    /// Begin running: fires `OnStart` rules and schedules `Every` rules.
    /// Script continuations survive; rule continuations reset.
    pub fn start(&mut self, now_ms: u64) -> Vec<HostMsg> {
        self.running = true;
        self.pending.retain(|p| p.rule_idx == SCRIPT_RULE_IDX);
        self.every_due.clear();
        self.analog_latch.clear();
        let mut out = Vec::new();
        for i in 0..self.program.rules.len() {
            let rule = self.program.rules[i].clone();
            if !rule.enabled {
                continue;
            }
            match rule.trigger {
                Trigger::OnStart => self.fire(i, &rule, now_ms, &mut out),
                Trigger::Every { ms } => {
                    self.every_due.insert(i, now_ms + u64::from(ms.max(10)));
                }
                _ => {}
            }
        }
        out
    }

    pub fn stop(&mut self) {
        self.running = false;
        self.pending.retain(|p| p.rule_idx == SCRIPT_RULE_IDX);
        self.every_due.clear();
    }

    /// Advance timers: `Every` rules and queued `Wait` continuations.
    /// Continuations drain even while stopped so script `beep`/`tone` finish.
    pub fn tick(&mut self, now_ms: u64) -> Vec<HostMsg> {
        let mut out = Vec::new();
        if self.running {
            let due: Vec<usize> = self
                .every_due
                .iter()
                .filter(|&(_, &t)| t <= now_ms)
                .map(|(&i, _)| i)
                .collect();
            for i in due {
                let rule = self.program.rules[i].clone();
                if let Trigger::Every { ms } = rule.trigger {
                    self.every_due.insert(i, now_ms + u64::from(ms.max(10)));
                    self.fire(i, &rule, now_ms, &mut out);
                }
            }
        }
        let mut still: Vec<PendingActions> = Vec::new();
        let mut ready: Vec<PendingActions> = Vec::new();
        for p in self.pending.drain(..) {
            if p.due_ms <= now_ms {
                ready.push(p);
            } else {
                still.push(p);
            }
        }
        self.pending = still;
        for p in ready {
            self.run_actions(p.rule_idx, p.actions, now_ms, &mut out);
        }
        out
    }

    /// React to a device message; returns commands to send back.
    pub fn handle_device(&mut self, now_ms: u64, msg: &DeviceMsg) -> Vec<HostMsg> {
        let mut out = Vec::new();
        if !self.running {
            return out;
        }
        match msg {
            DeviceMsg::Event { pin, edge, .. } => {
                let comp_events = self.comp_events_for(*pin, *edge);
                for i in 0..self.program.rules.len() {
                    let rule = self.program.rules[i].clone();
                    if !rule.enabled {
                        continue;
                    }
                    let hit = match &rule.trigger {
                        Trigger::PinRises { gpio } => {
                            *gpio == *pin && *edge == EventEdge::Rising
                        }
                        Trigger::PinFalls { gpio } => {
                            *gpio == *pin && *edge == EventEdge::Falling
                        }
                        Trigger::CompEvent { comp, event } => comp_events
                            .iter()
                            .any(|(c, e)| c == comp && e == event),
                        _ => false,
                    };
                    if hit {
                        self.fire(i, &rule, now_ms, &mut out);
                    }
                }
            }
            DeviceMsg::Telemetry { analog, .. } => {
                for sample in analog.iter() {
                    self.check_analog(sample.pin, sample.millivolts, now_ms, &mut out);
                }
            }
            DeviceMsg::AnalogValue { pin, millivolts } => {
                self.check_analog(*pin, *millivolts, now_ms, &mut out);
            }
            _ => {}
        }
        out
    }

    /// Run actions produced by a component script, outside any rule.
    pub fn run_script_actions(&mut self, actions: Vec<Action>, now_ms: u64) -> Vec<HostMsg> {
        let mut out = Vec::new();
        self.run_actions(SCRIPT_RULE_IDX, actions.into(), now_ms, &mut out);
        out
    }

    /// Commanded level of an output GPIO, as last sent to the device.
    pub fn out_high(&self, gpio: u8) -> bool {
        self.out_shadow.get(gpio).out_high
    }

    /// Component-level events implied by a pin edge.
    pub fn comp_events_for(&self, pin: u8, edge: EventEdge) -> Vec<(CompId, String)> {
        let Some(binding) = self.inputs_get(pin) else { return Vec::new() };
        let level = edge == EventEdge::Rising;
        let logical = if binding.active_low { !level } else { level };
        let mut evs = Vec::new();
        match binding.kind {
            InKind::Button => {
                evs.push((binding.comp, if logical { "pressed" } else { "released" }.to_string()));
            }
            InKind::Toggle => {
                evs.push((binding.comp, if logical { "on" } else { "off" }.to_string()));
            }
            InKind::DigitalSensor => {
                evs.push((binding.comp, if logical { "high" } else { "low" }.to_string()));
            }
        }
        evs.push((binding.comp, "changed".to_string()));
        evs
    }

    fn inputs_get(&self, pin: u8) -> Option<InBinding> {
        self.bindings.inputs.get(&pin).copied()
    }

    fn check_analog(&mut self, pin: u8, mv: u16, now_ms: u64, out: &mut Vec<HostMsg>) {
        for i in 0..self.program.rules.len() {
            let rule = self.program.rules[i].clone();
            if !rule.enabled {
                continue;
            }
            let (above, gpio, threshold) = match rule.trigger {
                Trigger::AnalogAbove { gpio, millivolts } => (true, gpio, millivolts),
                Trigger::AnalogBelow { gpio, millivolts } => (false, gpio, millivolts),
                _ => continue,
            };
            if gpio != pin {
                continue;
            }
            let hysteresis = 50i32;
            let crossed = if above {
                i32::from(mv) > i32::from(threshold) + hysteresis
            } else {
                i32::from(mv) < i32::from(threshold) - hysteresis
            };
            let released = if above {
                i32::from(mv) < i32::from(threshold) - hysteresis
            } else {
                i32::from(mv) > i32::from(threshold) + hysteresis
            };
            let latched = self.analog_latch.get(&i).copied().unwrap_or(false);
            if crossed && !latched {
                self.analog_latch.insert(i, true);
                self.fire(i, &rule, now_ms, out);
            } else if released && latched {
                self.analog_latch.insert(i, false);
            }
        }
    }

    fn fire(&mut self, rule_idx: usize, rule: &Rule, now_ms: u64, out: &mut Vec<HostMsg>) {
        self.firings.push_back(Firing { rule_idx, at_ms: now_ms });
        while self.firings.len() > 64 {
            self.firings.pop_front();
        }
        self.run_actions(rule_idx, rule.actions.clone().into(), now_ms, out);
    }

    fn run_actions(
        &mut self,
        rule_idx: usize,
        mut actions: VecDeque<Action>,
        now_ms: u64,
        out: &mut Vec<HostMsg>,
    ) {
        while let Some(action) = actions.pop_front() {
            match action {
                Action::Wait { ms } => {
                    self.pending.push(PendingActions {
                        due_ms: now_ms + u64::from(ms),
                        rule_idx,
                        actions,
                    });
                    return;
                }
                Action::SetPin { gpio, high } => {
                    self.emit(HostMsg::WriteDigital { pin: gpio, high }, out);
                }
                Action::TogglePin { gpio } => {
                    let high = !self.out_shadow.get(gpio).out_high;
                    self.emit(HostMsg::WriteDigital { pin: gpio, high }, out);
                }
                Action::SetPwm { gpio, freq_hz, duty_permille } => {
                    self.emit(HostMsg::SetPwm { pin: gpio, freq_hz, duty_permille }, out);
                }
                Action::Log { text } => self.log.push(text),
                Action::SetPinMode { gpio, mode } => {
                    self.emit(HostMsg::SetPinMode { pin: gpio, mode }, out);
                }
                Action::SetRgb { gpio, r, g, b } => {
                    self.emit(HostMsg::SetRgb { pin: gpio, r, g, b }, out);
                }
                Action::WatchAnalog { gpio, interval_ms } => {
                    self.emit(HostMsg::WatchAnalog { pin: gpio, interval_ms }, out);
                }
                Action::UartConfig { tx, rx, baud } => {
                    self.emit(HostMsg::UartConfig { tx, rx, baud }, out);
                }
                Action::UartWrite { data } => {
                    for chunk in data.chunks(wirelab_proto::UART_CHUNK) {
                        if let Ok(v) = wirelab_proto::heapless::Vec::from_slice(chunk) {
                            self.emit(HostMsg::UartWrite { data: v }, out);
                        }
                    }
                }
                Action::LcdInit { sck, mosi, cs, dc, rst, bl } => {
                    self.emit(HostMsg::LcdInit { sck, mosi, cs, dc, rst, bl }, out);
                }
                Action::LcdClear { rgb } => {
                    self.emit(HostMsg::LcdClear { rgb565: crate::program::rgb565(rgb) }, out);
                }
                Action::LcdRect { x, y, w, h, rgb } => {
                    self.emit(
                        HostMsg::LcdRect { x, y, w, h, rgb565: crate::program::rgb565(rgb) },
                        out,
                    );
                }
                Action::SpiConfig { sck, mosi, miso, freq_khz } => {
                    self.emit(HostMsg::SpiConfig { sck, mosi, miso, freq_khz }, out);
                }
                Action::SpiTransfer { cs, data } => {
                    if let Ok(v) = wirelab_proto::heapless::Vec::from_slice(
                        &data[..data.len().min(wirelab_proto::UART_CHUNK)],
                    ) {
                        self.emit(HostMsg::SpiTransfer { cs, data: v }, out);
                    }
                }
                Action::I2cConfig { sda, scl, freq_khz } => {
                    self.emit(HostMsg::I2cConfig { sda, scl, freq_khz }, out);
                }
                Action::I2cWrite { addr, data } => {
                    if let Ok(v) = wirelab_proto::heapless::Vec::from_slice(
                        &data[..data.len().min(wirelab_proto::UART_CHUNK)],
                    ) {
                        self.emit(HostMsg::I2cWrite { addr, data: v }, out);
                    }
                }
                Action::I2cRead { addr, reg, len } => {
                    self.emit(HostMsg::I2cRead { addr, reg, len }, out);
                }
                // Routed host-side by the app before actions reach the engine.
                Action::BoardMsg { .. } => {}
                // Executed host-side by the app before actions reach the engine.
                Action::HttpGet { .. } => {}
                Action::LcdText { x, y, rgb, text } => {
                    let mut t = text.as_str();
                    if t.len() > 32 {
                        t = &t[..32];
                    }
                    if let Ok(s) = wirelab_proto::heapless::String::try_from(t) {
                        self.emit(
                            HostMsg::LcdText { x, y, rgb565: crate::program::rgb565(rgb), text: s },
                            out,
                        );
                    }
                }
                Action::CompAction { comp, action, params } => {
                    self.comp_action(comp, &action, &params, &mut actions, out);
                }
            }
        }
    }

    fn comp_action(
        &mut self,
        comp: CompId,
        action: &str,
        params: &crate::component::PropMap,
        continuation: &mut VecDeque<Action>,
        out: &mut Vec<HostMsg>,
    ) {
        let Some(binding) = self.bindings.outputs.get(&comp).copied() else {
            self.log.push(format!("action '{action}' on unbound component"));
            return;
        };
        let gpio = binding.gpio;
        let level_on = binding.active_high;
        let param = |key: &str, default: f64| params.get(key).copied().unwrap_or(default);
        self.detach_behavior_for(comp, action, out);
        match action {
            "on" => self.emit(HostMsg::WriteDigital { pin: gpio, high: level_on }, out),
            "off" => self.emit(HostMsg::WriteDigital { pin: gpio, high: !level_on }, out),
            "toggle" => {
                let high = !self.out_shadow.get(gpio).out_high;
                self.emit(HostMsg::WriteDigital { pin: gpio, high }, out);
            }
            "blink" => {
                let period = param("period_ms", 500.0).clamp(20.0, 60000.0) as u16;
                if let Some(slot) = self.alloc_slot(comp) {
                    self.emit(
                        HostMsg::AttachBehavior {
                            slot,
                            behavior: Behavior::Blink { pin: gpio, period_ms: period },
                        },
                        out,
                    );
                } else {
                    self.log.push("no free behavior slots".to_string());
                }
            }
            "breathe" => {
                let period = param("period_ms", 2000.0).clamp(100.0, 60000.0) as u16;
                if let Some(slot) = self.alloc_slot(comp) {
                    self.emit(
                        HostMsg::AttachBehavior {
                            slot,
                            behavior: Behavior::Breathe { pin: gpio, period_ms: period },
                        },
                        out,
                    );
                }
            }
            "dim" => {
                let pct = param("percent", 50.0).clamp(0.0, 100.0);
                let duty = (pct * 10.0) as u16;
                let duty = if level_on { duty } else { 1000 - duty };
                self.emit(HostMsg::SetPwm { pin: gpio, freq_hz: 1000, duty_permille: duty }, out);
            }
            "set_angle" => {
                let deg = param("degrees", 90.0).clamp(0.0, 180.0);
                let duty = (25.0 + deg / 180.0 * 100.0) as u16;
                self.emit(HostMsg::SetPwm { pin: gpio, freq_hz: 50, duty_permille: duty }, out);
            }
            "beep" => {
                let ms = param("ms", 200.0).clamp(10.0, 10000.0) as u32;
                self.emit(HostMsg::WriteDigital { pin: gpio, high: level_on }, out);
                continuation.push_front(Action::SetPin { gpio, high: !level_on });
                continuation.push_front(Action::Wait { ms });
            }
            "tone" => {
                let freq = param("freq_hz", 880.0).clamp(20.0, 20000.0) as u32;
                let ms = param("ms", 300.0).clamp(10.0, 10000.0) as u32;
                self.emit(HostMsg::SetPwm { pin: gpio, freq_hz: freq, duty_permille: 500 }, out);
                continuation.push_front(Action::SetPwm { gpio, freq_hz: freq, duty_permille: 0 });
                continuation.push_front(Action::Wait { ms });
            }
            other => self.log.push(format!("unknown action '{other}'")),
        }
    }

    /// Blink/breathe own a slot per component; any other verb releases it.
    fn detach_behavior_for(&mut self, comp: CompId, action: &str, out: &mut Vec<HostMsg>) {
        if matches!(action, "blink" | "breathe") {
            return;
        }
        for (i, slot) in self.behavior_slots.iter_mut().enumerate() {
            if *slot == Some(comp) {
                *slot = None;
                out.push(HostMsg::DetachBehavior { slot: i as u8 });
            }
        }
    }

    fn alloc_slot(&mut self, comp: CompId) -> Option<u8> {
        if let Some(i) = self.behavior_slots.iter().position(|s| *s == Some(comp)) {
            return Some(i as u8);
        }
        let i = self.behavior_slots.iter().position(|s| s.is_none())?;
        self.behavior_slots[i] = Some(comp);
        Some(i as u8)
    }

    fn emit(&mut self, msg: HostMsg, out: &mut Vec<HostMsg>) {
        self.out_shadow.apply(&msg);
        out.push(msg);
    }
}
