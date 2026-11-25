//! Perfboard builds: hole-level placement, connectivity check against the netlist, SVG drawing.
//!
//! Locations are strings: `E12` (hole), `+@12` (rail at a row, `+@?` = row unknown),
//! `C1.pos` (solder straight to a part lead), `U2.GPIO1` (board pin) and `?` (not placed).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::circuit::Endpoint;
use crate::export::{DocCtx, part_ref, xml_escape};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfLayout {
    pub board: PerfBoard,
    #[serde(default)]
    pub parts: Vec<PerfPart>,
    #[serde(default)]
    pub runs: Vec<PerfRun>,
    #[serde(default)]
    pub areas: Vec<PerfArea>,
    /// Parts mounted off the board, drawn beside it.
    #[serde(default)]
    pub offboard: Vec<OffBoard>,
    #[serde(default)]
    pub notes: Vec<String>,
    /// Required copper clearance around high-voltage nets, mm.
    #[serde(default)]
    pub hv_clearance_mm: f32,
    /// Nets the clearance rule applies to.
    #[serde(default)]
    pub hv_nets: Vec<String>,
    /// Board drawing's top-left corner on the canvas.
    #[serde(default = "default_board_at")]
    pub board_at: [f32; 2],
    /// Left edge of the notes column on the canvas (0 = beside the board).
    #[serde(default)]
    pub notes_x: f32,
}

