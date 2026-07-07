//! World-space geometry shared by every renderer (desktop canvas, iPad
//! plugin): board pin and component terminal positions in mm.

use crate::board::{BoardPin, BoardProfile, Side};
use crate::circuit::PlacedComponent;
use crate::component::ComponentDef;

/// Rotate a local offset by a component rotation (quarter turns).
pub fn rotate(v: [f32; 2], deg: u16) -> [f32; 2] {
    match deg % 360 {
        90 => [-v[1], v[0]],
        180 => [-v[0], -v[1]],
        270 => [v[1], -v[0]],
        _ => v,
    }
}

/// Header pin position in world mm; USB end is the bottom of the board.
pub fn board_pin_world_pos(board: &BoardProfile, pin: &BoardPin, board_pos: [f32; 2]) -> [f32; 2] {
    let count = |side: Side| {
        board.pins.iter().filter(|p| p.side == side).map(|p| p.index).max().unwrap_or(0) + 1
    };
    let margin = 6.0;
    match pin.side {
        Side::Left | Side::Right => {
            let n = count(pin.side) as f32;
            let usable = board.height_mm - 2.0 * margin;
            let pitch = if n > 1.0 { usable / (n - 1.0) } else { 0.0 };
            let y = board_pos[1] + board.height_mm - margin - pin.index as f32 * pitch;
            let x = if pin.side == Side::Left {
                board_pos[0] + 1.5
            } else {
                board_pos[0] + board.width_mm - 1.5
            };
            [x, y]
        }
        Side::Top | Side::Bottom => {
            let n = count(pin.side) as f32;
            let usable = board.width_mm - 2.0 * margin;
            let pitch = if n > 1.0 { usable / (n - 1.0) } else { 0.0 };
            let x = board_pos[0] + margin + pin.index as f32 * pitch;
            let y = if pin.side == Side::Top {
                board_pos[1] + 1.5
            } else {
                board_pos[1] + board.height_mm - 1.5
            };
            [x, y]
        }
    }
}

/// Local terminal offsets in mm, by terminal index, before rotation.
pub fn terminal_offsets(def: &ComponentDef) -> Vec<[f32; 2]> {
    let w = def.visual.width_mm;
    let h = def.visual.height_mm;
    let n = def.terminals.len();
    match n {
        // Single-terminal parts (junction dots) attach at the centre.
        1 => vec![[0.0, 0.0]],
        2 => vec![[-w / 2.0, 0.0], [w / 2.0, 0.0]],
        3 => vec![[-w / 2.0, 0.0], [w / 2.0, 0.0], [0.0, h / 2.0]],
        _ => {
            let per_side = n.div_ceil(2);
            (0..n)
                .map(|i| {
                    let (side, j, m) = if i < per_side {
                        (-1.0, i, per_side)
                    } else {
                        (1.0, i - per_side, n - per_side)
                    };
                    let t = if m <= 1 { 0.5 } else { j as f32 / (m as f32 - 1.0) };
                    [side * w / 2.0, (t - 0.5) * h * 0.8]
                })
                .collect()
        }
    }
}

/// World-space terminal position for a placed component.
pub fn terminal_world_pos(comp: &PlacedComponent, def: &ComponentDef, index: usize) -> [f32; 2] {
    let off = terminal_offsets(def).get(index).copied().unwrap_or([0.0, 0.0]);
    let r = rotate(off, comp.rotation);
    [comp.pos[0] + r[0], comp.pos[1] + r[1]]
}
