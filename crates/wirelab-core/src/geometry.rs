//! World-space geometry shared by every renderer (desktop canvas, iPad
//! plugin): board pin and component terminal positions in mm.

use crate::board::{BoardPin, BoardProfile, Side};
use crate::circuit::{Circuit, Endpoint, PlacedComponent};
use crate::component::ComponentDef;
use crate::library::Library;

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

fn v_len(v: [f32; 2]) -> f32 {
    v[0].hypot(v[1])
}

fn v_add_scaled(p: [f32; 2], dir: [f32; 2], t: f32) -> [f32; 2] {
    [p[0] + dir[0] * t, p[1] + dir[1] * t]
}

fn v_normalized(v: [f32; 2]) -> [f32; 2] {
    let len = v_len(v);
    if len <= 0.0 { v } else { [v[0] / len, v[1] / len] }
}

/// How a wire leaves its endpoints and dodges its siblings.
#[derive(Debug, Clone, Copy)]
pub struct Route {
    /// Unit-ish exit direction at each end (away from the pad's body);
    /// `[0,0]` = no preference (routing dots, the mouse cursor).
    pub exit_a: [f32; 2],
    pub exit_b: [f32; 2],
    /// Small deterministic offset so parallel runs don't overlap.
    pub lane: i32,
    /// Stub length in pixels before the wire starts routing.
    pub stub: f32,
    /// Pixels needed to clear each endpoint's body when the route has to
    /// double back — keeps wires from cutting through their own component.
    pub clear_a: f32,
    pub clear_b: f32,
}

impl Default for Route {
    fn default() -> Self {
        Route {
            exit_a: [0.0, 0.0],
            exit_b: [0.0, 0.0],
            lane: 0,
            stub: 0.0,
            clear_a: 0.0,
            clear_b: 0.0,
        }
    }
}

/// Orthogonal wire path between two points, honoring exit stubs, with
/// rounded corners. Coordinate-agnostic ([f32;2]); callers pass whatever
/// space they draw in.
pub fn wire_path(a: [f32; 2], b: [f32; 2], route: &Route) -> Vec<[f32; 2]> {
    let snap = |v: [f32; 2]| -> [f32; 2] {
        if v_len(v) < 0.1 {
            [0.0, 0.0]
        } else if v[0].abs() >= v[1].abs() {
            [v[0].signum(), 0.0]
        } else {
            [0.0, v[1].signum()]
        }
    };
    let (ea, eb) = (snap(route.exit_a), snap(route.exit_b));
    let mut sa = v_add_scaled(a, ea, route.stub);
    let mut sb = v_add_scaled(b, eb, route.stub);
    let lane = route.lane as f32 * 7.0;

    // When the target sits BEHIND an exit direction, step sideways past the
    // body first instead of routing straight back through it.
    let dodge = |s: [f32; 2], e: [f32; 2], toward: [f32; 2], clear: f32| -> Option<[f32; 2]> {
        if e == [0.0, 0.0] || clear <= 0.0 {
            return None;
        }
        let behind = if e[1] == 0.0 {
            (toward[0] - s[0]) * e[0] < 0.0
        } else {
            (toward[1] - s[1]) * e[1] < 0.0
        };
        if !behind {
            return None;
        }
        if e[1] == 0.0 {
            let dir = if toward[1] >= s[1] { 1.0 } else { -1.0 };
            Some([s[0], s[1] + dir * clear])
        } else {
            let dir = if toward[0] >= s[0] { 1.0 } else { -1.0 };
            Some([s[0] + dir * clear, s[1]])
        }
    };
    let extra_a = dodge(sa, ea, sb, route.clear_a);
    if let Some(p) = extra_a {
        sa = p;
    }
    let extra_b = dodge(sb, eb, sa, route.clear_b);
    if let Some(p) = extra_b {
        sb = p;
    }

    // Fall back to geometry when an end has no exit preference.
    let axis_h = |e: [f32; 2], from: [f32; 2], to: [f32; 2]| {
        if e == [0.0, 0.0] {
            (to[0] - from[0]).abs() >= (to[1] - from[1]).abs()
        } else {
            e[1] == 0.0
        }
    };
    let mut ha = axis_h(ea, sa, sb);
    let mut hb = axis_h(eb, sb, sa);
    // After a dodge the leg leaves perpendicular to the original exit.
    if extra_a.is_some() {
        ha = ea[1] != 0.0;
    }
    if extra_b.is_some() {
        hb = eb[1] != 0.0;
    }

    let mut corners: Vec<[f32; 2]> = vec![a];
    if extra_a.is_some() {
        corners.push(v_add_scaled(a, ea, route.stub));
    }
    if sa != a {
        corners.push(sa);
    }
    match (ha, hb) {
        (true, true) => {
            let mid_x = (sa[0] + sb[0]) / 2.0 + lane;
            corners.push([mid_x, sa[1]]);
            corners.push([mid_x, sb[1]]);
        }
        (false, false) => {
            let mid_y = (sa[1] + sb[1]) / 2.0 + lane;
            corners.push([sa[0], mid_y]);
            corners.push([sb[0], mid_y]);
        }
        (true, false) => corners.push([sb[0], sa[1]]),
        (false, true) => corners.push([sa[0], sb[1]]),
    }
    if sb != b {
        corners.push(sb);
    }
    if extra_b.is_some() {
        corners.push(v_add_scaled(b, eb, route.stub));
    }
    corners.push(b);
    corners.dedup_by(|p, q| v_len([p[0] - q[0], p[1] - q[1]]) < 0.5);
    if corners.len() < 2 {
        return vec![a, b];
    }
    rounded_polyline(&corners, 9.0)
}

