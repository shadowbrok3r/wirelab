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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BbId(pub u32);

/// One end of a wire: a board pin or a component terminal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum Endpoint {
    BoardPin { key: String },
    Terminal { comp: CompId, terminal: String },
    /// A breadboard hole by printed name, e.g. "E12".
    Hole { bb: BbId, hole: String },
}

/// A solderable breadboard placed on the canvas.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacedBreadboard {
    pub id: BbId,
    /// Key into the breadboard library.
    pub profile: String,
    /// Canvas position in millimetres, board centre.
    pub pos: [f32; 2],
    /// 0, 90, 180 or 270.
    #[serde(default)]
    pub rotation: u16,
    #[serde(default)]
    pub label: String,
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
    /// Build label for wiring tables, e.g. "W1".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// Physical wire spec, e.g. "12 AWG stranded silicone, 600 V".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub spec: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Circuit {
    /// Key into the board library.
    pub board_id: String,
    /// Board position on the canvas in millimetres, top-left corner.
    #[serde(default)]
    pub board_pos: [f32; 2],
    /// Dev board quarter turn about its centre, in degrees.
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub board_rotation: u16,
    /// On-board display picture turn relative to the board, in degrees: 0, 90, 180 or 270.
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub display_rotation: u16,
    pub components: BTreeMap<CompId, PlacedComponent>,
    pub wires: BTreeMap<WireId, Wire>,
    next_comp: u32,
    next_wire: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub breadboards: BTreeMap<BbId, PlacedBreadboard>,
    #[serde(default = "first_id")]
    next_bb: u32,
    /// Net names, each pinned to one endpoint on the named net.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub net_names: BTreeMap<String, Endpoint>,
    /// Schematic placement and sheet notes for exports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schematic: Option<crate::export::SchematicLayout>,
    /// Physical perfboard build for exports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perf: Option<crate::perf::PerfLayout>,
}

fn first_id() -> u32 {
    1
}

impl Circuit {
    pub fn new(board_id: &str) -> Self {
        Circuit {
            board_id: board_id.to_string(),
            board_pos: [0.0, 0.0],
            board_rotation: 0,
            display_rotation: 0,
            components: BTreeMap::new(),
            wires: BTreeMap::new(),
            next_comp: 1,
            next_wire: 1,
            breadboards: BTreeMap::new(),
            next_bb: 1,
            net_names: BTreeMap::new(),
            schematic: None,
            perf: None,
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
        self.wires.insert(
            id,
            Wire { id, a, b, color, waypoints: Vec::new(), label: String::new(), spec: String::new() },
        );
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

    pub fn add_breadboard(&mut self, mut bb: PlacedBreadboard) -> BbId {
        self.next_bb = self.next_bb.max(self.breadboards.keys().map(|k| k.0 + 1).max().unwrap_or(1));
        let id = BbId(self.next_bb);
        self.next_bb += 1;
        bb.id = id;
        self.breadboards.insert(id, bb);
        id
    }

    /// Remove a breadboard and every wire landing on its holes.
    pub fn remove_breadboard(&mut self, id: BbId) {
        self.breadboards.remove(&id);
        self.wires.retain(|_, w| {
            let touches = |e: &Endpoint| matches!(e, Endpoint::Hole { bb, .. } if *bb == id);
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

    /// Canvas turn of the on-board display picture: board turn plus picture turn.
    pub fn screen_rotation(&self) -> u16 {
        (self.board_rotation % 360 + self.display_rotation % 360) % 360
    }
}

fn is_zero_u16(v: &u16) -> bool {
    *v == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"{"board_id":"esp32-c6-lcd-147","board_pos":[0.0,0.0],"board_rotation":180,
        "components":{},"wires":{},"next_comp":1,"next_wire":1}"#;

    #[test]
    fn display_rotation_defaults_to_zero_and_is_omitted_when_zero() {
        let mut c: Circuit = serde_json::from_str(MINIMAL).expect("minimal circuit parses");
        assert_eq!(c.display_rotation, 0);
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("display_rotation"), "{json}");
        c.display_rotation = 180;
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains("\"display_rotation\":180"), "{json}");
        let back: Circuit = serde_json::from_str(&json).unwrap();
        assert_eq!(back.display_rotation, 180);
    }

    #[test]
    fn screen_rotation_adds_board_and_display_turns() {
        let mut c = Circuit::new("esp32-c6-lcd-147");
        assert_eq!(c.screen_rotation(), 0);
        for (board, display, screen) in [(180, 180, 0), (180, 0, 180), (90, 180, 270), (270, 180, 90), (270, 270, 180)] {
            c.board_rotation = board;
            c.display_rotation = display;
            assert_eq!(c.screen_rotation(), screen, "board {board} + display {display}");
        }
    }
}
