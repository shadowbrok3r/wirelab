//! Net extraction: wires + board pins + terminals folded into electrical nets.

use std::collections::HashMap;

use crate::board::{BoardProfile, PinKind};
use crate::circuit::{Circuit, Endpoint};
use crate::component::SimModel;
use crate::library::Library;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RailSet {
    pub gnd: bool,
    pub v33: bool,
    pub v5: bool,
}

impl RailSet {
    pub fn any(self) -> bool {
        self.gnd || self.v33 || self.v5
    }

    pub fn merge(&mut self, other: RailSet) {
        self.gnd |= other.gnd;
        self.v33 |= other.v33;
        self.v5 |= other.v5;
    }
}

#[derive(Debug, Clone)]
pub struct Net {
    pub id: usize,
    pub endpoints: Vec<Endpoint>,
    pub gpios: Vec<u8>,
    pub rails: RailSet,
}

/// Nets plus "reach" groups: nets joined across resistive passives, used to
/// answer "which GPIO ultimately controls this component".
#[derive(Debug, Clone, Default)]
pub struct Netlist {
    pub nets: Vec<Net>,
    ep_net: HashMap<Endpoint, usize>,
    net_reach: Vec<usize>,
    reach_gpios: HashMap<usize, Vec<u8>>,
    reach_rails: HashMap<usize, RailSet>,
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        UnionFind { parent: (0..n).collect() }
    }

    fn find(&mut self, i: usize) -> usize {
        if self.parent[i] != i {
            let root = self.find(self.parent[i]);
            self.parent[i] = root;
        }
        self.parent[i]
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[rb] = ra;
        }
    }
}

impl Netlist {
    pub fn build(circuit: &Circuit, board: &BoardProfile, lib: &Library) -> Netlist {
        // Enumerate every endpoint: board pins, then each component terminal.
        let mut eps: Vec<Endpoint> = Vec::new();
        let mut index: HashMap<Endpoint, usize> = HashMap::new();
        let push = |eps: &mut Vec<Endpoint>, index: &mut HashMap<Endpoint, usize>, ep: Endpoint| {
            *index.entry(ep.clone()).or_insert_with(|| {
                eps.push(ep);
                eps.len() - 1
            })
        };
        for pin in &board.pins {
            push(&mut eps, &mut index, Endpoint::BoardPin { key: pin.key.clone() });
        }
        for comp in circuit.components.values() {
            if let Some(def) = lib.component(&comp.def_id) {
                for t in &def.terminals {
                    push(
                        &mut eps,
                        &mut index,
                        Endpoint::Terminal { comp: comp.id, terminal: t.id.clone() },
                    );
                }
            }
        }
        for wire in circuit.wires.values() {
            push(&mut eps, &mut index, wire.a.clone());
            push(&mut eps, &mut index, wire.b.clone());
        }

        let mut uf = UnionFind::new(eps.len());

        // All pins of one power rail are the same node.
        let mut rail_first: HashMap<&'static str, usize> = HashMap::new();
        for pin in &board.pins {
            let rail_key = match pin.kind {
                PinKind::Gnd => "gnd",
                PinKind::V3_3 => "v33",
                PinKind::V5 => "v5",
                _ => continue,
            };
            let i = index[&Endpoint::BoardPin { key: pin.key.clone() }];
            match rail_first.get(rail_key) {
                Some(&first) => uf.union(first, i),
                None => {
                    rail_first.insert(rail_key, i);
                }
            }
        }

        for wire in circuit.wires.values() {
            uf.union(index[&wire.a], index[&wire.b]);
        }

        // Collapse roots into dense net ids.
        let mut root_net: HashMap<usize, usize> = HashMap::new();
        let mut nets: Vec<Net> = Vec::new();
        let mut ep_net: HashMap<Endpoint, usize> = HashMap::new();
        for (i, ep) in eps.iter().enumerate() {
            let root = uf.find(i);
            let net_id = *root_net.entry(root).or_insert_with(|| {
                nets.push(Net {
                    id: nets.len(),
                    endpoints: Vec::new(),
                    gpios: Vec::new(),
                    rails: RailSet::default(),
                });
                nets.len() - 1
            });
            nets[net_id].endpoints.push(ep.clone());
            if let Endpoint::BoardPin { key } = ep
                && let Some(pin) = board.pin(key) {
                    match pin.kind {
                        PinKind::Gpio(g) => {
                            if !nets[net_id].gpios.contains(&g) {
                                nets[net_id].gpios.push(g);
                            }
                        }
                        PinKind::Gnd => nets[net_id].rails.gnd = true,
                        PinKind::V3_3 => nets[net_id].rails.v33 = true,
                        PinKind::V5 => nets[net_id].rails.v5 = true,
                        _ => {}
                    }
                }
            ep_net.insert(ep.clone(), net_id);
        }

        // Reach: merge nets across resistive passives.
        let mut reach_uf = UnionFind::new(nets.len());
        for comp in circuit.components.values() {
            let Some(def) = lib.component(&comp.def_id) else { continue };
            let bridges = matches!(
                def.sim,
                SimModel::Resistor { .. }
                    | SimModel::Photoresistor { .. }
                    | SimModel::Potentiometer { .. }
            );
            if !bridges {
                continue;
            }
            let term_nets: Vec<usize> = def
                .terminals
                .iter()
                .filter_map(|t| {
                    ep_net
                        .get(&Endpoint::Terminal { comp: comp.id, terminal: t.id.clone() })
                        .copied()
                })
                .collect();
            for pair in term_nets.windows(2) {
                reach_uf.union(pair[0], pair[1]);
            }
        }
        let net_reach: Vec<usize> = (0..nets.len()).map(|i| reach_uf.find(i)).collect();
        let mut reach_gpios: HashMap<usize, Vec<u8>> = HashMap::new();
        let mut reach_rails: HashMap<usize, RailSet> = HashMap::new();
        for net in &nets {
            let r = net_reach[net.id];
            let entry = reach_gpios.entry(r).or_default();
            for &g in &net.gpios {
                if !entry.contains(&g) {
                    entry.push(g);
                }
            }
            reach_rails.entry(r).or_default().merge(net.rails);
        }

        Netlist { nets, ep_net, net_reach, reach_gpios, reach_rails }
    }

