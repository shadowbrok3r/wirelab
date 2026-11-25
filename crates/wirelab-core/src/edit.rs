//! One structural circuit edit, applied with the electrical-safety verdict.
//!
//! Shared by the desktop's remote-edit endpoints and the phone plugin so both
//! reach the same verdict for the same op.

use serde_json::{Value, json};

use crate::circuit::{Circuit, CompId, Endpoint, PlacedComponent, WireId};
use crate::component::CompState;
use crate::library::Library;
use crate::netlist::{Netlist, WireVerdict, wire_verdict};

/// One structural edit, board-addressed. Endpoints use the same serde shape as
/// project files: `{"at":"board_pin","key":"GPIO2"}` /
/// `{"at":"terminal","comp":1,"terminal":"anode"}`.
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum EditOp {
    AddComp {
        def_id: String,
        pos: [f32; 2],
        #[serde(default)]
        label: String,
    },
    RemoveComp { comp: u32 },
    AddWire { a: Endpoint, b: Endpoint },
    RemoveWire { wire: u32 },
}

/// Apply one structural edit to a circuit, with the same electrical-safety
/// verdict the desktop and MCP enforce.
pub fn apply_edit(
    circuit: &mut Circuit,
    lib: &Library,
    live_output_gpios: &[u8],
    op: EditOp,
) -> Result<Value, String> {
    match op {
        EditOp::AddComp { def_id, pos, label } => {
            let def = lib
                .component(&def_id)
                .ok_or_else(|| format!("unknown component '{def_id}'"))?
                .clone();
            let id = circuit.add_component(PlacedComponent {
                id: CompId(0),
                def_id,
                pos: [pos[0].round(), pos[1].round()],
                rotation: 0,
                label,
                props: Default::default(),
                state: CompState::initial(&def.sim),
                script: None,
            });
            Ok(json!({ "ok": true, "comp": id.0 }))
        }
        EditOp::RemoveComp { comp } => {
            let id = CompId(comp);
            if !circuit.components.contains_key(&id) {
                return Err(format!("no component {comp}"));
            }
            circuit.remove_component(id);
            Ok(json!({ "ok": true, "removed": comp }))
        }
        EditOp::AddWire { a, b } => {
            let board = lib.board(&circuit.board_id).ok_or("no board profile")?;
            // Reject dangling endpoints so a malformed client can't wire to a
            // pin or terminal that doesn't exist on this board/component.
            let valid = |ep: &Endpoint| match ep {
                Endpoint::BoardPin { key } => board.pins.iter().any(|p| &p.key == key),
                Endpoint::Terminal { comp, terminal } => circuit
                    .components
                    .get(comp)
                    .and_then(|c| lib.component(&c.def_id))
                    .is_some_and(|d| d.terminals.iter().any(|t| &t.id == terminal)),
                Endpoint::Hole { bb, hole } => circuit
                    .breadboards
                    .get(bb)
                    .and_then(|b| lib.breadboard(&b.profile))
                    .is_some_and(|p| p.parse_hole(hole).is_some()),
            };
            if !valid(&a) || !valid(&b) {
                return Err("wire references an endpoint that doesn't exist".into());
            }
            let nl = Netlist::build(circuit, board, lib);
            match wire_verdict(&nl, board, &a, &b, live_output_gpios) {
                WireVerdict::Blocked(why) => Err(format!("refused: {why}")),
                WireVerdict::Redundant => Err("those points are already connected".into()),
                WireVerdict::Ok => {
                    let id = circuit.add_wire(a, b, [190, 120, 255]);
                    Ok(json!({ "ok": true, "wire": id.0 }))
                }
            }
        }
        EditOp::RemoveWire { wire } => {
            let id = WireId(wire);
            if !circuit.wires.contains_key(&id) {
                return Err(format!("no wire {wire}"));
            }
            circuit.remove_wire(id);
            Ok(json!({ "ok": true, "removed": wire }))
        }
    }
}