/// Densify a corner path into a polyline with quarter-round corners.
fn rounded_polyline(corners: &[[f32; 2]], radius: f32) -> Vec<[f32; 2]> {
    let mut pts = vec![corners[0]];
    for i in 1..corners.len().saturating_sub(1) {
        let (prev, p, next) = (corners[i - 1], corners[i], corners[i + 1]);
        let (d1, d2) = ([p[0] - prev[0], p[1] - prev[1]], [next[0] - p[0], next[1] - p[1]]);
        let r = radius.min(v_len(d1) * 0.5).min(v_len(d2) * 0.5);
        if r < 0.5 {
            pts.push(p);
            continue;
        }
        let n1 = v_normalized(d1);
        let n2 = v_normalized(d2);
        let enter = [p[0] - n1[0] * r, p[1] - n1[1] * r];
        let exit = [p[0] + n2[0] * r, p[1] + n2[1] * r];
        for k in 0..=6 {
            let t = k as f32 / 6.0;
            let u = 1.0 - t;
            pts.push([
                u * u * enter[0] + 2.0 * u * t * p[0] + t * t * exit[0],
                u * u * enter[1] + 2.0 * u * t * p[1] + t * t * exit[1],
            ]);
        }
    }
    pts.push(*corners.last().unwrap());
    pts
}

/// Which way a wire should leave this endpoint: away from the board edge
/// for pins, away from the body for component terminals. `[0,0]` = no
/// preference.
pub fn endpoint_exit_dir(
    circuit: &Circuit,
    board: &BoardProfile,
    lib: &Library,
    ep: &Endpoint,
) -> [f32; 2] {
    match ep {
        Endpoint::BoardPin { key } => {
            let Some(pin) = board.pin(key) else { return [0.0, 0.0] };
            match pin.side {
                Side::Left => [-1.0, 0.0],
                Side::Right => [1.0, 0.0],
                Side::Top => [0.0, -1.0],
                Side::Bottom => [0.0, 1.0],
            }
        }
        Endpoint::Terminal { comp, terminal } => {
            let Some(c) = circuit.components.get(comp) else { return [0.0, 0.0] };
            let Some(def) = lib.component(&c.def_id) else { return [0.0, 0.0] };
            let Some(idx) = def.terminals.iter().position(|t| &t.id == terminal) else {
                return [0.0, 0.0];
            };
            let t = terminal_world_pos(c, def, idx);
            [t[0] - c.pos[0], t[1] - c.pos[1]]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuit::{Circuit, Endpoint};
    use crate::library::Library;
    use std::path::Path;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    /// Longest axis-aligned legs of a densified path: collapse the small
    /// corner arcs by keeping only points where the dominant axis flips.
    fn legs(pts: &[[f32; 2]]) -> Vec<char> {
        let mut axes: Vec<char> = Vec::new();
        for w in pts.windows(2) {
            let d = [w[1][0] - w[0][0], w[1][1] - w[0][1]];
            if d[0].abs() < 1e-4 && d[1].abs() < 1e-4 {
                continue;
            }
            let axis = if d[0].abs() >= d[1].abs() { 'h' } else { 'v' };
            if axes.last() != Some(&axis) {
                axes.push(axis);
            }
        }
        axes
    }

    #[test]
    fn horizontal_exits_make_orthogonal_turns() {
        let route = Route {
            exit_a: [1.0, 0.0],
            exit_b: [-1.0, 0.0],
            stub: 10.0,
            ..Default::default()
        };
        let a = [0.0, 0.0];
        let b = [100.0, 40.0];
        let pts = wire_path(a, b, &route);
        assert_eq!(pts.first().copied(), Some(a));
        assert_eq!(pts.last().copied(), Some(b));
        // Three-segment route: horizontal out, vertical across, horizontal in.
        assert_eq!(legs(&pts), vec!['h', 'v', 'h'], "path: {pts:?}");
    }

    #[test]
    fn collinear_case_stays_simple() {
        let route = Route { exit_a: [1.0, 0.0], exit_b: [-1.0, 0.0], ..Default::default() };
        let a = [0.0, 0.0];
        let b = [50.0, 0.0];
        let pts = wire_path(a, b, &route);
        assert_eq!(pts.first().copied(), Some(a));
        assert_eq!(pts.last().copied(), Some(b));
        // A single horizontal run: same y throughout, one leg.
        assert!(pts.iter().all(|p| approx(p[1], 0.0)), "collinear stays flat: {pts:?}");
        assert_eq!(legs(&pts), vec!['h']);
    }

    #[test]
    fn board_pin_exit_dir_points_away_from_edge() {
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        let lib = Library::load(&assets.join("boards"), &assets.join("components"))
            .expect("assets load");
        let circuit = Circuit::new("esp32-c5-devkitc-1");
        let board = lib.board(&circuit.board_id).expect("board present");
        // Pick a known Left-side pin from the profile.
        let left = board
            .pins
            .iter()
            .find(|p| p.side == Side::Left)
            .expect("a left pin");
        let ep = Endpoint::BoardPin { key: left.key.clone() };
        let dir = endpoint_exit_dir(&circuit, board, &lib, &ep);
        assert_eq!(dir, [-1.0, 0.0]);
    }
}
