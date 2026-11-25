//! Solderable breadboards: hole grids, bussed strips and rails, and part seating.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::board::BoardProfile;
use crate::circuit::{BbId, Circuit, Endpoint, PlacedBreadboard};
use crate::geometry::{board_pin_world_pos, rotate, terminal_world_pos};
use crate::library::Library;

/// Standard 0.1" hole pitch.
pub const PITCH_MM: f32 = 2.54;

/// A lead counts as inserted when it lands this close to a hole centre.
pub const SEAT_TOL_MM: f32 = 0.6;

fn default_pitch() -> f32 {
    PITCH_MM
}

fn one() -> u16 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreadboardProfile {
    /// Stable identifier, e.g. "electrocookie-half".
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    pub width_mm: f32,
    pub height_mm: f32,
    #[serde(default)]
    pub corner_radius_mm: f32,
    #[serde(default = "default_pitch")]
    pub pitch_mm: f32,
    #[serde(default)]
    pub colors: BreadboardColors,
    pub lines: Vec<HoleLine>,
    #[serde(default)]
    pub mounts: Vec<MountHole>,
    #[serde(default)]
    pub silk: Silk,
    /// Build notes and unverified measurements.
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BreadboardColors {
    pub mask: [u8; 3],
    pub pad: [u8; 3],
    pub silk: [u8; 3],
}

impl Default for BreadboardColors {
    fn default() -> Self {
        BreadboardColors { mask: [22, 22, 24], pad: [208, 172, 84], silk: [236, 236, 232] }
    }
}

/// A straight run of evenly spaced holes, named `<name><index>`, e.g. `J12`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HoleLine {
    /// Hole name prefix; must not end in a digit.
    pub name: String,
    /// First hole centre in board mm from the top-left corner.
    pub start: [f32; 2],
    /// Offset from one hole to the next, in pitches.
    pub step: [f32; 2],
    pub count: u16,
    /// Printed index of the first hole.
    #[serde(default = "one")]
    pub first: u16,
    #[serde(default)]
    pub bus: Bus,
    /// Rail polarity printed on the board: "+", "-" or empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub polarity: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// How a line's holes are joined underneath.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Bus {
    /// Hole k joins hole k of every line in the same group.
    Strip { group: String },
    /// Every hole in the line is one node.
    Rail,
    /// Each hole stands alone.
    #[default]
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountHole {
    pub at: [f32; 2],
    pub dia_mm: f32,
    #[serde(default)]
    pub plated: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Silk {
    /// Hole numbers printed beside a line.
    #[serde(default)]
    pub numbers: Vec<SilkNumbers>,
    /// Line names printed beside the hole with a given index.
    #[serde(default)]
    pub names: Vec<SilkNames>,
    #[serde(default)]
    pub texts: Vec<SilkText>,
    /// Printed border inset from the board edge in mm; 0 = none.
    #[serde(default)]
    pub border_inset_mm: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SilkNumbers {
    pub line: String,
    pub offset: [f32; 2],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SilkNames {
    pub index: u16,
    pub offset: [f32; 2],
    /// Lines to label; empty = every strip line.
    #[serde(default)]
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SilkText {
    pub text: String,
    pub at: [f32; 2],
    pub size_mm: f32,
    /// Printed reading bottom-to-top.
    #[serde(default)]
    pub vertical: bool,
}

/// One hole: line index into `BreadboardProfile::lines` and printed index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HoleRef {
    pub line: usize,
    pub index: u16,
}

/// The copper node a hole belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NodeKey {
    Strip(String, u16),
    Rail(usize),
    Hole(usize, u16),
}

impl BreadboardProfile {
    pub fn hole_name(&self, h: HoleRef) -> String {
        format!("{}{}", self.lines[h.line].name, h.index)
    }

    /// Parse `J12`, `+A3` or `pjl1`; line names match case-insensitively.
    pub fn parse_hole(&self, name: &str) -> Option<HoleRef> {
        let name = name.trim();
        let split = name.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        let (prefix, digits) = name.split_at(split);
        let index: u16 = digits.parse().ok()?;
        let line = self
            .lines
            .iter()
            .position(|l| l.name == prefix)
            .or_else(|| self.lines.iter().position(|l| l.name.eq_ignore_ascii_case(prefix)))?;
        let l = &self.lines[line];
        (index >= l.first && index < l.first + l.count).then_some(HoleRef { line, index })
    }

    /// Hole centre in board mm from the top-left corner.
    pub fn hole_local(&self, h: HoleRef) -> [f32; 2] {
        let l = &self.lines[h.line];
        let k = f32::from(h.index - l.first) * self.pitch_mm;
        [l.start[0] + l.step[0] * k, l.start[1] + l.step[1] * k]
    }

    pub fn holes(&self) -> impl Iterator<Item = HoleRef> + '_ {
        self.lines.iter().enumerate().flat_map(|(line, l)| {
            (l.first..l.first + l.count).map(move |index| HoleRef { line, index })
        })
    }

    pub fn node_of(&self, h: HoleRef) -> NodeKey {
        match &self.lines[h.line].bus {
            Bus::Strip { group } => NodeKey::Strip(group.clone(), h.index),
            Bus::Rail => NodeKey::Rail(h.line),
            Bus::None => NodeKey::Hole(h.line, h.index),
        }
    }

    /// Every hole on the same copper as `h`, including `h`.
    pub fn bus_holes(&self, h: HoleRef) -> Vec<HoleRef> {
        let node = self.node_of(h);
        self.holes().filter(|&o| self.node_of(o) == node).collect()
    }

    /// Readable name for the copper a hole sits on, e.g. "strip 12 (A-E)".
    pub fn node_label(&self, h: HoleRef) -> String {
        match self.node_of(h) {
            NodeKey::Strip(group, i) => format!("strip {i} ({group})"),
            NodeKey::Rail(line) => {
                let l = &self.lines[line];
                if l.polarity.is_empty() {
                    format!("rail {}", l.name)
                } else {
                    format!("{} rail {}", l.polarity, l.name)
                }
            }
            NodeKey::Hole(..) => format!("pad {}", self.hole_name(h)),
        }
    }

    /// Closest hole to a board-local point, if within `tol_mm`.
    pub fn nearest_hole(&self, local: [f32; 2], tol_mm: f32) -> Option<HoleRef> {
        let mut best: Option<(HoleRef, f32)> = None;
        for (line, l) in self.lines.iter().enumerate() {
            let step = [l.step[0] * self.pitch_mm, l.step[1] * self.pitch_mm];
            let len2 = step[0] * step[0] + step[1] * step[1];
            let rel = [local[0] - l.start[0], local[1] - l.start[1]];
            let k = if len2 > 0.0 { (rel[0] * step[0] + rel[1] * step[1]) / len2 } else { 0.0 };
            let k = k.round().clamp(0.0, f32::from(l.count.saturating_sub(1)));
            let h = HoleRef { line, index: l.first + k as u16 };
            let p = self.hole_local(h);
            let d = (p[0] - local[0]).hypot(p[1] - local[1]);
            if d <= tol_mm && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((h, d));
            }
        }
        best.map(|(h, _)| h)
    }
}

/// Board-local mm to world mm for a placed breadboard.
pub fn local_to_world(bb: &PlacedBreadboard, p: &BreadboardProfile, local: [f32; 2]) -> [f32; 2] {
    let c = [local[0] - p.width_mm / 2.0, local[1] - p.height_mm / 2.0];
    let r = rotate(c, bb.rotation);
    [bb.pos[0] + r[0], bb.pos[1] + r[1]]
}

/// World mm to board-local mm for a placed breadboard.
pub fn world_to_local(bb: &PlacedBreadboard, p: &BreadboardProfile, world: [f32; 2]) -> [f32; 2] {
    let rel = [world[0] - bb.pos[0], world[1] - bb.pos[1]];
    let r = rotate(rel, (360 - bb.rotation % 360) % 360);
    [r[0] + p.width_mm / 2.0, r[1] + p.height_mm / 2.0]
}

pub fn hole_world(bb: &PlacedBreadboard, p: &BreadboardProfile, h: HoleRef) -> [f32; 2] {
    local_to_world(bb, p, p.hole_local(h))
}

/// World position of a named hole on a placed breadboard.
pub fn hole_endpoint_world(circuit: &Circuit, lib: &Library, bb: BbId, hole: &str) -> Option<[f32; 2]> {
    let placed = circuit.breadboards.get(&bb)?;
    let p = lib.breadboard(&placed.profile)?;
    Some(hole_world(placed, p, p.parse_hole(hole)?))
}

/// Topmost placed breadboard hole within `tol_mm` of a world point.
pub fn hole_at(circuit: &Circuit, lib: &Library, world: [f32; 2], tol_mm: f32) -> Option<(BbId, HoleRef)> {
    circuit.breadboards.values().rev().find_map(|bb| {
        let p = lib.breadboard(&bb.profile)?;
        p.nearest_hole(world_to_local(bb, p, world), tol_mm).map(|h| (bb.id, h))
    })
}

/// A component lead or header pin inserted into a breadboard hole.
#[derive(Debug, Clone, PartialEq)]
pub struct Seat {
    pub ep: Endpoint,
    pub bb: BbId,
    pub hole: HoleRef,
    pub hole_name: String,
}

/// A through-hole lead that sits over a breadboard but misses every hole.
#[derive(Debug, Clone, PartialEq)]
pub struct Miss {
    pub ep: Endpoint,
    pub bb: BbId,
    pub world: [f32; 2],
}

#[derive(Debug, Clone, Default)]
pub struct Seating {
    pub seats: Vec<Seat>,
    pub misses: Vec<Miss>,
}

impl Seating {
    pub fn seat_of(&self, ep: &Endpoint) -> Option<&Seat> {
        self.seats.iter().find(|s| &s.ep == ep)
    }
}

fn over_board(circuit: &Circuit, lib: &Library, world: [f32; 2]) -> Option<BbId> {
    circuit.breadboards.values().rev().find_map(|bb| {
        let p = lib.breadboard(&bb.profile)?;
        let l = world_to_local(bb, p, world);
        (l[0] >= 0.0 && l[1] >= 0.0 && l[0] <= p.width_mm && l[1] <= p.height_mm).then_some(bb.id)
    })
}

/// Which leads and header pins sit in breadboard holes. Only parts with
/// declared `leads` and boards with `header` geometry can be inserted.
pub fn seating(circuit: &Circuit, board: Option<&BoardProfile>, lib: &Library) -> Seating {
    let mut out = Seating::default();
    if circuit.breadboards.is_empty() {
        return out;
    }
    let mut probe = |ep: Endpoint, world: [f32; 2]| {
        if let Some((bb, hole)) = hole_at(circuit, lib, world, SEAT_TOL_MM) {
            let p = circuit.breadboards.get(&bb).and_then(|b| lib.breadboard(&b.profile));
            let hole_name = p.map(|p| p.hole_name(hole)).unwrap_or_default();
            out.seats.push(Seat { ep, bb, hole, hole_name });
        } else if let Some(bb) = over_board(circuit, lib, world) {
            out.misses.push(Miss { ep, bb, world });
        }
    };
    for comp in circuit.components.values() {
        let Some(def) = lib.component(&comp.def_id) else { continue };
        if def.leads.len() != def.terminals.len() || def.leads.is_empty() {
            continue;
        }
        for (i, t) in def.terminals.iter().enumerate() {
            let ep = Endpoint::Terminal { comp: comp.id, terminal: t.id.clone() };
            probe(ep, terminal_world_pos(comp, def, i));
        }
    }
    if let Some(board) = board.filter(|b| b.header.is_some()) {
        for pin in &board.pins {
            let ep = Endpoint::BoardPin { key: pin.key.clone() };
            probe(ep, board_pin_world_pos(board, pin, circuit.board_pos, circuit.board_rotation));
        }
    }
    out
}

/// Copper node of a hole endpoint, for netlist joins.
pub fn endpoint_node(circuit: &Circuit, lib: &Library, ep: &Endpoint) -> Option<(BbId, NodeKey)> {
    let Endpoint::Hole { bb, hole } = ep else { return None };
    let p = lib.breadboard(&circuit.breadboards.get(bb)?.profile)?;
    Some((*bb, p.node_of(p.parse_hole(hole)?)))
}

/// Semantic checks for a breadboard profile.
pub fn lint_breadboard(p: &BreadboardProfile) -> Vec<String> {
    let mut problems = Vec::new();
    let mut names = std::collections::HashSet::new();
    for l in &p.lines {
        if l.name.is_empty() || l.name.ends_with(|c: char| c.is_ascii_digit()) {
            problems.push(format!("line name {:?} is empty or ends in a digit", l.name));
        }
        if !names.insert(l.name.to_ascii_lowercase()) {
            problems.push(format!("duplicate line name {}", l.name));
        }
        if l.count == 0 {
            problems.push(format!("line {} has no holes", l.name));
        }
    }
    let mut seen: BTreeMap<(i64, i64), String> = BTreeMap::new();
    for h in p.holes() {
        let [x, y] = p.hole_local(h);
        if x < 0.0 || y < 0.0 || x > p.width_mm || y > p.height_mm {
            problems.push(format!("hole {} lies outside the board", p.hole_name(h)));
        }
        let key = ((x * 10.0).round() as i64, (y * 10.0).round() as i64);
        if let Some(other) = seen.insert(key, p.hole_name(h)) {
            problems.push(format!("holes {other} and {} overlap", p.hole_name(h)));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::circuit::PlacedBreadboard;

    fn assets() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets")
    }

    fn lib() -> Library {
        let a = assets();
        Library::load(&a.join("boards"), &a.join("components")).expect("assets")
    }

    #[test]
    fn shipped_profiles_lint_clean() {
        let lib = lib();
        assert!(lib.breadboards.len() >= 2);
        for p in lib.breadboards.values() {
            assert_eq!(lint_breadboard(p), Vec::<String>::new(), "{}", p.id);
        }
    }

    #[test]
    fn half_size_strips_and_rails() {
        let lib = lib();
        let p = lib.breadboard("electrocookie-half").unwrap();
        let e12 = p.parse_hole("E12").unwrap();
        let a12 = p.parse_hole("a12").unwrap();
        let f12 = p.parse_hole("F12").unwrap();
        assert_eq!(p.node_of(e12), p.node_of(a12));
        assert_ne!(p.node_of(e12), p.node_of(f12));
        assert_eq!(p.bus_holes(e12).len(), 5);
        let r1 = p.parse_hole("+A1").unwrap();
        let r30 = p.parse_hole("+A30").unwrap();
        assert_eq!(p.node_of(r1), p.node_of(r30));
        assert_eq!(p.bus_holes(r1).len(), 30);
        assert!(p.parse_hole("E31").is_none());
        let pitch = {
            let [x1, _] = p.hole_local(p.parse_hole("J1").unwrap());
            let [x2, _] = p.hole_local(p.parse_hole("J2").unwrap());
            x2 - x1
        };
        assert!((pitch - 2.54).abs() < 1e-4);
    }

    #[test]
    fn rotated_board_round_trips_and_snaps() {
        let lib = lib();
        let p = lib.breadboard("electrocookie-half").unwrap();
        for rot in [0, 90, 180, 270] {
            let bb = PlacedBreadboard {
                id: BbId(1),
                profile: p.id.clone(),
                pos: [40.0, -12.0],
                rotation: rot,
                label: String::new(),
            };
            let h = p.parse_hole("C7").unwrap();
            let w = hole_world(&bb, p, h);
            let back = world_to_local(&bb, p, w);
            let local = p.hole_local(h);
            assert!((back[0] - local[0]).abs() < 1e-3 && (back[1] - local[1]).abs() < 1e-3);
            let near = p.nearest_hole(world_to_local(&bb, p, [w[0] + 0.3, w[1] - 0.2]), SEAT_TOL_MM);
            assert_eq!(near, Some(h));
        }
    }
}
