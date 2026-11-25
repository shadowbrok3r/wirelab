//! Documentation exports: KiCad schematic, perfboard drawing and net/wire tables.

pub mod kicad;
pub mod nets;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::board::{BoardProfile, PinKind};
use crate::circuit::{Circuit, PlacedComponent};
use crate::component::{ComponentDef, Shape, SimModel};
use crate::library::Library;
use crate::netlist::Netlist;

/// Sheet placement and annotations for the schematic export.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SchematicLayout {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub date: String,
    /// KiCad paper size, e.g. "A3" (default) or "A2".
    #[serde(default)]
    pub paper: String,
    /// Reference designator for the board, e.g. "U2".
    #[serde(default)]
    pub board_ref: String,
    /// Board symbol centre on the sheet, mm.
    #[serde(default)]
    pub board_at: Option<[f32; 2]>,
    /// Part placement by label: `[x_mm, y_mm, rotation_deg]`.
    #[serde(default)]
    pub parts: BTreeMap<String, [f32; 3]>,
    /// Free text blocks on the sheet.
    #[serde(default)]
    pub notes: Vec<SheetNote>,
    /// Board pin key -> note printed beside that pin's label.
    #[serde(default)]
    pub pin_notes: BTreeMap<String, String>,
    /// Safety findings carried into every export.
    #[serde(default)]
    pub flags: Vec<String>,
    /// Unresolved decisions carried into every export.
    #[serde(default)]
    pub open_items: Vec<String>,
    /// Component labels left out of the documents (script controllers, junctions).
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SheetNote {
    pub title: String,
    #[serde(default)]
    pub lines: Vec<String>,
    /// Top-left corner on the sheet, mm.
    pub at: [f32; 2],
    /// Wrap width in characters (0 = 100).
    #[serde(default)]
    pub wrap: usize,
    /// Font size in mm (0 = 1.27).
    #[serde(default)]
    pub font_mm: f32,
}

impl SheetNote {
    pub fn size(&self) -> f64 {
        if self.font_mm > 0.0 { f64::from(self.font_mm) } else { 1.27 }
    }
}

/// Shared view of a circuit for the document writers.
pub struct DocCtx<'a> {
    pub name: &'a str,
    pub circuit: &'a Circuit,
    pub board: &'a BoardProfile,
    pub lib: &'a Library,
    pub nl: Netlist,
    /// Display name per net id.
    pub net_names: Vec<String>,
    pub layout: SchematicLayout,
}

impl<'a> DocCtx<'a> {
    pub fn new(
        name: &'a str,
        circuit: &'a Circuit,
        board: &'a BoardProfile,
        lib: &'a Library,
    ) -> Self {
        let nl = Netlist::build(circuit, board, lib);
        let net_names = name_nets(circuit, &nl, board, lib);
        let layout = circuit.schematic.clone().unwrap_or_default();
        DocCtx { name, circuit, board, lib, nl, net_names, layout }
    }

    pub fn board_ref(&self) -> String {
        if self.layout.board_ref.is_empty() { "U1".into() } else { self.layout.board_ref.clone() }
    }

    /// Placed parts that appear in the documents, in label order.
    pub fn parts(&self) -> Vec<(&'a PlacedComponent, &'a ComponentDef)> {
        let mut v: Vec<_> = self
            .circuit
            .components
            .values()
            .filter_map(|c| Some((c, self.lib.component(&c.def_id)?)))
            .filter(|(c, d)| d.visual.shape != Shape::Dot && !self.layout.exclude.contains(&c.label))
            .collect();
        v.sort_by_key(|a| natural_key(&part_ref(a.0)));
        v
    }

    pub fn net_name(&self, net: usize) -> &str {
        &self.net_names[net]
    }
}

/// Reference designator of a placed part (its label, else its library id).
pub fn part_ref(c: &PlacedComponent) -> String {
    if c.label.is_empty() { c.def_id.clone() } else { c.label.clone() }
}

