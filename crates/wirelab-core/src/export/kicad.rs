//! KiCad schematic writer (file format 20231120, readable by KiCad 8 and newer).
//!
//! Aligned pins of a net that face each other are joined by one wire; every other pin gets a
//! short stub ending in a net label or power symbol. Symbols are embedded in the file.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::board::{PinKind, Side};
use crate::circuit::Endpoint;
use crate::component::{ComponentDef, SimModel, TerminalRole};

use super::{DocCtx, part_rating, part_ref, part_value};

const GRID: f64 = 1.27;
const STUB: f64 = 2.54;
const FONT: &str = "(effects (font (size 1.27 1.27)))";
const FONT_HIDE: &str = "(effects (font (size 1.27 1.27)) hide)";

struct Ids(u64);

impl Ids {
    fn new(seed: &str) -> Self {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in seed.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        Ids(h)
    }

    fn next(&mut self) -> String {
        let z = |s: &mut u64| {
            *s = s.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut x = *s;
            x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            x ^ (x >> 31)
        };
        let (a, b) = (z(&mut self.0), z(&mut self.0));
        format!(
            "{:08x}-{:04x}-4{:03x}-a{:03x}-{:012x}",
            a >> 32,
            (a >> 16) & 0xffff,
            a & 0xfff,
            (b >> 48) & 0xfff,
            b & 0xffff_ffff_ffff
        )
    }
}