fn default_board_at() -> [f32; 2] {
    [120.0, 40.0]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfBoard {
    pub name: String,
    pub rows: u16,
    pub pitch_mm: f32,
    /// Column letters per bussed group, left to right, e.g. `["ABCDE", "FGHIJ"]`.
    pub groups: Vec<String>,
    /// Centre channel between groups, in pitches.
    pub channel_pitches: f32,
    /// Rails along the left edge, outermost first.
    #[serde(default)]
    pub rails_left: Vec<String>,
    /// Gap from the innermost rail to the first column, in pitches.
    #[serde(default = "default_rail_gap")]
    pub rail_gap_pitches: f32,
    #[serde(default)]
    pub note: String,
}

fn default_rail_gap() -> f32 {
    2.0
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// Position taken from the build notes.
    #[default]
    Given,
    /// Region known, exact holes not.
    Approximate,
    /// Not in the build notes; a proposal.
    Suggested,
    /// Not on the board yet.
    Unplaced,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfPart {
    /// Component label.
    pub part: String,
    /// Terminal id -> location.
    pub legs: BTreeMap<String, String>,
    #[serde(default)]
    pub status: Placement,
    #[serde(default)]
    pub note: String,
    /// Drawing position for parts without exact holes: `[column_index, row]`.
    #[serde(default)]
    pub draw_at: Option<[f32; 2]>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunSide {
    #[default]
    Top,
    Underside,
    Offboard,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfRun {
    pub id: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub spec: String,
    #[serde(default)]
    pub carries: String,
    #[serde(default)]
    pub side: RunSide,
    #[serde(default)]
    pub status: Placement,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerfArea {
    pub label: String,
    /// First and last row.
    pub rows: [u16; 2],
    /// First and last column letter, e.g. "FJ".
    pub cols: String,
    #[serde(default)]
    pub status: Placement,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OffBoard {
    /// Component label, or the board reference.
    pub part: String,
    /// Top-left corner on the drawing canvas.
    pub at: [f32; 2],
    #[serde(default)]
    pub note: String,
    /// Terminal ids drawn on the edge facing away from the board.
    #[serde(default)]
    pub flip: Vec<String>,
}

/// A parsed location.
#[derive(Debug, Clone, PartialEq)]
pub enum Loc {
    Hole { col: usize, row: u16 },
    Rail { rail: String, row: Option<u16> },
    Lead { part: String, terminal: String },
    Unplaced,
}

impl PerfBoard {
    pub fn columns(&self) -> Vec<char> {
        self.groups.iter().flat_map(|g| g.chars()).collect()
    }

    fn group_of(&self, col: usize) -> usize {
        let mut n = 0;
        for (g, s) in self.groups.iter().enumerate() {
            n += s.chars().count();
            if col < n {
                return g;
            }
        }
        self.groups.len().saturating_sub(1)
    }

    pub fn parse(&self, s: &str) -> Loc {
        let s = s.trim();
        if s == "?" || s.is_empty() {
            return Loc::Unplaced;
        }
        if let Some((rail, row)) = s.split_once('@') {
            return Loc::Rail { rail: rail.to_string(), row: row.parse().ok() };
        }
        let cols = self.columns();
        let mut ch = s.chars();
        if let Some(c0) = ch.next()
            && let Some(col) = cols.iter().position(|c| *c == c0.to_ascii_uppercase())
            && let Ok(row) = ch.as_str().parse::<u16>()
            && (1..=self.rows).contains(&row)
        {
            return Loc::Hole { col, row };
        }
        if let Some((part, terminal)) = s.split_once('.') {
            return Loc::Lead { part: part.to_string(), terminal: terminal.to_string() };
        }
        Loc::Unplaced
    }

    /// Hole centre in board millimetres from the board's top-left corner.
    pub fn hole_xy(&self, col: usize, row: u16) -> [f32; 2] {
        let p = self.pitch_mm;
        let x0 = self.margin() + self.rails_left.len() as f32 * p + (self.rail_gap_pitches - 1.0) * p;
        let mut x = x0;
        let mut idx = 0;
        'outer: for (g, group) in self.groups.iter().enumerate() {
            if g > 0 {
                x += (self.channel_pitches - 1.0) * p;
            }
            for _ in group.chars() {
                if idx == col {
                    break 'outer;
                }
                x += p;
                idx += 1;
            }
        }
        [x, self.margin() + (f32::from(row) - 1.0) * p]
    }

    pub fn rail_x(&self, rail: &str) -> Option<f32> {
        let i = self.rails_left.iter().position(|r| r == rail)?;
        Some(self.margin() + i as f32 * self.pitch_mm)
    }

    fn margin(&self) -> f32 {
        self.pitch_mm * 2.0
    }

    pub fn size(&self) -> [f32; 2] {
        let last = self.columns().len().saturating_sub(1);
        let [x, _] = self.hole_xy(last, 1);
        [x + self.margin(), self.margin() * 2.0 + (f32::from(self.rows) - 1.0) * self.pitch_mm]
    }
}

/// Union-find key for a location; `None` when unplaced.
fn node_key(board: &PerfBoard, loc: &Loc) -> Option<String> {
    match loc {
        Loc::Hole { col, row } => Some(format!("strip:{row}:{}", board.group_of(*col))),
        Loc::Rail { rail, .. } => Some(format!("rail:{rail}")),
        Loc::Lead { part, terminal } => Some(format!("lead:{part}.{terminal}")),
        Loc::Unplaced => None,
    }
}

#[derive(Default)]
struct Uf {
    parent: HashMap<String, String>,
}

impl Uf {
    fn add(&mut self, k: &str) {
        self.parent.entry(k.to_string()).or_insert_with(|| k.to_string());
    }

    fn find(&mut self, k: &str) -> String {
        self.add(k);
        let p = self.parent[k].clone();
        if p == k {
            return p;
        }
        let r = self.find(&p);
        self.parent.insert(k.to_string(), r.clone());
        r
    }

    fn union(&mut self, a: &str, b: &str) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent.insert(rb, ra);
        }
    }
}

/// Result of comparing the build against the schematic netlist.
#[derive(Debug, Default)]
pub struct PerfReport {
    /// Different schematic nets joined on the board.
    pub shorts: Vec<String>,
    /// Schematic connections the board does not make yet.
    pub missing: Vec<String>,
    /// Connections the board makes correctly.
    pub ok: Vec<String>,
    /// Copper within the clearance distance of a high-voltage net.
    pub clearance: Vec<String>,
    /// Placement notes (unplaced, suggested, approximate).
    pub placement: Vec<String>,
    /// Net name per occupied strip/rail key, for drawing.
    pub copper_nets: HashMap<String, String>,
}

fn build_uf(perf: &PerfLayout, ctx: &DocCtx) -> (Uf, Vec<String>) {
    let b = &perf.board;
    let mut uf = Uf::default();
    // A board's own rail pins are joined on the board itself.
    let board_ref = ctx.board_ref();
    let rail_of = |key: &str| -> Option<&'static str> {
        let rest = key.strip_prefix("lead:")?;
        let (part, pin) = rest.split_once('.')?;
        if part != board_ref {
            return None;
        }
        match ctx.board.pin(pin)?.kind {
            crate::board::PinKind::Gnd => Some("boardrail:gnd"),
            crate::board::PinKind::V3_3 => Some("boardrail:3v3"),
            crate::board::PinKind::V5 => Some("boardrail:5v"),
            _ => None,
        }
    };
    let mut leads = Vec::new();
    for p in &perf.parts {
        for (term, loc) in &p.legs {
            let lead = format!("lead:{}.{}", p.part, term);
            uf.add(&lead);
            leads.push(lead.clone());
            if let Some(k) = node_key(b, &b.parse(loc)) {
                uf.union(&lead, &k);
            }
        }
    }
    for r in &perf.runs {
        let (fa, fb) = (b.parse(&r.from), b.parse(&r.to));
        for l in [&fa, &fb] {
            if let Loc::Lead { .. } = l
                && let Some(k) = node_key(b, l)
            {
                uf.add(&k);
                leads.push(k);
            }
        }
        if let (Some(ka), Some(kb)) = (node_key(b, &fa), node_key(b, &fb)) {
            uf.union(&ka, &kb);
        }
    }
    leads.sort();
    leads.dedup();
    for l in &leads {
        if let Some(rail) = rail_of(l) {
            uf.union(l, rail);
        }
    }
    (uf, leads)
}

/// Circuit endpoint a `lead:PART.TERM` key stands for.
fn lead_endpoint(ctx: &DocCtx, key: &str) -> Option<Endpoint> {
    let rest = key.strip_prefix("lead:")?;
    let (part, term) = rest.split_once('.')?;
    if part == ctx.board_ref() {
        return ctx.board.pin(term).map(|p| Endpoint::BoardPin { key: p.key.clone() });
    }
    let c = ctx.circuit.components.values().find(|c| part_ref(c) == part)?;
    let d = ctx.lib.component(&c.def_id)?;
    let t = d.terminals.iter().find(|t| t.id == term || t.name == term)?;
    Some(Endpoint::Terminal { comp: c.id, terminal: t.id.clone() })
}

fn pretty_lead(key: &str) -> String {
    key.strip_prefix("lead:").unwrap_or(key).to_string()
}

/// Net name for each run, by the circuit net its copper ends up on.
pub fn run_nets(perf: &PerfLayout, ctx: &DocCtx) -> Vec<String> {
    let (mut uf, leads) = build_uf(perf, ctx);
    let mut root_net: HashMap<String, String> = HashMap::new();
    for l in &leads {
        if let Some(ep) = lead_endpoint(ctx, l)
            && let Some(n) = ctx.nl.net_of(&ep)
        {
            let r = uf.find(l);
            root_net.entry(r).or_insert_with(|| ctx.net_name(n).to_string());
        }
    }
    perf.runs
        .iter()
        .map(|r| {
            let loc = perf.board.parse(&r.from);
            let loc = if loc == Loc::Unplaced { perf.board.parse(&r.to) } else { loc };
            node_key(&perf.board, &loc)
                .map(|k| uf.find(&k))
                .and_then(|root| root_net.get(&root).cloned())
                .unwrap_or_default()
        })
        .collect()
}

/// Compares the build's copper connectivity with the schematic netlist.
pub fn check(perf: &PerfLayout, ctx: &DocCtx, net_mv: Option<&[Option<f32>]>) -> PerfReport {
    let b = &perf.board;
    let mut rep = PerfReport::default();
    let (mut uf, leads) = build_uf(perf, ctx);

    // Lead -> circuit net.
    let mut lead_net: BTreeMap<String, usize> = BTreeMap::new();
    for l in &leads {
        match lead_endpoint(ctx, l) {
            Some(ep) => {
                if let Some(n) = ctx.nl.net_of(&ep) {
                    lead_net.insert(l.clone(), n);
                }
            }
            None => rep.shorts.push(format!("{}: no such part pin in the schematic", pretty_lead(l))),
        }
    }

    // Shorts: one copper group spanning several schematic nets.
    let mut groups: BTreeMap<String, Vec<(String, usize)>> = BTreeMap::new();
    for (l, &n) in &lead_net {
        groups.entry(uf.find(l)).or_default().push((l.clone(), n));
    }
    for members in groups.values() {
        let nets: BTreeSet<usize> = members.iter().map(|m| m.1).collect();
        if nets.len() > 1 {
            let desc: Vec<String> = members
                .iter()
                .map(|(l, n)| format!("{} ({})", pretty_lead(l), ctx.net_name(*n)))
                .collect();
            rep.shorts.push(format!("joined on the board but on different nets: {}", desc.join(", ")));
        }
    }

    // Missing links: one schematic net split across copper groups, or a member not on the board.
    let mut by_net: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (l, &n) in &lead_net {
        by_net.entry(n).or_default().push(l.clone());
    }
    let placed: BTreeSet<String> = lead_net.keys().cloned().collect();
    for (&net, members) in &by_net {
        let mut comps: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for l in members {
            comps.entry(uf.find(l)).or_default().push(pretty_lead(l));
        }
        let name = ctx.net_name(net);
        if comps.len() > 1 {
            let parts: Vec<String> = comps.values().map(|v| format!("[{}]", v.join(", "))).collect();
            rep.missing.push(format!("{name}: not joined on the board: {}", parts.join(" vs ")));
        } else if members.len() > 1 {
            rep.ok.push(format!("{name}: {}", members.iter().map(|l| pretty_lead(l)).collect::<Vec<_>>().join(" = ")));
        }
        // Schematic members of this net that never appear on the build.
        let mut absent = Vec::new();
        for ep in &ctx.nl.nets[net].endpoints {
            let key = match ep {
                Endpoint::Terminal { comp, terminal } => {
                    let Some(c) = ctx.circuit.components.get(comp) else { continue };
                    if ctx.layout.exclude.contains(&c.label) {
                        continue;
                    }
                    let Some(d) = ctx.lib.component(&c.def_id) else { continue };
                    if d.visual.shape == crate::component::Shape::Dot {
                        continue;
                    }
                    format!("lead:{}.{}", part_ref(c), terminal)
                }
                Endpoint::BoardPin { key } => {
                    if ctx.circuit.wires_at(ep).next().is_none() {
                        continue;
                    }
                    format!("lead:{}.{}", ctx.board_ref(), key)
                }
                Endpoint::Hole { .. } => continue,
            };
            if !placed.contains(&key) {
                absent.push(pretty_lead(&key));
            }
        }
        if !absent.is_empty() {
            rep.missing.push(format!("{name}: not on the build yet: {}", absent.join(", ")));
        }
    }

    // Copper nets for drawing.
    for (l, &n) in &lead_net {
        let root = uf.find(l);
        for key in uf.parent.keys().cloned().collect::<Vec<_>>() {
            if (key.starts_with("strip:") || key.starts_with("rail:")) && uf.find(&key) == root {
                rep.copper_nets.insert(key, ctx.net_name(n).to_string());
            }
        }
    }

    // Clearance: copper centre lines closer than the rule to a high-voltage net.
    if perf.hv_clearance_mm > 0.0 {
        let feats = copper_features(perf);
        let volts = |net: &str| -> Option<f32> {
            let mv = net_mv?;
            let id = ctx.net_names.iter().position(|n| n == net)?;
            mv.get(id).copied().flatten().map(|v| v / 1000.0)
        };
        let mut seen = BTreeSet::new();
        for (ka, ra) in &feats {
            let Some(na) = rep.copper_nets.get(ka) else { continue };
            if !perf.hv_nets.contains(na) {
                continue;
            }
            for (kb, rb) in &feats {
                if ka == kb {
                    continue;
                }
                let nb = rep.copper_nets.get(kb);
                if nb == Some(na) {
                    continue;
                }
                let d = rect_gap(*ra, *rb);
                if d >= perf.hv_clearance_mm {
                    continue;
                }
                let pair = if ka < kb { (ka.clone(), kb.clone()) } else { (kb.clone(), ka.clone()) };
                if !seen.insert(pair) {
                    continue;
                }
                let other = match nb {
                    Some(n) => {
                        let dv = match (volts(na), volts(n)) {
                            (Some(a), Some(b)) => format!(", {:.0} V apart", (a - b).abs()),
                            _ => String::new(),
                        };
                        format!("{} ({n}{dv})", describe(kb))
                    }
                    None => format!("{} (unused copper)", describe(kb)),
                };
                rep.clearance.push(format!(
                    "{} ({na}) is {:.2} mm centre-to-centre from {other}; rule is {:.1} mm edge-to-edge",
                    describe(ka),
                    d,
                    perf.hv_clearance_mm
                ));
            }
        }
    }

    for p in &perf.parts {
        match p.status {
            Placement::Given => {}
            Placement::Approximate => rep.placement.push(format!("{}: approximate — {}", p.part, p.note)),
            Placement::Suggested => rep.placement.push(format!("{}: SUGGESTED, not in the build notes — {}", p.part, p.note)),
            Placement::Unplaced => rep.placement.push(format!("{}: not placed — {}", p.part, p.note)),
        }
    }
    let _ = b;
    rep
}

fn describe(key: &str) -> String {
    if let Some(rest) = key.strip_prefix("strip:") {
        let mut it = rest.split(':');
        let row = it.next().unwrap_or("?");
        let group = it.next().unwrap_or("0");
        return format!("row {row} strip {}", if group == "0" { "A-E" } else { "F-J" });
    }
    if let Some(rail) = key.strip_prefix("rail:") {
        return format!("{rail} rail");
    }
    key.to_string()
}

/// Copper centre-line segments as `[x0, y0, x1, y1]` rectangles of zero width.
fn copper_features(perf: &PerfLayout) -> Vec<(String, [f32; 4])> {
    let b = &perf.board;
    let mut v = Vec::new();
    let cols = b.columns();
    for row in 1..=b.rows {
        let mut start = 0;
        for (g, group) in b.groups.iter().enumerate() {
            let n = group.chars().count();
            let [x0, y] = b.hole_xy(start, row);
            let [x1, _] = b.hole_xy(start + n - 1, row);
            v.push((format!("strip:{row}:{g}"), [x0, y, x1, y]));
            start += n;
        }
    }
    let _ = cols;
    let [_, y_last] = b.hole_xy(0, b.rows);
    let [_, y_first] = b.hole_xy(0, 1);
    for r in &b.rails_left {
        if let Some(x) = b.rail_x(r) {
            v.push((format!("rail:{r}"), [x, y_first, x, y_last]));
        }
    }
    v
}

/// Shortest distance between two axis-aligned segments/rectangles.
fn rect_gap(a: [f32; 4], b: [f32; 4]) -> f32 {
    let (ax0, ax1) = (a[0].min(a[2]), a[0].max(a[2]));
    let (ay0, ay1) = (a[1].min(a[3]), a[1].max(a[3]));
    let (bx0, bx1) = (b[0].min(b[2]), b[0].max(b[2]));
    let (by0, by1) = (b[1].min(b[3]), b[1].max(b[3]));
    let dx = (bx0 - ax1).max(ax0 - bx1).max(0.0);
    let dy = (by0 - ay1).max(ay0 - by1).max(0.0);
    dx.hypot(dy)
}

pub fn report_markdown(rep: &PerfReport) -> String {
    let mut s = String::from("# Perfboard check\n\nThe build's copper connectivity compared with the schematic netlist.\n\n");
    let mut section = |title: &str, items: &[String], empty: &str| {
        let _ = writeln!(s, "## {title}\n");
        if items.is_empty() {
            let _ = writeln!(s, "{empty}\n");
        } else {
            for i in items {
                let _ = writeln!(s, "- {i}");
            }
            s.push('\n');
        }
    };
    section("Shorts (board joins different nets)", &rep.shorts, "None.");
    section("Missing or unbuilt connections", &rep.missing, "None.");
    section("High-voltage clearance", &rep.clearance, "No copper inside the clearance distance.");
    section("Placement status", &rep.placement, "Every part placed from the build notes.");
    section("Verified connections", &rep.ok, "None.");
    s
}

fn color_hex(name: &str) -> &'static str {
    match name.to_ascii_lowercase().as_str() {
        "red" => "#d22f2f",
        "black" => "#222222",
        "blue" => "#2f6fd2",
        "green" => "#2e9e4a",
        "brown" => "#8b5a2b",
        "white" => "#f4f4f4",
        "yellow" => "#e0c020",
        "orange" => "#e8862a",
        "purple" => "#8a4fc7",
        "gray" | "grey" => "#888888",
        _ => "#555555",
    }
}

struct Drawn {
    /// Canvas position of each `lead:PART.TERM` key.
    leads: HashMap<String, [f32; 2]>,
}

/// Board drawing magnification relative to the off-board parts.
const BOARD_SCALE: f32 = 3.0;

/// The build as an SVG drawing: board magnified, off-board parts and flying leads around it.
pub fn svg(perf: &PerfLayout, ctx: &DocCtx, rep: &PerfReport) -> String {
    let b = &perf.board;
    let k = BOARD_SCALE;
    let [bw, bh] = b.size();
    let p = b.pitch_mm;
    let (ox, oy) = (perf.board_at[0], perf.board_at[1]);
    let to_c = |xy: [f32; 2]| [ox + xy[0] * k, oy + xy[1] * k];
    let notes_x = perf.notes_x.max(ox + bw * k + 30.0);
    let width = notes_x + 230.0;
    let mut s = String::new();
    let mut drawn = Drawn { leads: HashMap::new() };
    let cols = b.columns();

    // ---- board, in board millimetres inside a scaled group ----
    let mut g = String::new();
    let _ = writeln!(
        g,
        r##"<rect x="0" y="0" width="{bw}" height="{bh}" rx="1.2" fill="#f6f1e3" stroke="#9a8f70" stroke-width="0.25"/>"##
    );
    if perf.hv_clearance_mm > 0.0 {
        let c = perf.hv_clearance_mm;
        for (key, r) in copper_features(perf) {
            if rep.copper_nets.get(&key).is_none_or(|n| !perf.hv_nets.contains(n)) {
                continue;
            }
            let _ = writeln!(
                g,
                r##"<rect x="{}" y="{}" width="{}" height="{}" rx="{c}" fill="url(#hatch)" stroke="#c0392b" stroke-width="0.12" stroke-dasharray="0.6 0.4"/>"##,
                r[0].min(r[2]) - c,
                r[1].min(r[3]) - c,
                (r[2] - r[0]).abs() + 2.0 * c,
                (r[3] - r[1]).abs() + 2.0 * c
            );
        }
    }
    for (key, r) in copper_features(perf) {
        let used = rep.copper_nets.get(&key);
        let fill = match used {
            Some(n) if perf.hv_nets.contains(n) => "#e7a07a",
            Some(_) => "#e2bf8a",
            None => "#efe2c6",
        };
        let w = p * 0.62;
        let _ = writeln!(
            g,
            r##"<rect x="{}" y="{}" width="{}" height="{}" rx="{}" fill="{fill}"/>"##,
            r[0].min(r[2]) - w / 2.0,
            r[1].min(r[3]) - w / 2.0,
            (r[2] - r[0]).abs() + w,
            (r[3] - r[1]).abs() + w,
            w / 2.0
        );
        if let Some(net) = used
            && key.starts_with("strip:")
        {
            let _ = writeln!(
                g,
                r##"<text x="{}" y="{}" font-size="0.9" fill="#7a4b20" text-anchor="middle">{}</text>"##,
                (r[0] + r[2]) / 2.0,
                r[1] - 0.95,
                xml_escape(net)
            );
        }
    }
    for r in &b.rails_left {
        let Some(x) = b.rail_x(r) else { continue };
        let (color, glyph) = if r == "+" { ("#c0392b", "+") } else { ("#2f6fd2", "−") };
        for y in [b.margin() * 0.55, bh - b.margin() * 0.2] {
            let _ = writeln!(
                g,
                r##"<text x="{x}" y="{y}" font-size="2.6" font-weight="bold" text-anchor="middle" fill="{color}">{glyph}</text>"##
            );
        }
    }
    for row in 1..=b.rows {
        for col in 0..cols.len() {
            let [x, y] = b.hole_xy(col, row);
            let _ = writeln!(g, r##"<circle cx="{x}" cy="{y}" r="0.42" fill="#5a5040"/>"##);
        }
        for r in &b.rails_left {
            if let Some(x) = b.rail_x(r) {
                let y = b.hole_xy(0, row)[1];
                let _ = writeln!(g, r##"<circle cx="{x}" cy="{y}" r="0.42" fill="#5a5040"/>"##);
            }
        }
        let [xl, y] = b.hole_xy(0, row);
        let [xr, _] = b.hole_xy(cols.len() - 1, row);
        let weight = if row % 5 == 0 { "bold" } else { "normal" };
        let _ = writeln!(
            g,
            r##"<text x="{}" y="{}" font-size="1.25" text-anchor="end" font-weight="{weight}" fill="#6b5f45">{row}</text><text x="{}" y="{}" font-size="1.25" font-weight="{weight}" fill="#6b5f45">{row}</text>"##,
            xl - 1.0,
            y + 0.45,
            xr + 1.0,
            y + 0.45
        );
    }
    for (col, c) in cols.iter().enumerate() {
        let [x, y] = b.hole_xy(col, 1);
        let [_, y2] = b.hole_xy(col, b.rows);
        let _ = writeln!(
            g,
            r##"<text x="{x}" y="{}" font-size="1.5" text-anchor="middle" fill="#6b5f45">{c}</text><text x="{x}" y="{}" font-size="1.5" text-anchor="middle" fill="#6b5f45">{c}</text>"##,
            y - 1.7,
            y2 + 2.7
        );
    }
    for a in &perf.areas {
        let mut ch = a.cols.chars();
        let (Some(c0), Some(c1)) = (ch.next(), ch.next()) else { continue };
        let (Some(i0), Some(i1)) = (cols.iter().position(|c| *c == c0), cols.iter().position(|c| *c == c1)) else {
            continue;
        };
        let [x0, y0] = b.hole_xy(i0, a.rows[0]);
        let [x1, y1] = b.hole_xy(i1, a.rows[1]);
        let dash = if a.status == Placement::Given { "" } else { r#" stroke-dasharray="0.8 0.5""# };
        let _ = writeln!(
            g,
            r##"<rect x="{}" y="{}" width="{}" height="{}" rx="0.6" fill="#2f6fd2" fill-opacity="0.06" stroke="#2f6fd2" stroke-width="0.2"{dash}/>"##,
            x0 - 1.3,
            y0 - 1.3,
            x1 - x0 + 2.6,
            y1 - y0 + 2.6
        );
        for (i, line) in wrap(&a.label, 16).iter().chain(wrap(&a.note, 22).iter()).enumerate() {
            let _ = writeln!(
                g,
                r##"<text x="{}" y="{}" font-size="{}" fill="#1f4f9a" text-anchor="middle" stroke="#f6f1e3" stroke-width="0.3" paint-order="stroke">{}</text>"##,
                (x0 + x1) / 2.0,
                y0 + 2.2 + i as f32 * 1.6,
                if i == 0 { 1.3 } else { 1.0 },
                xml_escape(line)
            );
        }
    }

    // On-board parts.
    for part in &perf.parts {
        let comp = ctx.circuit.components.values().find(|c| part_ref(c) == part.part);
        let def = comp.and_then(|c| ctx.lib.component(&c.def_id));
        let value = match (comp, def) {
            (Some(c), Some(d)) => crate::export::part_value(c, d),
            _ => String::new(),
        };
        let (stroke, dash) = match part.status {
            Placement::Given => ("#1b1b1b", ""),
            Placement::Approximate => ("#7a5a00", r#" stroke-dasharray="0.6 0.4""#),
            Placement::Suggested => ("#1f6f9a", r#" stroke-dasharray="0.4 0.4""#),
            Placement::Unplaced => ("#999999", r#" stroke-dasharray="0.3 0.45""#),
        };
        let holes: Vec<(String, [f32; 2])> = part
            .legs
            .iter()
            .filter_map(|(t, loc)| match b.parse(loc) {
                Loc::Hole { col, row } => Some((t.clone(), b.hole_xy(col, row))),
                _ => None,
            })
            .collect();
        if holes.len() == 2 {
            let ([ax, ay], [bx, by]) = (holes[0].1, holes[1].1);
            let (mx, my) = ((ax + bx) / 2.0, (ay + by) / 2.0);
            let len = (bx - ax).hypot(by - ay);
            let ang = (by - ay).atan2(bx - ax).to_degrees();
            let _ = writeln!(
                g,
                r##"<line x1="{ax}" y1="{ay}" x2="{bx}" y2="{by}" stroke="#8a8a8a" stroke-width="0.3"/><g transform="translate({mx} {my}) rotate({ang})"><rect x="{}" y="-0.75" width="{}" height="1.5" rx="0.6" fill="#d8c8a0" stroke="{stroke}" stroke-width="0.2"{dash}/></g>"##,
                -len * 0.3,
                len * 0.6
            );
            for (term, xy) in &holes {
                let _ = writeln!(
                    g,
                    r##"<circle cx="{}" cy="{}" r="0.55" fill="#c0c0c0" stroke="#333" stroke-width="0.15"/>"##,
                    xy[0],
                    xy[1]
                );
                drawn.leads.insert(format!("lead:{}.{}", part.part, term), to_c(*xy));
            }
            let side = if (bx - ax).abs() < 0.1 { 1.0 } else { 0.0 };
            let _ = writeln!(
                g,
                r##"<text x="{}" y="{}" font-size="1.4" font-weight="bold" fill="{stroke}" stroke="#f6f1e3" stroke-width="0.3" paint-order="stroke">{} <tspan font-weight="normal" font-size="1.1">{}</tspan></text>"##,
                mx + side * 1.4,
                my + 0.5 - (1.0 - side) * 1.6,
                xml_escape(&part.part),
                xml_escape(&value)
            );
        } else if let Some([cx, cy]) = part.draw_at {
            let [x, y] = b.hole_xy(cx.max(0.0) as usize, cy.max(1.0) as u16);
            let x = x + (cx.fract()) * p;
            let terms: Vec<(String, String)> = match def {
                Some(d) => d.terminals.iter().map(|t| (t.id.clone(), t.name.clone())).collect(),
                None => part.legs.keys().map(|t| (t.clone(), t.clone())).collect(),
            };
            let r = 3.2;
            let _ = writeln!(
                g,
                r##"<circle cx="{x}" cy="{y}" r="{r}" fill="#30343c" fill-opacity="0.88" stroke="{stroke}" stroke-width="0.25"{dash}/><rect x="{}" y="{}" width="1.1" height="{}" fill="#9aa4b4"/><text x="{x}" y="{}" font-size="1.5" text-anchor="middle" fill="#fff" font-weight="bold">{}</text><text x="{x}" y="{}" font-size="1.0" text-anchor="middle" fill="#ddd">{}</text>"##,
                x + r - 1.6,
                y - r * 0.7,
                r * 1.4,
                y - 0.2,
                xml_escape(&part.part),
                y + 1.4,
                xml_escape(&value)
            );
            for (i, (tid, tname)) in terms.iter().enumerate() {
                let dx = if i == 0 { -1.4 } else { 1.4 };
                let xy = [x + dx, y + r + 0.9];
                let _ = writeln!(
                    g,
                    r##"<circle cx="{}" cy="{}" r="0.5" fill="#c0c0c0" stroke="#333" stroke-width="0.15"/><text x="{}" y="{}" font-size="1.0" text-anchor="middle" font-weight="bold">{}</text>"##,
                    xy[0],
                    xy[1],
                    xy[0],
                    xy[1] + 1.8,
                    xml_escape(tname)
                );
                drawn.leads.insert(format!("lead:{}.{}", part.part, tid), to_c(xy));
            }
        }
    }

    // ---- canvas ----
    let _ = writeln!(
        s,
        r##"<defs><pattern id="hatch" width="0.9" height="0.9" patternUnits="userSpaceOnUse" patternTransform="rotate(45)"><line x1="0" y1="0" x2="0" y2="0.9" stroke="#c0392b" stroke-width="0.22" opacity="0.55"/></pattern></defs>"##
    );
    let title = if ctx.layout.title.is_empty() { ctx.name.to_string() } else { ctx.layout.title.clone() };
    let _ = writeln!(
        s,
        r##"<text x="8" y="14" font-size="9" font-weight="bold">{} — perfboard placement</text><text x="8" y="23" font-size="4.2" fill="#444">{} · top view · board drawn {k}× · nominal 2.54 mm geometry, verify against the board · rev {} · {}</text>"##,
        xml_escape(&title),
        xml_escape(&b.name),
        xml_escape(&ctx.layout.revision),
        xml_escape(&ctx.layout.date)
    );
    let _ = writeln!(s, r##"<g transform="translate({ox} {oy}) scale({k})">{g}</g>"##);

    // Off-board parts.
    let board_cx = ox + bw * k / 2.0;
    for o in &perf.offboard {
        let is_board = o.part == ctx.board_ref();
        let terms: Vec<(String, String)> = if is_board {
            perf.runs
                .iter()
                .flat_map(|r| [&r.from, &r.to])
                .filter_map(|l| match b.parse(l) {
                    Loc::Lead { part, terminal } if part == o.part => Some((terminal.clone(), terminal)),
                    _ => None,
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        } else {
            ctx.circuit
                .components
                .values()
                .find(|c| part_ref(c) == o.part)
                .and_then(|c| ctx.lib.component(&c.def_id))
                .map(|d| d.terminals.iter().map(|t| (t.id.clone(), t.name.clone())).collect())
                .unwrap_or_default()
        };
        let name = if is_board {
            ctx.board.name.clone()
        } else {
            ctx.circuit
                .components
                .values()
                .find(|c| part_ref(c) == o.part)
                .and_then(|c| ctx.lib.component(&c.def_id))
                .map(|d| d.name.clone())
                .unwrap_or_default()
        };
        let w = 62.0;
        let [x, y] = o.at;
        let faces_right = x + w / 2.0 < board_cx;
        let (mut n_main, mut n_flip) = (0, 0);
        for (tid, _) in &terms {
            if o.flip.contains(tid) {
                n_flip += 1;
            } else {
                n_main += 1;
            }
        }
        let h = 15.0 + n_main.max(n_flip) as f32 * 6.0;
        let _ = writeln!(
            s,
            r##"<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="2" fill="#eef1f6" stroke="#55606e" stroke-width="0.5"/><text x="{}" y="{}" font-size="5" font-weight="bold" text-anchor="middle">{}</text><text x="{}" y="{}" font-size="2.6" fill="#444" text-anchor="middle">{}</text>"##,
            x + w / 2.0,
            y + 6.5,
            xml_escape(&o.part),
            x + w / 2.0,
            y + 10.5,
            xml_escape(&truncate(&name, 40))
        );
        let (mut i_main, mut i_flip) = (0, 0);
        for (tid, tname) in &terms {
            let flipped = o.flip.contains(tid);
            let i = if flipped { &mut i_flip } else { &mut i_main };
            let ty = y + 16.0 + *i as f32 * 6.0;
            *i += 1;
            let right = faces_right != flipped;
            let tx = if right { x + w } else { x };
            let _ = writeln!(
                s,
                r##"<circle cx="{tx}" cy="{ty}" r="1.3" fill="#c9a24a" stroke="#6b5f45" stroke-width="0.35"/><text x="{}" y="{}" font-size="3" text-anchor="{}">{}</text>"##,
                if right { tx - 2.8 } else { tx + 2.8 },
                ty + 1.0,
                if right { "end" } else { "start" },
                xml_escape(tname)
            );
            drawn.leads.insert(format!("lead:{}.{}", o.part, tid), [tx, ty]);
            drawn.leads.insert(format!("lead:{}.{}", o.part, tname), [tx, ty]);
        }
        if !o.note.is_empty() {
            let _ = writeln!(
                s,
                r##"<text x="{}" y="{}" font-size="2.6" fill="#7a2a1a" text-anchor="middle">{}</text>"##,
                x + w / 2.0,
                y + h + 4.0,
                xml_escape(&o.note)
            );
        }
    }

    // Parts with no position at all: a list under the board.
    let tray: Vec<&PerfPart> = perf
        .parts
        .iter()
        .filter(|p| p.draw_at.is_none() && p.legs.values().all(|l| b.parse(l) == Loc::Unplaced))
        .collect();
    let mut bottom = oy + bh * k;
    if !tray.is_empty() {
        let ty = bottom + 10.0;
        let _ = writeln!(s, r##"<text x="{ox}" y="{ty}" font-size="3.6" font-weight="bold" fill="#7a2a1a">Not placed in the build notes</text>"##);
        for (i, part) in tray.iter().enumerate() {
            let _ = writeln!(
                s,
                r##"<text x="{}" y="{}" font-size="3" fill="#7a2a1a">{} — {}</text>"##,
                ox + 2.0,
                ty + 5.5 + i as f32 * 4.6,
                xml_escape(&part.part),
                xml_escape(&part.note)
            );
        }
        bottom = ty + 5.5 + tray.len() as f32 * 4.6;
    }

    // Runs.
    let resolve = |loc: &Loc, toward: Option<[f32; 2]>| -> Option<[f32; 2]> {
        match loc {
            Loc::Hole { col, row } => Some(to_c(b.hole_xy(*col, *row))),
            Loc::Rail { rail, row } => {
                let x = b.rail_x(rail)?;
                let y = match (row, toward) {
                    (Some(r), _) => b.hole_xy(0, *r)[1],
                    (None, Some(t)) => ((t[1] - oy) / k).clamp(b.hole_xy(0, 1)[1], b.hole_xy(0, b.rows)[1]),
                    (None, None) => b.hole_xy(0, b.rows / 2)[1],
                };
                Some(to_c([x, y]))
            }
            Loc::Lead { part, terminal } => drawn.leads.get(&format!("lead:{part}.{terminal}")).copied(),
            Loc::Unplaced => None,
        }
    };
    for r in &perf.runs {
        let (la, lb) = (b.parse(&r.from), b.parse(&r.to));
        let pb0 = resolve(&lb, None);
        let pa = resolve(&la, pb0);
        let pb = resolve(&lb, pa);
        let (Some([ax, ay]), Some([bx, by])) = (pa, pb) else { continue };
        let col = color_hex(&r.color);
        let width = if r.spec.contains("12 AWG") { 2.4 } else { 1.2 };
        let dash = match (&r.side, r.status) {
            (_, Placement::Suggested) | (_, Placement::Unplaced) => r#" stroke-dasharray="2.4 1.8""#,
            (RunSide::Underside, _) => r#" stroke-dasharray="4 2""#,
            _ => "",
        };
        let (mx, my) = ((ax + bx) / 2.0, (ay + by) / 2.0);
        let len = (bx - ax).hypot(by - ay).max(0.001);
        let bow = (len * 0.1).min(10.0);
        let (nx, ny) = (-(by - ay) / len, (bx - ax) / len);
        let (cx, cy) = (mx + nx * bow, my + ny * bow);
        let outline = if r.color.eq_ignore_ascii_case("white") { "#777" } else { "#00000055" };
        let _ = writeln!(
            s,
            r##"<path d="M {ax} {ay} Q {cx} {cy} {bx} {by}" fill="none" stroke="{outline}" stroke-width="{}" stroke-linecap="round"{dash}/><path d="M {ax} {ay} Q {cx} {cy} {bx} {by}" fill="none" stroke="{col}" stroke-width="{width}" stroke-linecap="round"{dash}/>"##,
            width + 0.8
        );
        for [x, y] in [[ax, ay], [bx, by]] {
            let _ = writeln!(s, r##"<circle cx="{x}" cy="{y}" r="1.0" fill="{col}" stroke="#222" stroke-width="0.3"/>"##);
        }
        let _ = writeln!(
            s,
            r##"<text x="{}" y="{}" font-size="2.8" font-weight="bold" text-anchor="middle" fill="#111" stroke="#fff" stroke-width="0.9" paint-order="stroke">{}</text>"##,
            (mx + cx) / 2.0,
            (my + cy) / 2.0 - 0.8,
            xml_escape(&r.id)
        );
    }

    // Legend and notes column.
    let mut y = 34.0;
    let line = |s: &mut String, y: &mut f32, text: &str, size: f32, weight: &str, color: &str| {
        for chunk in wrap(text, (220.0 / (size * 0.53)) as usize) {
            let _ = writeln!(
                s,
                r##"<text x="{notes_x}" y="{}" font-size="{size}" font-weight="{weight}" fill="{color}">{}</text>"##,
                *y,
                xml_escape(&chunk)
            );
            *y += size * 1.32;
        }
    };
    line(&mut s, &mut y, "Legend", 4.4, "bold", "#111");
    let halo = format!("{:.0} mm clearance zone around {}", perf.hv_clearance_mm, perf.hv_nets.join(", "));
    let legend: [(&str, &str); 7] = [
        ("solid wire", "top-side jumper or flying lead (thick = 12 AWG weld lead)"),
        ("long dashes", "underside jumper"),
        ("short dashes", "suggested or implied, not from the build notes"),
        ("hatched halo", &halo),
        ("orange copper", "strip or rail on a high-voltage net"),
        ("tan copper", "strip or rail in use (net name printed above the strip)"),
        ("dashed outline", "approximate position: region known, holes not"),
    ];
    for (k, v) in legend {
        line(&mut s, &mut y, &format!("{k}: {v}"), 3.2, "normal", "#333");
    }
    y += 3.0;
    let section = |s: &mut String, y: &mut f32, title: &str, items: &[String], color: &str| {
        if items.is_empty() {
            return;
        }
        line(s, y, title, 4.2, "bold", color);
        for c in items {
            line(s, y, &format!("• {c}"), 3.1, "normal", "#222");
        }
        *y += 3.0;
    };
    section(&mut s, &mut y, "Clearance findings", &rep.clearance, "#b0281a");
    section(&mut s, &mut y, "Shorts", &rep.shorts, "#b0281a");
    let missing: Vec<String> = rep.missing.clone();
    section(&mut s, &mut y, "Unbuilt connections", &missing, "#b0281a");
    section(&mut s, &mut y, "Build notes", &perf.notes, "#111");
    section(&mut s, &mut y, "Placement status", &rep.placement, "#111");

    let height = (y + 8.0).max(bottom + 12.0).max(oy + bh * k + 20.0);
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {width} {height}\" width=\"{}\" height=\"{}\" font-family=\"DejaVu Sans, Arial, sans-serif\">\n<rect x=\"0\" y=\"0\" width=\"{width}\" height=\"{height}\" fill=\"#ffffff\"/>\n{s}</svg>\n",
        width * 4.0,
        height * 4.0
    )
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n - 1).collect::<String>() + "…" }
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}
