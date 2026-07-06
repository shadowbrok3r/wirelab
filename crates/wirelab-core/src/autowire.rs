//! Automatic wiring: connect selected components to the board (and each
//! other) using their terminal roles and free board pins.

use std::collections::HashSet;

use crate::board::{BoardProfile, PinCaps, PinKind};
use crate::circuit::{Circuit, CompId, Endpoint};
use crate::component::{SimModel, TerminalRole};
use crate::library::Library;

#[derive(Debug, Default)]
pub struct AutoWirePlan {
    pub wires: Vec<(Endpoint, Endpoint)>,
    pub notes: Vec<String>,
}

/// What a free GPIO must be able to do for a given hookup.
#[derive(Clone, Copy, PartialEq)]
enum Need {
    Output,
    Input,
    Adc,
    Pwm,
}

struct Pins<'a> {
    board: &'a BoardProfile,
    used: HashSet<u8>,
}

impl<'a> Pins<'a> {
    fn new(circuit: &Circuit, board: &'a BoardProfile) -> Pins<'a> {
        let mut used = HashSet::new();
        for wire in circuit.wires.values() {
            for ep in [&wire.a, &wire.b] {
                if let Endpoint::BoardPin { key } = ep
                    && let Some(pin) = board.pin(key)
                        && let PinKind::Gpio(g) = pin.kind {
                            used.insert(g);
                        }
            }
        }
        Pins { board, used }
    }

    fn rail(&self, kind: PinKind) -> Option<Endpoint> {
        self.board
            .pins
            .iter()
            .find(|p| p.kind == kind)
            .map(|p| Endpoint::BoardPin { key: p.key.clone() })
    }

    /// Claim the first free, unwarned GPIO satisfying `need`.
    fn claim(&mut self, need: Need) -> Option<(u8, Endpoint)> {
        // Two passes: prefer pins without warnings (strapping, reserved...).
        for allow_warned in [false, true] {
            for pin in &self.board.pins {
                let PinKind::Gpio(g) = pin.kind else { continue };
                if self.used.contains(&g) || (pin.warning.is_some() && !allow_warned) {
                    continue;
                }
                let ok = match need {
                    Need::Output => {
                        pin.caps.contains(PinCaps::DIGITAL_OUT)
                            && !pin.caps.contains(PinCaps::INPUT_ONLY)
                    }
                    Need::Input => pin.caps.contains(PinCaps::DIGITAL_IN),
                    Need::Adc => pin.caps.contains(PinCaps::ADC) && pin.adc.is_some(),
                    Need::Pwm => {
                        pin.caps.contains(PinCaps::PWM)
                            && !pin.caps.contains(PinCaps::INPUT_ONLY)
                    }
                };
                if ok {
                    self.used.insert(g);
                    return Some((g, Endpoint::BoardPin { key: pin.key.clone() }));
                }
            }
        }
        None
    }
}

/// Plan wires hooking `targets` up to the board. Terminals that already have
/// wires are left alone; selected resistors pair up with selected LEDs
/// (series) and photoresistors (divider).
pub fn auto_wire(
    circuit: &Circuit,
    board: &BoardProfile,
    lib: &Library,
    targets: &[CompId],
) -> AutoWirePlan {
    let mut plan = AutoWirePlan::default();
    let mut pins = Pins::new(circuit, board);

    let term = |comp: CompId, role: TerminalRole| -> Option<Endpoint> {
        let c = circuit.components.get(&comp)?;
        let def = lib.component(&c.def_id)?;
        let t = def.terminal_by_role(role)?;
        Some(Endpoint::Terminal { comp, terminal: t.id.clone() })
    };
    let wired = |ep: &Endpoint| circuit.wires_at(ep).next().is_some();
    let name = |comp: CompId| -> String {
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
    };

    let gnd = pins.rail(PinKind::Gnd);
    let v33 = pins.rail(PinKind::V3_3);
    let v5 = pins.rail(PinKind::V5);

    // Free series resistors among the targets, ready to pair up.
    let mut resistors: Vec<CompId> = targets
        .iter()
        .copied()
        .filter(|id| {
            let Some(c) = circuit.components.get(id) else { return false };
            let Some(def) = lib.component(&c.def_id) else { return false };
            matches!(def.sim, SimModel::Resistor { .. })
                && term(*id, TerminalRole::A).is_some_and(|e| !wired(&e))
                && term(*id, TerminalRole::B).is_some_and(|e| !wired(&e))
        })
        .collect();

    let push = |plan: &mut AutoWirePlan, a: Option<Endpoint>, b: Option<Endpoint>| {
        if let (Some(a), Some(b)) = (a, b) {
            plan.wires.push((a, b));
        }
    };

    for &id in targets {
        let Some(c) = circuit.components.get(&id) else { continue };
        let Some(def) = lib.component(&c.def_id).cloned() else { continue };
        let who = name(id);
        match def.sim {
            SimModel::Led { .. } => {
                let anode = term(id, TerminalRole::Anode);
                let cathode = term(id, TerminalRole::Cathode);
                if anode.as_ref().is_some_and(wired) || cathode.as_ref().is_some_and(wired) {
                    plan.notes.push(format!("{who}: already wired, skipped"));
                    continue;
                }
                let Some((g, gpio_ep)) = pins.claim(Need::Output) else {
                    plan.notes.push(format!("{who}: no free output GPIO"));
                    continue;
                };
                if let Some(res) = resistors.pop() {
                    push(&mut plan, Some(gpio_ep), term(res, TerminalRole::A));
                    push(&mut plan, term(res, TerminalRole::B), anode);
                    plan.notes.push(format!(
                        "{who}: GPIO{g} → {} → anode, cathode → GND",
                        name(res)
                    ));
                } else {
                    push(&mut plan, Some(gpio_ep), anode);
                    plan.notes.push(format!(
                        "{who}: GPIO{g} → anode (consider a series resistor)"
                    ));
                }
                push(&mut plan, cathode, gnd.clone());
            }
            SimModel::PushButton | SimModel::ToggleSwitch => {
                let a = term(id, TerminalRole::A);
                let b = term(id, TerminalRole::B);
                if a.as_ref().is_some_and(wired) || b.as_ref().is_some_and(wired) {
                    plan.notes.push(format!("{who}: already wired, skipped"));
                    continue;
                }
                let Some((g, gpio_ep)) = pins.claim(Need::Input) else {
                    plan.notes.push(format!("{who}: no free input GPIO"));
                    continue;
                };
                push(&mut plan, Some(gpio_ep), a);
                push(&mut plan, b, gnd.clone());
                plan.notes.push(format!("{who}: GPIO{g} → switch → GND (pull-up)"));
            }
            SimModel::SlideSwitchSpdt => {
                let common = term(id, TerminalRole::Common);
                if common.as_ref().is_some_and(wired) {
                    plan.notes.push(format!("{who}: already wired, skipped"));
                    continue;
                }
                let Some((g, gpio_ep)) = pins.claim(Need::Input) else {
                    plan.notes.push(format!("{who}: no free input GPIO"));
                    continue;
                };
                push(&mut plan, Some(gpio_ep), common);
                push(&mut plan, term(id, TerminalRole::A), v33.clone());
                push(&mut plan, term(id, TerminalRole::B), gnd.clone());
                plan.notes.push(format!("{who}: GPIO{g} on common, A→3V3, B→GND"));
            }
            SimModel::Potentiometer { .. } => {
                let wiper = term(id, TerminalRole::Wiper);
                if wiper.as_ref().is_some_and(wired) {
                    plan.notes.push(format!("{who}: already wired, skipped"));
                    continue;
                }
                let Some((g, gpio_ep)) = pins.claim(Need::Adc) else {
                    plan.notes.push(format!("{who}: no free ADC pin"));
                    continue;
                };
                push(&mut plan, term(id, TerminalRole::EndA), v33.clone());
                push(&mut plan, term(id, TerminalRole::EndB), gnd.clone());
                push(&mut plan, wiper, Some(gpio_ep));
                plan.notes.push(format!("{who}: wiper on GPIO{g} (ADC)"));
            }
            SimModel::Photoresistor { .. } => {
                let a = term(id, TerminalRole::A);
                let b = term(id, TerminalRole::B);
                if a.as_ref().is_some_and(wired) || b.as_ref().is_some_and(wired) {
                    plan.notes.push(format!("{who}: already wired, skipped"));
                    continue;
                }
                let Some((g, gpio_ep)) = pins.claim(Need::Adc) else {
                    plan.notes.push(format!("{who}: no free ADC pin"));
                    continue;
                };
                push(&mut plan, a, v33.clone());
                push(&mut plan, b.clone(), Some(gpio_ep));
                if let Some(res) = resistors.pop() {
                    push(&mut plan, b, term(res, TerminalRole::A));
                    push(&mut plan, term(res, TerminalRole::B), gnd.clone());
                    plan.notes.push(format!(
                        "{who}: divider with {} onto GPIO{g} (ADC)",
                        name(res)
                    ));
                } else {
                    plan.notes.push(format!(
                        "{who}: on GPIO{g} (ADC) — add a resistor to GND for a divider"
                    ));
                }
            }
            SimModel::Buzzer { .. } => {
                let sig = term(id, TerminalRole::Signal);
                if sig.as_ref().is_some_and(wired) {
                    plan.notes.push(format!("{who}: already wired, skipped"));
                    continue;
                }
                let Some((g, gpio_ep)) = pins.claim(Need::Pwm) else {
                    plan.notes.push(format!("{who}: no free PWM pin"));
                    continue;
                };
                push(&mut plan, sig, Some(gpio_ep));
                push(&mut plan, term(id, TerminalRole::Gnd), gnd.clone());
                plan.notes.push(format!("{who}: signal on GPIO{g}"));
            }
            SimModel::Servo => {
                let sig = term(id, TerminalRole::Signal);
                if sig.as_ref().is_some_and(wired) {
                    plan.notes.push(format!("{who}: already wired, skipped"));
                    continue;
                }
                let Some((g, gpio_ep)) = pins.claim(Need::Pwm) else {
                    plan.notes.push(format!("{who}: no free PWM pin"));
                    continue;
                };
                let supply = v5.clone().or_else(|| v33.clone());
                if v5.is_none() {
                    plan.notes.push(format!("{who}: no 5V pin, using 3V3"));
                }
                push(&mut plan, term(id, TerminalRole::Vcc), supply);
                push(&mut plan, term(id, TerminalRole::Gnd), gnd.clone());
                push(&mut plan, sig, Some(gpio_ep));
                plan.notes.push(format!("{who}: signal on GPIO{g} (PWM)"));
            }
            SimModel::RelayModule | SimModel::DigitalSensor => {
                let sig = term(id, TerminalRole::Signal);
                if sig.as_ref().is_some_and(wired) {
                    plan.notes.push(format!("{who}: already wired, skipped"));
                    continue;
                }
                let need = if matches!(def.sim, SimModel::RelayModule) {
                    Need::Output
                } else {
                    Need::Input
                };
                let Some((g, gpio_ep)) = pins.claim(need) else {
                    plan.notes.push(format!("{who}: no free GPIO"));
                    continue;
                };
                push(&mut plan, term(id, TerminalRole::Vcc), v33.clone());
                push(&mut plan, term(id, TerminalRole::Gnd), gnd.clone());
                push(&mut plan, sig, Some(gpio_ep));
                plan.notes.push(format!("{who}: signal on GPIO{g}"));
            }
            SimModel::AnalogSensor { .. } => {
                let sig = term(id, TerminalRole::Signal);
                if sig.as_ref().is_some_and(wired) {
                    plan.notes.push(format!("{who}: already wired, skipped"));
                    continue;
                }
                let Some((g, gpio_ep)) = pins.claim(Need::Adc) else {
                    plan.notes.push(format!("{who}: no free ADC pin"));
                    continue;
                };
                push(&mut plan, term(id, TerminalRole::Vcc), v33.clone());
                push(&mut plan, term(id, TerminalRole::Gnd), gnd.clone());
                push(&mut plan, sig, Some(gpio_ep));
                plan.notes.push(format!("{who}: signal on GPIO{g} (ADC)"));
            }
            SimModel::Resistor { .. } | SimModel::Generic => {}
        }
    }

    if !resistors.is_empty() {
        plan.notes.push(format!(
            "{} resistor(s) left unwired — pair them with an LED or photoresistor",
            resistors.len()
        ));
    }
    plan
}