/// Sort key that orders "R2" before "R10".
pub fn natural_key(s: &str) -> (String, u64, String) {
    let prefix: String = s.chars().take_while(|c| !c.is_ascii_digit()).collect();
    let rest = &s[prefix.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let tail = rest[digits.len()..].to_string();
    (prefix, digits.parse().unwrap_or(0), tail)
}

/// Engineering-notation value, e.g. 100000 -> "100k".
pub fn eng(v: f64, unit: &str) -> String {
    let (scaled, prefix) = if v == 0.0 {
        (0.0, "")
    } else {
        let a = v.abs();
        if a >= 1e6 {
            (v / 1e6, "M")
        } else if a >= 1e3 {
            (v / 1e3, "k")
        } else if a >= 1.0 {
            (v, "")
        } else if a >= 1e-3 {
            (v * 1e3, "m")
        } else if a >= 1e-6 {
            (v * 1e6, "u")
        } else if a >= 1e-9 {
            (v * 1e9, "n")
        } else {
            (v * 1e12, "p")
        }
    };
    let mut s = format!("{scaled:.2}");
    while s.contains('.') && (s.ends_with('0') || s.ends_with('.')) {
        s.pop();
    }
    format!("{s}{prefix}{unit}")
}

/// Value text for schematics and BOMs.
pub fn part_value(c: &PlacedComponent, d: &ComponentDef) -> String {
    match &d.sim {
        SimModel::Resistor { ohms } => {
            let ohms = c.props.get("ohms").copied().unwrap_or(f64::from(*ohms));
            eng(ohms, "")
        }
        _ if !d.value.is_empty() => d.value.clone(),
        SimModel::Capacitor { farads, .. } => eng(f64::from(*farads), "F"),
        _ => d.name.clone(),
    }
}

/// Rating text for BOMs.
pub fn part_rating(d: &ComponentDef) -> String {
    if !d.rating.is_empty() {
        return d.rating.clone();
    }
    match d.sim {
        SimModel::Resistor { .. } => "1/4 W metal film".into(),
        _ => String::new(),
    }
}

/// Names every net: explicit names first, then rails, GPIOs and part pins.
pub fn name_nets(circuit: &Circuit, nl: &Netlist, board: &BoardProfile, lib: &Library) -> Vec<String> {
    let mut names: Vec<Option<String>> = vec![None; nl.nets.len()];
    for (name, ep) in &circuit.net_names {
        if let Some(n) = nl.net_of(ep) {
            names[n] = Some(name.clone());
        }
    }
    for net in &nl.nets {
        if names[net.id].is_some() {
            continue;
        }
        names[net.id] = if net.rails.gnd {
            Some("GND".into())
        } else if net.rails.v33 {
            Some("+3V3".into())
        } else if net.rails.v5 {
            Some("+5V".into())
        } else if let Some(g) = net.gpios.first() {
            Some(format!("GPIO{g}"))
        } else {
            net.endpoints.iter().find_map(|ep| match ep {
                crate::circuit::Endpoint::Terminal { comp, terminal } => {
                    let c = circuit.components.get(comp)?;
                    let d = lib.component(&c.def_id)?;
                    let t = d.terminal(terminal)?;
                    Some(format!("Net-({}-{})", part_ref(c), t.name))
                }
                crate::circuit::Endpoint::BoardPin { key } => {
                    board.pin(key).map(|p| format!("Net-({key}-{})", p.label))
                }
                crate::circuit::Endpoint::Hole { .. } => None,
            })
        };
    }
    names.into_iter().enumerate().map(|(i, n)| n.unwrap_or_else(|| format!("Net-{i}"))).collect()
}

/// True for board pins that are supply rails.
pub fn is_rail(kind: PinKind) -> bool {
    matches!(kind, PinKind::Gnd | PinKind::V3_3 | PinKind::V5)
}

/// Writes every document for one circuit into `dir`; returns the files written.
pub fn export_all(
    name: &str,
    circuit: &Circuit,
    board: &BoardProfile,
    lib: &Library,
    dir: &Path,
) -> std::io::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(dir)?;
    let ctx = DocCtx::new(name, circuit, board, lib);
    let stem = slug(name);
    let mut written = Vec::new();
    let mut put = |file: String, text: String| -> std::io::Result<()> {
        let path = dir.join(file);
        std::fs::write(&path, text)?;
        written.push(path);
        Ok(())
    };
    put(format!("{stem}.kicad_sch"), kicad::schematic(&ctx))?;
    put(format!("{stem}.kicad_pro"), kicad::project_file(&stem))?;
    put("wirelab.kicad_sym".into(), kicad::symbol_library())?;
    put("sym-lib-table".into(), kicad::sym_lib_table())?;
    let ops = nets::operating_points(&ctx);
    put(format!("{stem}-nets.md"), nets::markdown(&ctx, &ops))?;
    put(format!("{stem}-wires.csv"), nets::wires_csv(&ctx))?;
    put(format!("{stem}-bom.csv"), nets::bom_csv(&ctx))?;
    if let Some(perf) = &circuit.perf {
        let ready = ops.ready.as_ref().map(|(_, o)| o.net_avg_mv.as_slice());
        let report = crate::perf::check(perf, &ctx, ready);
        put(format!("{stem}-perfboard.svg"), crate::perf::svg(perf, &ctx, &report))?;
        put(format!("{stem}-perfboard-check.md"), crate::perf::report_markdown(&report))?;
    }
    Ok(written)
}

/// File-name stem: lowercase ASCII words joined by dashes.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// Escapes text for XML/SVG.
pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
