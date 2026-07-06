//! The user's circuit: a board, placed components and wires between pins.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::component::{CompState, PropMap};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CompId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WireId(pub u32);

/// One end of a wire: a board pin or a component terminal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum Endpoint {
    BoardPin { key: String },
    Terminal { comp: CompId, terminal: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacedComponent {
    pub id: CompId,
    /// Key into the component library.
    pub def_id: String,
    /// Canvas position in millimetres, component centre.
    pub pos: [f32; 2],
    /// 0, 90, 180 or 270.
    #[serde(default)]
    pub rotation: u16,
    #[serde(default)]
    pub label: String,
    /// Overrides for `ComponentDef::props`, e.g. resistance.
    #[serde(default)]
    pub props: PropMap,
    /// Live, user-pokeable state (button pressed, pot position...).
    #[serde(default = "default_comp_state")]
    pub state: CompState,
    /// Attached behavior script (Rhai), Godot-style.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
}

fn default_comp_state() -> CompState {
    CompState::None
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Wire {
    pub id: WireId,
    pub a: Endpoint,
    pub b: Endpoint,
    pub color: [u8; 3],
    /// Optional manual routing points in canvas millimetres.
    #[serde(default)]
    pub waypoints: Vec<[f32; 2]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Circuit {
    /// Key into the board library.
    pub board_id: String,
    /// Board position on the canvas in millimetres, top-left corner.
    #[serde(default)]
    pub board_pos: [f32; 2],
    pub components: BTreeMap<CompId, PlacedComponent>,
    pub wires: BTreeMap<WireId, Wire>,
    next_comp: u32,
    next_wire: u32,
}

impl Circuit {
    pub fn new(board_id: &str) -> Self {
        Circuit {
            board_id: board_id.to_string(),
            board_pos: [0.0, 0.0],
            components: BTreeMap::new(),
            wires: BTreeMap::new(),
            next_comp: 1,
            next_wire: 1,
        }
    }

    pub fn add_component(&mut self, mut comp: PlacedComponent) -> CompId {
        let id = CompId(self.next_comp);
        self.next_comp += 1;
        comp.id = id;
        self.components.insert(id, comp);
        id
    }

    pub fn add_wire(&mut self, a: Endpoint, b: Endpoint, color: [u8; 3]) -> WireId {
        let id = WireId(self.next_wire);
        self.next_wire += 1;
        self.wires.insert(id, Wire { id, a, b, color, waypoints: Vec::new() });
        id
    }

    /// Remove a component and every wire attached to it.
    pub fn remove_component(&mut self, id: CompId) {
        self.components.remove(&id);
        self.wires.retain(|_, w| {
            let touches = |e: &Endpoint| matches!(e, Endpoint::Terminal { comp, .. } if *comp == id);
            !touches(&w.a) && !touches(&w.b)
        });
    }

    pub fn remove_wire(&mut self, id: WireId) {
        self.wires.remove(&id);
    }

    /// Insert a component in series into an existing wire: the wire is
    /// replaced by two wires meeting at the component's terminals. A
    /// single-terminal part (routing dot) joins both halves at its node.
    pub fn splice_component(
        &mut self,
        wire: WireId,
        comp: CompId,
        term_a: &str,
        term_b: &str,
    ) -> bool {
        let Some(w) = self.wires.get(&wire).cloned() else { return false };
        self.remove_wire(wire);
        self.add_wire(
            w.a,
            Endpoint::Terminal { comp, terminal: term_a.into() },
            w.color,
        );
        self.add_wire(
            Endpoint::Terminal { comp, terminal: term_b.into() },
            w.b,
            w.color,
        );
        true
    }

    pub fn wires_at(&self, ep: &Endpoint) -> impl Iterator<Item = &Wire> {
        self.wires.values().filter(move |w| &w.a == ep || &w.b == ep)
    }
}
