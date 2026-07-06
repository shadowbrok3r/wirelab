//! Static circuit lints, independent of any live session.

use crate::board::{BoardProfile, PinCaps};
use crate::circuit::{Circuit, CompId, Endpoint, PlacedComponent, WireId};
use crate::component::{SimModel, TerminalRole};
use crate::library::Library;
use crate::netlist::Netlist;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// A remedy WireLab can apply with one click.
#[derive(Debug, Clone, PartialEq)]
pub enum LintFix {
    /// Splice a ~`ohms` resistor into `wire`.
    SpliceResistor { wire: WireId, ohms: f32, action: String },
}

#[derive(Debug, Clone)]
pub struct Lint {
    pub severity: Severity,
    pub message: String,
    /// Components this lint concerns, for hover-highlighting.
    pub comps: Vec<CompId>,
    pub fix: Option<LintFix>,
}

/// Component ids present on a net, for highlighting net-level problems.
fn net_comps(nl: &Netlist, net: usize) -> Vec<CompId> {
    let mut out: Vec<CompId> = nl.nets[net]
        .endpoints
        .iter()
        .filter_map(|ep| match ep {
            Endpoint::Terminal { comp, .. } => Some(*comp),
            _ => None,
        })
        .collect();
    out.dedup();
    out
}