    pub fn net_of(&self, ep: &Endpoint) -> Option<usize> {
        self.ep_net.get(ep).copied()
    }

    pub fn net_of_gpio(&self, board: &BoardProfile, gpio: u8) -> Option<usize> {
        let pin = board.gpio_pin(gpio)?;
        self.net_of(&Endpoint::BoardPin { key: pin.key.clone() })
    }

    pub fn reach_of(&self, net: usize) -> usize {
        self.net_reach[net]
    }

    pub fn reach_gpios(&self, net: usize) -> &[u8] {
        self.reach_gpios
            .get(&self.net_reach[net])
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn reach_rails(&self, net: usize) -> RailSet {
        self.reach_rails.get(&self.net_reach[net]).copied().unwrap_or_default()
    }
}

/// Whether a proposed wire between two endpoints is electrically sound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireVerdict {
    Ok,
    /// Both points already sit on the same net; the wire would do nothing.
    Redundant,
    /// Hard electrical conflict; the wire must not be created.
    Blocked(String),
}

impl WireVerdict {
    pub fn is_ok(&self) -> bool {
        matches!(self, WireVerdict::Ok)
    }
}

/// Judge a candidate wire before it exists. `output_gpios` lists GPIOs the
/// current plan drives as outputs; tying those straight to a rail is a short.
pub fn wire_verdict(
    nl: &Netlist,
    board: &BoardProfile,
    from: &Endpoint,
    to: &Endpoint,
    output_gpios: &[u8],
) -> WireVerdict {
    if from == to {
        return WireVerdict::Redundant;
    }
    let unusable = |ep: &Endpoint| -> Option<&'static str> {
        let Endpoint::BoardPin { key } = ep else { return None };
        match board.pin(key)?.kind {
            PinKind::NotConnected => Some("pin has no internal connection"),
            _ => None,
        }
    };
    if let Some(why) = unusable(from).or_else(|| unusable(to)) {
        return WireVerdict::Blocked(why.into());
    }
    let (Some(na), Some(nb)) = (nl.net_of(from), nl.net_of(to)) else {
        return WireVerdict::Ok;
    };
    if na == nb {
        return WireVerdict::Redundant;
    }
    let mut rails = nl.nets[na].rails;
    rails.merge(nl.nets[nb].rails);
    if rails.gnd && (rails.v33 || rails.v5) {
        return WireVerdict::Blocked("would short GND to a supply rail".into());
    }
    if rails.v33 && rails.v5 {
        return WireVerdict::Blocked("would short 3.3 V to 5 V".into());
    }
    if rails.any() {
        let driven = nl.nets[na]
            .gpios
            .iter()
            .chain(&nl.nets[nb].gpios)
            .find(|g| output_gpios.contains(g));
        if let Some(g) = driven {
            return WireVerdict::Blocked(format!(
                "would tie GPIO{g} (driven as an output) straight to a rail"
            ));
        }
    }
    WireVerdict::Ok
}