fn num(v: f64) -> String {
    let r = (v * 10_000.0).round() / 10_000.0;
    let mut s = format!("{r:.4}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    if s == "-0" { "0".into() } else { s }
}

fn snap(v: f64) -> f64 {
    (v / GRID).round() * GRID
}

fn q(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One pin in library coordinates (y up); `angle` points from the tip into the body.
#[derive(Clone)]
struct PinGeo {
    number: String,
    name: String,
    x: f64,
    y: f64,
    angle: i32,
    etype: &'static str,
}

struct SymDef {
    lib_id: String,
    text: String,
    /// Terminal id (or board pin key) -> pin.
    pins: BTreeMap<String, PinGeo>,
    /// Body half-extent in library units, for field placement.
    half: (f64, f64),
    /// Box outline `[left, top, right, bottom]` in library units, for module symbols.
    frame: Option<[f64; 4]>,
}

fn pin_sexpr(p: &PinGeo, length: f64, hide: bool) -> String {
    format!(
        "(pin {} line (at {} {} {}) (length {}){} (name {} {FONT}) (number {} {FONT}))",
        p.etype,
        num(p.x),
        num(p.y),
        p.angle,
        num(length),
        if hide { " hide" } else { "" },
        q(&p.name),
        q(&p.number)
    )
}

fn header(lib_id: &str, prefix: &str, value: &str, names: bool, numbers: bool, power: bool) -> String {
    let mut s = format!("(symbol {}", q(lib_id));
    if power {
        s.push_str(" (power)");
    }
    if !numbers {
        s.push_str(" (pin_numbers hide)");
    }
    if names {
        s.push_str(" (pin_names (offset 1.016))");
    } else {
        s.push_str(" (pin_names (offset 0) hide)");
    }
    let _ = write!(
        s,
        " (exclude_from_sim no) (in_bom {}) (on_board yes)\n  (property \"Reference\" {} (at 0 0 0) {})\n  (property \"Value\" {} (at 0 0 0) {FONT})\n  (property \"Footprint\" \"\" (at 0 0 0) {FONT_HIDE})\n  (property \"Datasheet\" \"~\" (at 0 0 0) {FONT_HIDE})\n",
        if power { "no" } else { "yes" },
        q(prefix),
        if power { FONT_HIDE } else { FONT },
        q(value)
    );
    s
}

fn unit_name(lib_id: &str) -> &str {
    lib_id.split(':').nth(1).unwrap_or(lib_id)
}

/// Rotates a library point `rot` degrees counter-clockwise (y up).
fn rot_pt(rot: i32, x: f64, y: f64) -> (f64, f64) {
    match rot.rem_euclid(360) {
        90 => (-y, x),
        180 => (-x, -y),
        270 => (y, -x),
        _ => (x, y),
    }
}

fn pts(rot: i32, list: &[(f64, f64)]) -> String {
    list.iter()
        .map(|&(x, y)| {
            let (a, b) = rot_pt(rot, x, y);
            format!("(xy {} {})", num(a), num(b))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn rect(rot: i32, x0: f64, y0: f64, x1: f64, y1: f64, width: &str, fill: &str) -> String {
    let (a0, b0) = rot_pt(rot, x0, y0);
    let (a1, b1) = rot_pt(rot, x1, y1);
    format!(
        "(rectangle (start {} {}) (end {} {}) (stroke (width {width}) (type default)) (fill (type {fill})))",
        num(a0),
        num(b0),
        num(a1),
        num(b1)
    )
}

fn poly(rot: i32, list: &[(f64, f64)], width: &str) -> String {
    format!("(polyline (pts {}) (stroke (width {width}) (type default)) (fill (type none)))", pts(rot, list))
}

fn rpin(rot: i32, number: &str, name: &str, x: f64, y: f64, angle: i32) -> PinGeo {
    let (a, b) = rot_pt(rot, x, y);
    PinGeo {
        number: number.into(),
        name: name.into(),
        x: a,
        y: b,
        angle: (angle + rot).rem_euclid(360),
        etype: "passive",
    }
}

/// Two-pin symbol from rotated graphics; `half` is the unrotated body half-extent.
fn two_pin(
    lib_id: &str,
    prefix: &str,
    body: &str,
    pins: [(String, PinGeo, f64); 2],
    half: (f64, f64),
    rot: i32,
) -> SymDef {
    let base = unit_name(lib_id);
    let mut text = header(lib_id, prefix, prefix, false, false, false);
    let _ = write!(
        text,
        "  (symbol \"{base}_0_1\" {body})\n  (symbol \"{base}_1_1\" {} {})\n)",
        pin_sexpr(&pins[0].1, pins[0].2, false),
        pin_sexpr(&pins[1].1, pins[1].2, false)
    );
    let half = if rot.rem_euclid(180) == 90 { (half.1, half.0) } else { half };
    let [(k0, p0, _), (k1, p1, _)] = pins;
    SymDef { lib_id: lib_id.into(), text, pins: BTreeMap::from([(k0, p0), (k1, p1)]), half, frame: None }
}

fn sym_resistor(lib_id: &str, d: &ComponentDef, rot: i32) -> SymDef {
    let t0 = d.terminals.first().map(|t| t.id.clone()).unwrap_or_default();
    let t1 = d.terminals.get(1).map(|t| t.id.clone()).unwrap_or_default();
    two_pin(
        lib_id,
        "R",
        &rect(rot, -1.016, -2.54, 1.016, 2.54, "0.254", "none"),
        [(t0, rpin(rot, "1", "~", 0.0, 3.81, 270), 1.27), (t1, rpin(rot, "2", "~", 0.0, -3.81, 90), 1.27)],
        (1.016, 2.54),
        rot,
    )
}

fn sym_cap(lib_id: &str, d: &ComponentDef, rot: i32) -> SymDef {
    let pos = d.terminal_by_role(TerminalRole::Positive).map(|t| t.id.clone()).unwrap_or_default();
    let neg = d.terminal_by_role(TerminalRole::Negative).map(|t| t.id.clone()).unwrap_or_default();
    let body = format!(
        "\n    {}\n    {}\n    {}\n    {}",
        rect(rot, -2.286, 0.508, 2.286, 1.016, "0", "none"),
        rect(rot, 2.286, -1.016, -2.286, -0.508, "0", "outline"),
        poly(rot, &[(-1.778, 2.286), (-0.762, 2.286)], "0"),
        poly(rot, &[(-1.27, 2.794), (-1.27, 1.778)], "0")
    );
    two_pin(
        lib_id,
        "C",
        &body,
        [(pos, rpin(rot, "1", "+", 0.0, 3.81, 270), 2.794), (neg, rpin(rot, "2", "-", 0.0, -3.81, 90), 2.794)],
        (2.286, 1.016),
        rot,
    )
}

fn sym_diode(lib_id: &str, d: &ComponentDef, rot: i32) -> SymDef {
    let a = d.terminal_by_role(TerminalRole::Anode).map(|t| t.id.clone()).unwrap_or_default();
    let k = d.terminal_by_role(TerminalRole::Cathode).map(|t| t.id.clone()).unwrap_or_default();
    let body = format!(
        "\n    {}\n    {}",
        poly(rot, &[(-1.27, 1.27), (-1.27, -1.27), (1.27, 0.0), (-1.27, 1.27)], "0.254"),
        poly(rot, &[(1.27, 1.27), (1.27, -1.27)], "0.254")
    );
    two_pin(
        lib_id,
        "D",
        &body,
        [(k, rpin(rot, "1", "K", 3.81, 0.0, 180), 2.54), (a, rpin(rot, "2", "A", -3.81, 0.0, 0), 2.54)],
        (1.27, 1.27),
        rot,
    )
}

/// A visible field: position, justification, font size; `None` hides it.
type FieldPos<'a> = Option<((f64, f64), &'a str, f64)>;

/// One pin request for a box symbol: key, name, number, electrical type.
type PinSpec = (String, String, String, &'static str);

#[derive(Default)]
struct BoxPins {
    left: Vec<PinSpec>,
    right: Vec<PinSpec>,
    top: Vec<PinSpec>,
    bottom: Vec<PinSpec>,
}

/// A line of text inside a box symbol: text, font size, style.
type BoxLine = (String, f64, &'static str);

fn grid_up(v: f64) -> f64 {
    (v / GRID - 1e-9).ceil() * GRID
}

fn grid_down(v: f64) -> f64 {
    (v / GRID + 1e-9).floor() * GRID
}

/// Rectangular module symbol: title lines at the top, pins on any edge.
fn sym_box(
    lib_id: &str,
    prefix: &str,
    lines: &[BoxLine],
    pins: &BoxPins,
    pitch: f64,
    min_width: f64,
    numbers: bool,
) -> SymDef {
    let base = unit_name(lib_id);
    let text_w = lines
        .iter()
        .map(|(t, size, _)| t.chars().count() as f64 * size * 0.82)
        .fold(0.0, f64::max)
        + 4.0;
    let tb = pins.top.len().max(pins.bottom.len()) as f64;
    let width = (min_width.max(text_w).max((tb + 1.0) * 5.08) / 5.08).ceil() * 5.08;
    let half_w = width / 2.0;
    let title_h: f64 = lines.iter().map(|(_, size, _)| size * 1.5).sum::<f64>() + 1.2;
    let top_zone = if pins.top.is_empty() { 0.0 } else { 5.08 };
    let bottom_zone = if pins.bottom.is_empty() { 0.0 } else { 5.08 };
    let n = pins.left.len().max(pins.right.len()) as f64;
    let shift = grid_up((title_h + top_zone) / 2.0);
    let y_first = if n > 0.0 { (n - 1.0) / 2.0 * pitch - shift } else { -shift };
    let y_last = y_first - (n - 1.0).max(0.0) * pitch;
    let top = grid_up(y_first + pitch / 2.0 + title_h + top_zone);
    let bottom = grid_down(y_last - pitch / 2.0 - bottom_zone);

    let mut out = BTreeMap::new();
    let mut pin_text = String::new();
    let mut add = |p: PinGeo, key: &str| {
        let _ = write!(pin_text, "\n    {}", pin_sexpr(&p, 2.54, false));
        out.insert(key.to_string(), p);
    };
    for (side, list) in [(-1.0f64, &pins.left), (1.0, &pins.right)] {
        for (i, (key, name, number, etype)) in list.iter().enumerate() {
            add(
                PinGeo {
                    number: number.clone(),
                    name: name.clone(),
                    x: side * (half_w + 2.54),
                    y: y_first - i as f64 * pitch,
                    angle: if side < 0.0 { 0 } else { 180 },
                    etype,
                },
                key,
            );
        }
    }
    for (y_edge, angle, list) in [(top + 2.54, 270, &pins.top), (bottom - 2.54, 90, &pins.bottom)] {
        let m = list.len() as f64;
        for (j, (key, name, number, etype)) in list.iter().enumerate() {
            add(
                PinGeo {
                    number: number.clone(),
                    name: name.clone(),
                    x: (j as f64 - (m - 1.0) / 2.0) * 5.08,
                    y: y_edge,
                    angle,
                    etype,
                },
                key,
            );
        }
    }

    let mut text = header(lib_id, prefix, base, true, numbers, false);
    let mut body = format!(
        "(rectangle (start {} {}) (end {} {}) (stroke (width 0.254) (type default)) (fill (type background)))",
        num(-half_w),
        num(top),
        num(half_w),
        num(bottom)
    );
    let mut y = top - top_zone - 0.9;
    for (line, size, style) in lines {
        let center = y - size * 0.75;
        let _ = write!(
            body,
            "\n    (text {} (at 0 {} 0) (effects (font (size {s} {s}){})))",
            q(line),
            num(center),
            if style.is_empty() { String::new() } else { format!(" {style}") },
            s = num(*size)
        );
        y -= size * 1.5;
    }
    let _ = write!(text, "  (symbol \"{base}_0_1\" {body})\n  (symbol \"{base}_1_1\"{pin_text})\n)");
    SymDef {
        lib_id: lib_id.into(),
        text,
        pins: out,
        half: (half_w, top.max(-bottom)),
        frame: Some([-half_w, top, half_w, bottom]),
    }
}

fn sym_power(name: &str, ground: bool) -> SymDef {
    let lib_id = format!("wirelab:{name}");
    let mut text = format!(
        "(symbol {} (power) (pin_names (offset 0)) (exclude_from_sim no) (in_bom yes) (on_board yes)\n  (property \"Reference\" \"#PWR\" (at 0 0 0) {FONT_HIDE})\n  (property \"Value\" {} (at 0 0 0) {FONT})\n  (property \"Footprint\" \"\" (at 0 0 0) {FONT_HIDE})\n  (property \"Datasheet\" \"\" (at 0 0 0) {FONT_HIDE})\n",
        q(&lib_id),
        q(name)
    );
    let pin = PinGeo {
        number: "1".into(),
        name: name.into(),
        x: 0.0,
        y: 0.0,
        angle: if ground { 270 } else { 90 },
        etype: "power_in",
    };
    let graphic = if ground {
        "(polyline (pts (xy 0 0) (xy 0 -1.27) (xy 1.27 -1.27) (xy 0 -2.54) (xy -1.27 -1.27) (xy 0 -1.27)) (stroke (width 0) (type default)) (fill (type none)))"
    } else {
        "(polyline (pts (xy -0.762 1.27) (xy 0 2.54) (xy 0.762 1.27)) (stroke (width 0) (type default)) (fill (type none))) (polyline (pts (xy 0 0) (xy 0 2.54)) (stroke (width 0) (type default)) (fill (type none)))"
    };
    let _ = write!(
        text,
        "  (symbol {} {graphic})\n  (symbol {} {})\n)",
        q(&format!("{name}_0_1")),
        q(&format!("{name}_1_1")),
        pin_sexpr(&pin, 0.0, true)
    );
    SymDef { lib_id, text, pins: BTreeMap::from([("1".into(), pin)]), half: (1.27, 2.54), frame: None }
}

fn lib_key(d: &ComponentDef) -> String {
    let mut s = String::new();
    for c in d.id.chars() {
        s.push(if c.is_ascii_alphanumeric() { c.to_ascii_uppercase() } else { '_' });
    }
    format!("wirelab:{s}")
}

fn box_title(d: &ComponentDef) -> (String, Option<String>) {
    match d.sim {
        SimModel::DcSupply { .. } => ("DC SUPPLY".into(), None),
        SimModel::HvBoost { .. } => ("HV CHARGER".into(), Some("ZVS, non-isolated".into())),
        SimModel::RelayModule => ("RELAY".into(), Some("opto-isolated module".into())),
        SimModel::ArcContact { .. } => ("WELD PEN".into(), Some("electrode".into())),
        SimModel::Conductor { .. } => ("WORK CLAMP".into(), None),
        _ => (d.name.chars().take(22).collect(), None),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

fn edge_of(d: &ComponentDef, index: usize, role: TerminalRole) -> Edge {
    match (&d.sim, role) {
        (_, TerminalRole::Vcc) => Edge::Top,
        (_, TerminalRole::Gnd | TerminalRole::InNeg | TerminalRole::OutNeg) => Edge::Bottom,
        (SimModel::DcSupply { .. }, TerminalRole::Negative) => Edge::Bottom,
        (SimModel::DcSupply { .. }, TerminalRole::Positive) => Edge::Right,
        (_, TerminalRole::InPos | TerminalRole::Signal) => Edge::Left,
        (
            _,
            TerminalRole::OutPos
            | TerminalRole::Common
            | TerminalRole::NormallyOpen
            | TerminalRole::NormallyClosed,
        ) => Edge::Right,
        _ => {
            if index < d.terminals.len().div_ceil(2) {
                Edge::Left
            } else {
                Edge::Right
            }
        }
    }
}

/// True for parts drawn as two-pin symbols whose orientation is baked into the library symbol.
fn is_small(d: &ComponentDef) -> bool {
    matches!(
        d.sim,
        SimModel::Resistor { .. } | SimModel::Capacitor { .. } | SimModel::Diode { .. } | SimModel::Led { .. }
    )
}

fn component_symbol(c: &crate::circuit::PlacedComponent, d: &ComponentDef, lib_id: &str, rot: i32) -> SymDef {
    let lib_id = lib_id.to_string();
    match d.sim {
        SimModel::Resistor { .. } => sym_resistor(&lib_id, d, rot),
        SimModel::Capacitor { .. } => sym_cap(&lib_id, d, rot),
        SimModel::Diode { .. } | SimModel::Led { .. } => sym_diode(&lib_id, d, rot),
        _ => {
            let etype: &'static str =
                if matches!(d.sim, SimModel::DcSupply { .. }) { "power_out" } else { "passive" };
            let mut pins = BoxPins::default();
            for (i, t) in d.terminals.iter().enumerate() {
                let spec = (t.id.clone(), t.name.clone(), (i + 1).to_string(), etype);
                // A half turn mirrors the pin sides instead, so the text stays upright.
                let edge = match (edge_of(d, i, t.role), rot) {
                    (Edge::Left, 180) => Edge::Right,
                    (Edge::Right, 180) => Edge::Left,
                    (e, _) => e,
                };
                match edge {
                    Edge::Left => pins.left.push(spec),
                    Edge::Right => pins.right.push(spec),
                    Edge::Top => pins.top.push(spec),
                    Edge::Bottom => pins.bottom.push(spec),
                }
            }
            let (title, sub) = box_title(d);
            let mut lines: Vec<BoxLine> = vec![(title, 1.524, "bold")];
            if let Some(sub) = sub {
                lines.push((sub, 1.0, "italic"));
            }
            let value = part_value(c, d);
            if !value.is_empty() && value != d.name {
                lines.push((value, 1.143, ""));
            }
            let rating = part_rating(c, d);
            if !rating.is_empty() {
                lines.push((rating, 1.0, "italic"));
            }
            sym_box(&lib_id, ref_prefix(d), &lines, &pins, 5.08, 15.24, true)
        }
    }
}

fn ref_prefix(d: &ComponentDef) -> &'static str {
    match d.sim {
        SimModel::Resistor { .. } => "R",
        SimModel::Capacitor { .. } => "C",
        SimModel::Diode { .. } | SimModel::Led { .. } => "D",
        SimModel::RelayModule => "K",
        SimModel::DcSupply { .. } => "PS",
        SimModel::PushButton | SimModel::ToggleSwitch | SimModel::SlideSwitchSpdt => "SW",
        _ => "U",
    }
}

fn board_symbol(ctx: &DocCtx) -> SymDef {
    let b = ctx.board;
    let spec = |p: &crate::board::BoardPin| -> PinSpec {
        let name = match p.kind {
            PinKind::Gpio(g) => format!("GPIO{g}"),
            PinKind::Gnd => "GND".into(),
            PinKind::V3_3 => "3V3".into(),
            PinKind::V5 => "5V".into(),
            _ => p.label.clone(),
        };
        let etype = match p.kind {
            PinKind::V3_3 | PinKind::V5 => "power_out",
            PinKind::Gnd => "passive",
            _ => "bidirectional",
        };
        (p.key.clone(), name, p.key.clone(), etype)
    };
    let mut pins = BoxPins::default();
    let mut ordered: Vec<_> = b.pins.iter().collect();
    ordered.sort_by_key(|p| (p.side != Side::Left, p.index));
    for p in ordered {
        match p.kind {
            PinKind::V5 | PinKind::V3_3 => pins.top.push(spec(p)),
            PinKind::Gnd => pins.bottom.push(spec(p)),
            _ if p.side == Side::Left => pins.left.push(spec(p)),
            _ => pins.right.push(spec(p)),
        }
    }
    let mut lines: Vec<BoxLine> = Vec::new();
    let mut line = String::new();
    for word in b.name.split_whitespace() {
        if line.len() + word.len() > 18 && !line.is_empty() {
            lines.push((std::mem::take(&mut line), 1.27, "bold"));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push((line, 1.27, "bold"));
    }
    sym_box("wirelab:BOARD", "U", &lines, &pins, 2.54, 30.48, false)
}

/// Sheet position of a library point for a symbol at `at` rotated `rot` degrees CCW.
fn place(at: (f64, f64), rot: i32, x: f64, y: f64) -> (f64, f64) {
    let (s, c) = match rot.rem_euclid(360) {
        90 => (1.0, 0.0),
        180 => (0.0, -1.0),
        270 => (-1.0, 0.0),
        _ => (0.0, 1.0),
    };
    let xr = x * c - y * s;
    let yr = x * s + y * c;
    (at.0 + xr, at.1 - yr)
}

/// Unit vector on the sheet pointing away from the body at a pin.
fn outward(pin: &PinGeo, rot: i32) -> (f64, f64) {
    let a = (pin.angle + 180 + rot).rem_euclid(360);
    match a {
        0 => (1.0, 0.0),
        90 => (0.0, -1.0),
        180 => (-1.0, 0.0),
        _ => (0.0, 1.0),
    }
}

struct PlacedPin {
    at: (f64, f64),
    out: (f64, f64),
    net: Option<usize>,
    board_key: Option<String>,
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    let indent = if text.starts_with("- ") { "  " } else { "" };
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
            line.push_str(indent);
        }
        if !line.is_empty() && !line.ends_with(' ') {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

struct Sheet {
    ids: Ids,
    body: String,
    project: String,
    root: String,
}

impl Sheet {
    fn wire(&mut self, a: (f64, f64), b: (f64, f64)) {
        let id = self.ids.next();
        let _ = writeln!(
            self.body,
            "(wire (pts (xy {} {}) (xy {} {})) (stroke (width 0) (type default)) (uuid {}))",
            num(a.0),
            num(a.1),
            num(b.0),
            num(b.1),
            q(&id)
        );
    }

    fn label(&mut self, name: &str, at: (f64, f64), angle: i32) {
        let just = match angle {
            180 | 270 => "right bottom",
            _ => "left bottom",
        };
        let id = self.ids.next();
        let _ = writeln!(
            self.body,
            "(label {} (at {} {} {angle}) (effects (font (size 1.27 1.27)) (justify {just})) (uuid {}))",
            q(name),
            num(at.0),
            num(at.1),
            q(&id)
        );
    }

    fn no_connect(&mut self, at: (f64, f64)) {
        let id = self.ids.next();
        let _ = writeln!(self.body, "(no_connect (at {} {}) (uuid {}))", num(at.0), num(at.1), q(&id));
    }

    fn text(&mut self, text: &str, at: (f64, f64), size: f64, justify: &str, style: &str) {
        let id = self.ids.next();
        let _ = writeln!(
            self.body,
            "(text {} (exclude_from_sim no) (at {} {} 0) (effects (font (size {s} {s}){style}) (justify {justify})) (uuid {}))",
            q(text),
            num(at.0),
            num(at.1),
            q(&id),
            s = num(size)
        );
    }

    /// A symbol instance with explicit field positions.
    #[allow(clippy::too_many_arguments)]
    fn instance(
        &mut self,
        sym: &SymDef,
        at: (f64, f64),
        rot: i32,
        reference: &str,
        fields: &[(&str, String, FieldPos)],
    ) {
        let uuid = self.ids.next();
        let _ = write!(
            self.body,
            "(symbol (lib_id {}) (at {} {} {rot}) (unit 1)\n  (exclude_from_sim no) (in_bom {}) (on_board yes) (dnp no)\n  (uuid {})\n",
            q(&sym.lib_id),
            num(at.0),
            num(at.1),
            if reference.starts_with('#') { "no" } else { "yes" },
            q(&uuid)
        );
        for (name, value, pos) in fields {
            match pos {
                Some(((x, y), just, size)) => {
                    let j = if just.is_empty() { String::new() } else { format!(" (justify {just})") };
                    // KiCad turns field text with the symbol; counter-rotate so it reads level.
                    let angle = if rot.rem_euclid(180) == 90 { rot.rem_euclid(360) } else { 0 };
                    let _ = writeln!(
                        self.body,
                        "  (property {} {} (at {} {} {angle}) (effects (font (size {s} {s})){j}))",
                        q(name),
                        q(value),
                        num(*x),
                        num(*y),
                        s = num(*size)
                    );
                }
                None => {
                    let _ = writeln!(
                        self.body,
                        "  (property {} {} (at {} {} 0) {FONT_HIDE})",
                        q(name),
                        q(value),
                        num(at.0),
                        num(at.1)
                    );
                }
            }
        }
        for p in sym.pins.values() {
            let id = self.ids.next();
            let _ = writeln!(self.body, "  (pin {} (uuid {}))", q(&p.number), q(&id));
        }
        let _ = writeln!(
            self.body,
            "  (instances (project {} (path {} (reference {}) (unit 1))))\n)",
            q(&self.project),
            q(&format!("/{}", self.root)),
            q(reference)
        );
    }
}

/// Sheet-space bounding box `[x0, y0, x1, y1]` of a symbol's frame.
fn frame_on_sheet(sym: &SymDef, at: (f64, f64), rot: i32) -> [f64; 4] {
    let [l, t, r, b] = sym.frame.unwrap_or([-sym.half.0, sym.half.1, sym.half.0, -sym.half.1]);
    let pts = [place(at, rot, l, t), place(at, rot, r, b)];
    [
        pts[0].0.min(pts[1].0),
        pts[0].1.min(pts[1].1),
        pts[0].0.max(pts[1].0),
        pts[0].1.max(pts[1].1),
    ]
}

/// The schematic as KiCad S-expression text.
pub fn schematic(ctx: &DocCtx) -> String {
    let mut ids = Ids::new(ctx.name);
    let root = ids.next();
    let layout = &ctx.layout;
    let mut sh = Sheet { ids, body: String::new(), project: super::slug(ctx.name), root: root.clone() };

    let mut defs: BTreeMap<String, SymDef> = BTreeMap::new();
    let mut pins: Vec<PlacedPin> = Vec::new();

    // Parts.
    let mut auto_x = 20.0;
    for (c, d) in ctx.parts() {
        let reference = part_ref(c);
        let [x, y, r] = layout.parts.get(&c.label).copied().unwrap_or_else(|| {
            auto_x += 25.4;
            [auto_x as f32, 230.0, 0.0]
        });
        let at = (snap(f64::from(x)), snap(f64::from(y)));
        let mut rot = (r as i32).rem_euclid(360);
        // Two-pin parts carry their orientation in the symbol so their fields stay level.
        let baked = if is_small(d) || rot == 180 { std::mem::take(&mut rot) } else { 0 };
        let key = if baked == 0 { lib_key(d) } else { format!("{}_{baked}", lib_key(d)) };
        let sym = defs.entry(key.clone()).or_insert_with(|| component_symbol(c, d, &key, baked));
        let value = part_value(c, d);
        let rating = part_rating(c, d);
        let fields: Vec<(&str, String, FieldPos)> = if sym.frame.is_some() {
            let [x0, y0, _, _] = frame_on_sheet(sym, at, rot);
            vec![
                ("Reference", reference.clone(), Some(((x0, y0 - 1.0), "left bottom", 1.27))),
                ("Value", value, None),
                ("Footprint", String::new(), None),
                ("Datasheet", "~".into(), None),
                ("Rating", rating, None),
            ]
        } else {
            let p: Vec<(f64, f64)> = sym.pins.values().map(|p| place(at, rot, p.x, p.y)).collect();
            let vertical = p.len() == 2 && (p[0].0 - p[1].0).abs() < 1e-6;
            if vertical {
                let x = at.0 + sym.half.0.max(1.0) + 1.5;
                vec![
                    ("Reference", reference.clone(), Some(((x, at.1 - 1.6), "left", 1.27))),
                    ("Value", value, Some(((x, at.1 + 0.6), "left", 1.27))),
                    ("Footprint", String::new(), None),
                    ("Datasheet", "~".into(), None),
                    ("Rating", rating, Some(((x, at.1 + 2.6), "left", 1.0))),
                ]
            } else {
                vec![
                    ("Reference", reference.clone(), Some(((at.0, at.1 - 3.2), "", 1.27))),
                    ("Value", value, Some(((at.0, at.1 + 3.0), "", 1.27))),
                    ("Footprint", String::new(), None),
                    ("Datasheet", "~".into(), None),
                    ("Rating", rating, Some(((at.0, at.1 + 4.9), "", 1.0))),
                ]
            }
        };
        sh.instance(sym, at, rot, &reference, &fields);
        for t in &d.terminals {
            let Some(p) = sym.pins.get(&t.id) else { continue };
            let net = ctx.nl.net_of(&Endpoint::Terminal { comp: c.id, terminal: t.id.clone() });
            pins.push(PlacedPin { at: place(at, rot, p.x, p.y), out: outward(p, rot), net, board_key: None });
        }
    }

    // Board.
    let board_sym = board_symbol(ctx);
    let bat = layout.board_at.map(|[x, y]| (snap(f64::from(x)), snap(f64::from(y)))).unwrap_or((300.0, 120.0));
    let [bx0, by0, _, _] = frame_on_sheet(&board_sym, bat, 0);
    sh.instance(
        &board_sym,
        bat,
        0,
        &ctx.board_ref(),
        &[
            ("Reference", ctx.board_ref(), Some(((bx0, by0 - 1.0), "left bottom", 1.27))),
            ("Value", ctx.board.name.clone(), None),
            ("Footprint", String::new(), None),
            ("Datasheet", "~".into(), None),
        ],
    );
    for pin in &ctx.board.pins {
        let Some(p) = board_sym.pins.get(&pin.key) else { continue };
        let net = ctx.nl.net_of(&Endpoint::BoardPin { key: pin.key.clone() });
        pins.push(PlacedPin {
            at: place(bat, 0, p.x, p.y),
            out: outward(p, 0),
            net,
            board_key: Some(pin.key.clone()),
        });
    }
    defs.insert(board_sym.lib_id.clone(), board_sym);

    let mut net_pins: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, p) in pins.iter().enumerate() {
        if let Some(n) = p.net {
            net_pins.entry(n).or_default().push(i);
        }
    }
    let net_has_wire = |net: usize| -> bool {
        ctx.nl.nets[net].endpoints.iter().any(|ep| ctx.circuit.wires_at(ep).next().is_some())
    };
    let named = |net: usize| ctx.circuit.net_names.contains_key(ctx.net_name(net));

    // Aligned pins facing each other within a net become one straight wire.
    let mut done: Vec<bool> = vec![false; pins.len()];
    for (&net, idxs) in &net_pins {
        if ctx.nl.nets[net].rails.any() {
            continue;
        }
        let mut pairs = Vec::new();
        for (k, &i) in idxs.iter().enumerate() {
            if done[i] || pins[i].board_key.is_some() {
                continue;
            }
            let mut best: Option<(f64, usize)> = None;
            for &j in &idxs[k + 1..] {
                if done[j] || pins[j].board_key.is_some() {
                    continue;
                }
                let (a, b) = (&pins[i], &pins[j]);
                let d = (b.at.0 - a.at.0, b.at.1 - a.at.1);
                let aligned = d.0.abs() < 1e-6 || d.1.abs() < 1e-6;
                let len = d.0.hypot(d.1);
                let facing = d.0 * a.out.0 + d.1 * a.out.1 > 0.0 && d.0 * b.out.0 + d.1 * b.out.1 < 0.0;
                if aligned && facing && len <= 25.41 && best.is_none_or(|(l, _)| len < l) {
                    best = Some((len, j));
                }
            }
            if let Some((_, j)) = best {
                done[i] = true;
                done[j] = true;
                pairs.push((i, j));
            }
        }
        let leftovers = idxs.iter().any(|i| !done[*i]);
        for (i, j) in pairs {
            let (a, b) = (pins[i].at, pins[j].at);
            sh.wire(a, b);
            if named(net) || leftovers {
                let mid = (snap((a.0 + b.0) / 2.0), snap((a.1 + b.1) / 2.0));
                let vertical = (a.0 - b.0).abs() < 1e-6;
                sh.label(ctx.net_name(net), mid, if vertical { 180 } else { 0 });
            }
        }
    }

    let mut pwr_n = 0;
    for (i, p) in pins.iter().enumerate() {
        if done[i] {
            continue;
        }
        let Some(net) = p.net else { continue };
        let rails = ctx.nl.nets[net].rails;
        let lone = net_pins.get(&net).is_none_or(|v| v.len() < 2) && !rails.any();
        if lone || !net_has_wire(net) {
            sh.no_connect(p.at);
            continue;
        }
        let end = (p.at.0 + p.out.0 * STUB, p.at.1 + p.out.1 * STUB);
        sh.wire(p.at, end);
        let name = ctx.net_name(net).to_string();
        if rails.any() {
            let ground = rails.gnd;
            let sym = defs
                .entry(format!("wirelab:{}", if ground { "GND" } else { name.as_str() }))
                .or_insert_with(|| sym_power(if ground { "GND" } else { &name }, ground));
            // Rotate so the symbol's graphic points along the stub.
            let rot = match (ground, p.out) {
                (true, (_, y)) if y > 0.5 => 0,
                (true, (_, y)) if y < -0.5 => 180,
                (true, (x, _)) if x > 0.5 => 90,
                (true, _) => 270,
                (false, (_, y)) if y < -0.5 => 0,
                (false, (_, y)) if y > 0.5 => 180,
                (false, (x, _)) if x > 0.5 => 270,
                (false, _) => 90,
            };
            pwr_n += 1;
            let reference = format!("#PWR{pwr_n:02}");
            let along = if rot % 180 == 90 { 2.6 } else { 0.0 };
            let vfield = place(end, rot, 0.0, if ground { -4.2 - along } else { 3.6 + along });
            let fields = [
                ("Reference", reference.clone(), None),
                ("Value", name.clone(), Some((vfield, "", 1.27))),
                ("Footprint", String::new(), None),
                ("Datasheet", String::new(), None),
            ];
            sh.instance(sym, end, rot, &reference, &fields);
        } else {
            let angle = match p.out {
                (x, _) if x > 0.5 => 0,
                (x, _) if x < -0.5 => 180,
                (_, y) if y < -0.5 => 90,
                _ => 270,
            };
            sh.label(&name, end, angle);
            if let Some(note) = p.board_key.as_ref().and_then(|k| layout.pin_notes.get(k)) {
                let tx = end.0 + p.out.0 * (name.chars().count() as f64 * 1.1 + 2.0);
                let justify = if p.out.0 < 0.0 { "right bottom" } else { "left bottom" };
                sh.text(note, (tx, end.1), 1.143, justify, " italic");
            }
        }
    }

    // Notes.
    for note in &layout.notes {
        let width = if note.wrap == 0 { 100 } else { note.wrap };
        let mut text = note.title.to_uppercase();
        for line in &note.lines {
            for chunk in wrap(line, width) {
                text.push('\n');
                text.push_str(&chunk);
            }
        }
        sh.text(&text, (f64::from(note.at[0]), f64::from(note.at[1])), note.size(), "left top", "");
    }

    let mut out = String::new();
    let paper = if layout.paper.is_empty() { "A3" } else { &layout.paper };
    let _ = writeln!(
        out,
        "(kicad_sch (version 20231120) (generator \"wirelab\") (generator_version \"0.1\")\n(uuid {})\n(paper {})",
        q(&root),
        q(paper)
    );
    LIB_CACHE.with(|c| {
        *c.borrow_mut() = defs.values().map(|d| d.text.clone()).collect();
    });
    let title = if layout.title.is_empty() { ctx.name.to_string() } else { layout.title.clone() };
    let _ = writeln!(
        out,
        "(title_block (title {}) (date {}) (rev {}) (comment 1 {}) (comment 2 {}))",
        q(&title),
        q(&layout.date),
        q(&layout.revision),
        q("Generated by WireLab from the simulated project"),
        q("Gauges and board placement: see the net/wire table and the perfboard drawing")
    );
    out.push_str("(lib_symbols\n");
    for d in defs.values() {
        out.push_str(&d.text);
        out.push('\n');
    }
    out.push_str(")\n");
    out.push_str(&sh.body);
    out.push_str("(sheet_instances (path \"/\" (page \"1\")))\n)\n");
    out
}

thread_local! {
    static LIB_CACHE: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// The symbols of the last [`schematic`] call as a `.kicad_sym` library named `wirelab`.
pub fn symbol_library() -> String {
    let mut out = String::from("(kicad_symbol_lib (version 20231120) (generator \"wirelab\") (generator_version \"0.1\")\n");
    LIB_CACHE.with(|c| {
        for text in c.borrow().iter() {
            out.push_str(&text.replacen("(symbol \"wirelab:", "(symbol \"", 1));
            out.push('\n');
        }
    });
    out.push_str(")\n");
    out
}

/// Project symbol table pointing `wirelab` at the library beside the schematic.
pub fn sym_lib_table() -> String {
    "(sym_lib_table\n  (version 7)\n  (lib (name \"wirelab\") (type \"KiCad\") (uri \"${KIPRJMOD}/wirelab.kicad_sym\") (options \"\") (descr \"Symbols exported by WireLab\"))\n)\n".into()
}

/// Minimal KiCad project file so the schematic opens as a project.
pub fn project_file(stem: &str) -> String {
    format!(
        "{{\n  \"meta\": {{ \"filename\": \"{stem}.kicad_pro\", \"version\": 1 }},\n  \"sheets\": [],\n  \"text_variables\": {{}}\n}}\n"
    )
}