pub fn validate(
    circuit: &Circuit,
    board: &BoardProfile,
    lib: &Library,
    nl: &Netlist,
) -> Vec<Lint> {
    let mut lints = Vec::new();
    fn push(lints: &mut Vec<Lint>, severity: Severity, message: String) {
        lints.push(Lint { severity, message, comps: Vec::new(), fix: None });
    }

    for net in &nl.nets {
        if net.rails.gnd && (net.rails.v33 || net.rails.v5) {
            lints.push(Lint {
                severity: Severity::Error,
                message: "power rail shorted directly to GND".to_string(),
                comps: net_comps(nl, net.id),
                fix: None,
            });
        }
        if net.rails.v33 && net.rails.v5 {
            lints.push(Lint {
                severity: Severity::Error,
                message: "3V3 and 5V rails wired together".to_string(),
                comps: net_comps(nl, net.id),
                fix: None,
            });
        }
        if net.gpios.len() > 1 {
            lints.push(Lint {
                severity: Severity::Warning,
                message: format!("GPIOs {:?} are wired directly together", net.gpios),
                comps: net_comps(nl, net.id),
                fix: None,
            });
        }
    }

    for comp in circuit.components.values() {
        let Some(def) = lib.component(&comp.def_id) else {
            push(&mut lints, Severity::Error, format!("unknown component definition '{}'", comp.def_id));
            continue;
        };
        let name = if comp.label.is_empty() { &def.name } else { &comp.label };

        let mut unwired = Vec::new();
        for t in &def.terminals {
            let ep = Endpoint::Terminal { comp: comp.id, terminal: t.id.clone() };
            let wired = circuit.wires_at(&ep).next().is_some();
            if !wired {
                unwired.push(t.name.clone());
            }
        }
        if !unwired.is_empty() && unwired.len() < def.terminals.len() {
            lints.push(Lint {
                severity: Severity::Warning,
                message: format!("{name}: unwired terminals: {}", unwired.join(", ")),
                comps: vec![comp.id],
                fix: None,
            });
        }

        if let SimModel::Led { forward_mv } = def.sim {
            let anode_ep = def
                .terminal_by_role(TerminalRole::Anode)
                .map(|t| Endpoint::Terminal { comp: comp.id, terminal: t.id.clone() });
            let anode = anode_ep.as_ref().and_then(|ep| nl.net_of(ep));
            let cathode = def.terminal_by_role(TerminalRole::Cathode).and_then(|t| {
                nl.net_of(&Endpoint::Terminal { comp: comp.id, terminal: t.id.clone() })
            });
            if let (Some(a), Some(c)) = (anode, cathode) {
                let direct_rail_a = nl.nets[a].rails.v33 || nl.nets[a].rails.v5;
                let direct_gpio_a = !nl.nets[a].gpios.is_empty();
                let direct_low_c = nl.nets[c].rails.gnd || !nl.nets[c].gpios.is_empty();
                if (direct_rail_a || direct_gpio_a) && direct_low_c {
                    // LEDs don't limit their own current: pick R for ~6 mA.
                    let vf = f32::from(forward_mv) / 1000.0;
                    let ohms = ((3.3 - vf) / 0.006).max(50.0);
                    // Fix by splicing into a wire on the anode terminal.
                    let fix = anode_ep
                        .as_ref()
                        .and_then(|ep| circuit.wires_at(ep).next())
                        .map(|w| LintFix::SpliceResistor {
                            wire: w.id,
                            ohms,
                            action: format!("add ≈{ohms:.0} Ω in series"),
                        });
                    lints.push(Lint {
                        severity: Severity::Warning,
                        message: format!(
                            "{name}: needs a series resistor — an LED doesn't limit its own \
                             current. (3.3 V − {vf:.1} V forward drop) ÷ 6 mA ≈ {ohms:.0} Ω"
                        ),
                        comps: vec![comp.id],
                        fix,
                    });
                }
            }
        }
    }

    // A wire joining both ends of one component short-circuits it.
    for wire in circuit.wires.values() {
        if let (Endpoint::Terminal { comp: ca, .. }, Endpoint::Terminal { comp: cb, .. }) =
            (&wire.a, &wire.b)
            && ca == cb
        {
            let name = circuit
                .components
                .get(ca)
                .and_then(|c| {
                    if c.label.is_empty() {
                        lib.component(&c.def_id).map(|d| d.name.clone())
                    } else {
                        Some(c.label.clone())
                    }
                })
                .unwrap_or_default();
            lints.push(Lint {
                severity: Severity::Warning,
                message: format!(
                    "a wire connects {name}'s terminals to each other — current will bypass it entirely"
                ),
                comps: vec![*ca],
                fix: None,
            });
        }
    }

    // A resistor in PARALLEL with an LED doesn't limit its current at all.
    let two_nets = |comp: &PlacedComponent, ra: TerminalRole, rb: TerminalRole| {
        let def = lib.component(&comp.def_id)?;
        let a = def.terminal_by_role(ra)?;
        let b = def.terminal_by_role(rb)?;
        let na = nl.net_of(&Endpoint::Terminal { comp: comp.id, terminal: a.id.clone() })?;
        let nb = nl.net_of(&Endpoint::Terminal { comp: comp.id, terminal: b.id.clone() })?;
        Some((na, nb))
    };
    for res in circuit.components.values() {
        let Some(rdef) = lib.component(&res.def_id) else { continue };
        if !matches!(rdef.sim, SimModel::Resistor { .. }) {
            continue;
        }
        let Some((ra, rb)) = two_nets(res, TerminalRole::A, TerminalRole::B) else { continue };
        for led in circuit.components.values() {
            let Some(ldef) = lib.component(&led.def_id) else { continue };
            if !matches!(ldef.sim, SimModel::Led { .. }) {
                continue;
            }
            let Some((la, lc)) = two_nets(led, TerminalRole::Anode, TerminalRole::Cathode)
            else {
                continue;
            };
            if (ra == la && rb == lc) || (ra == lc && rb == la) {
                let lname = if led.label.is_empty() { &ldef.name } else { &led.label };
                lints.push(Lint {
                    severity: Severity::Warning,
                    message: format!(
                        "{}: resistor is in PARALLEL with {lname} — parallel parts share voltage, they don't \
                         limit each other's current. Put the resistor in series (in the LED's supply wire) instead",
                        if res.label.is_empty() { &rdef.name } else { &res.label },
                    ),
                    comps: vec![res.id, led.id],
                    fix: None,
                });
            }
        }
    }

    // Pins with special roles used at all.
    for wire in circuit.wires.values() {
        for ep in [&wire.a, &wire.b] {
            let Endpoint::BoardPin { key } = ep else { continue };
            let Some(pin) = board.pin(key) else {
                push(&mut lints, Severity::Error, format!("wire references unknown board pin '{key}'"));
                continue;
            };
            if pin.caps.contains(PinCaps::STRAPPING) {
                push(&mut lints, Severity::Info,
                    format!("{}: strapping pin, keep it in a safe state at boot", pin.key),
                );
            }
            if pin.caps.contains(PinCaps::FLASH_RESERVED) {
                push(&mut lints, Severity::Error,
                    format!("{}: reserved for SPI flash, using it will crash the chip", pin.key),
                );
            }
            if let Some(w) = &pin.warning {
                push(&mut lints, Severity::Info, format!("{}: {w}", pin.key));
            }
        }
    }

    lints
}
