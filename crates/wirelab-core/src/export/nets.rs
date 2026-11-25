//! Net table, wire table and BOM, annotated with simulated operating points.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use wirelab_proto::PinMode;

use crate::circuit::{Circuit, CompId, Endpoint};
use crate::component::{ArcPulse, BoostMode, CompState, SimModel, TerminalRole, VisualState};
use crate::sim::{PinBank, PinDrive, PowerState, SimOutput, step};

use super::{DocCtx, part_rating, part_ref, part_value};

/// Simulated states the documents quote.
pub struct OperatingPoints {
    /// Bank charging at mid voltage with every relay-driving GPIO high.
    pub charging: Option<(f64, SimOutput)>,
    /// Bank at the charger set point, relays released.
    pub ready: Option<(f64, SimOutput)>,
    /// First discharge pulse from the ready state, per electrode.
    pub pulses: Vec<(String, ArcPulse)>,
    /// Seconds from 0 V to 99 % of the charger set point.
    pub charge_time_s: Option<f64>,
    /// Bank capacitance (F) and bleed resistance (ohm) seen by it.
    pub bank_farads: f64,
    pub bleed_ohms: Option<f64>,
    /// Charger set point, volts.
    pub v_set: Option<f64>,
}

fn relay_gpios(ctx: &DocCtx) -> Vec<u8> {
    let mut out = Vec::new();
    for (c, d) in ctx.parts() {
        if !matches!(d.sim, SimModel::RelayModule) {
            continue;
        }
        let Some(t) = d.terminal_by_role(TerminalRole::Signal) else { continue };
        if let Some(n) = ctx.nl.net_of(&Endpoint::Terminal { comp: c.id, terminal: t.id.clone() }) {
            out.extend(ctx.nl.nets[n].gpios.iter().copied());
        }
    }
    out
}

fn bank_for(ctx: &DocCtx, relays_on: bool) -> PinBank {
    let mut bank = PinBank::default();
    let relays = relay_gpios(ctx);
    for net in &ctx.nl.nets {
        if net.endpoints.len() < 2 {
            continue;
        }
        for &g in &net.gpios {
            let drive = bank.get_mut(g).expect("gpio in range");
            *drive = if relays.contains(&g) {
                PinDrive { mode: PinMode::Output, out_high: relays_on, ..PinDrive::default() }
            } else {
                PinDrive { mode: PinMode::Analog, ..PinDrive::default() }
            };
        }
    }
    bank
}

fn with_touch(circuit: &Circuit, ctx: &DocCtx, pressed: bool) -> Circuit {
    let mut c = circuit.clone();
    for comp in c.components.values_mut() {
        if let Some(d) = ctx.lib.component(&comp.def_id)
            && matches!(d.sim, SimModel::ArcContact { .. })
        {
            comp.state = CompState::Button { pressed };
        }
    }
    c
}

fn caps_of(ctx: &DocCtx) -> Vec<(CompId, f64)> {
    ctx.parts()
        .into_iter()
        .filter_map(|(c, d)| match d.sim {
            SimModel::Capacitor { farads, .. } => Some((c.id, f64::from(farads))),
            _ => None,
        })
        .collect()
}

/// Runs the power-stage scenarios the tables quote.
pub fn operating_points(ctx: &DocCtx) -> OperatingPoints {
    let caps = caps_of(ctx);
    let bank_farads: f64 = caps.iter().map(|c| c.1).sum();
    let v_set = ctx.parts().into_iter().find_map(|(c, d)| match d.sim {
        SimModel::HvBoost { v_set, .. } => Some(c.props.get("v_set").copied().unwrap_or(f64::from(v_set))),
        _ => None,
    });
    let mut ops = OperatingPoints {
        charging: None,
        ready: None,
        pulses: Vec::new(),
        charge_time_s: None,
        bank_farads,
        bleed_ohms: None,
        v_set,
    };
    if caps.is_empty() {
        return ops;
    }
    let idle = with_touch(ctx.circuit, ctx, false);
    let run = |circuit: &Circuit, bank: &PinBank, volts: f64, dt: f64| {
        let mut st = PowerState::default();
        for (id, _) in &caps {
            st.set_cap_volts(*id, volts);
        }
        let mut left = dt;
        let mut out = step(circuit, ctx.board, ctx.lib, &ctx.nl, bank, &mut st, left.min(0.2));
        left -= left.min(0.2);
        while left > 1e-9 {
            out = step(circuit, ctx.board, ctx.lib, &ctx.nl, bank, &mut st, left.min(0.2));
            left -= left.min(0.2);
        }
        (st, out)
    };
    let top = v_set.unwrap_or(300.0);
    let on = bank_for(ctx, true);
    let off = bank_for(ctx, false);
    let (_, charging) = run(&idle, &on, top / 2.0, 0.002);
    ops.charging = Some((top / 2.0, charging));
    let (st_ready, ready) = run(&idle, &off, top, 0.002);
    let v_ready = st_ready.caps.values().copied().fold(0.0, f64::max);
    ops.ready = Some((v_ready, ready));

    // Bleed resistance from the ready-state droop over one second.
    let (st_bleed, _) = run(&idle, &off, top, 1.0);
    let v1 = st_bleed.caps.values().copied().fold(0.0, f64::max);
    if v1 > 0.0 && v1 < top {
        let tau = 1.0 / (top / v1).ln();
        ops.bleed_ohms = Some(tau / bank_farads);
    }

    // Weld pulse from the ready state.
    let touched = with_touch(ctx.circuit, ctx, true);
    let (st_weld, _) = run(&touched, &off, top, 0.004);
    for (id, (pulse, _)) in &st_weld.pulses {
        if let Some(c) = ctx.circuit.components.get(id) {
            ops.pulses.push((part_ref(c), *pulse));
        }
    }

    // Charge time from empty.
    let mut st = PowerState::default();
    for (id, _) in &caps {
        st.set_cap_volts(*id, 0.0);
    }
    let mut t = 0.0;
    while t < 30.0 {
        step(&idle, ctx.board, ctx.lib, &ctx.nl, &on, &mut st, 0.005);
        t += 0.005;
        let v = st.caps.values().copied().fold(0.0, f64::max);
        if v >= top * 0.99 {
            ops.charge_time_s = Some(t);
            break;
        }
    }
    ops
}

fn volts(out: &SimOutput, net: usize) -> String {
    match out.net_avg_mv.get(net).copied().flatten() {
        Some(mv) => {
            let v = f64::from(mv) / 1000.0;
            if v.abs() >= 10.0 { format!("{v:.1} V") } else { format!("{v:.3} V") }
        }
        None => "floating".into(),
    }
}

fn endpoint_name(ctx: &DocCtx, ep: &Endpoint) -> Option<String> {
    match ep {
        Endpoint::BoardPin { key } => {
            let pin = ctx.board.pin(key)?;
            Some(format!("{}.{}", ctx.board_ref(), pin.key))
        }
        Endpoint::Terminal { comp, terminal } => {
            let c = ctx.circuit.components.get(comp)?;
            let d = ctx.lib.component(&c.def_id)?;
            if d.visual.shape == crate::component::Shape::Dot || ctx.layout.exclude.contains(&c.label) {
                return None;
            }
            let t = d.terminal(terminal)?;
            Some(format!("{}.{}", part_ref(c), t.name))
        }
        Endpoint::Hole { .. } => None,
    }
}

/// Members of each net that carry a wire or a part, in display form.
fn net_members(ctx: &DocCtx) -> BTreeMap<String, (usize, Vec<String>)> {
    let mut out: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
    for net in &ctx.nl.nets {
        let has_wire = net.endpoints.iter().any(|ep| ctx.circuit.wires_at(ep).next().is_some());
        if !has_wire {
            continue;
        }
        let mut names: Vec<String> = net
            .endpoints
            .iter()
            .filter(|ep| {
                matches!(ep, Endpoint::Terminal { .. }) || ctx.circuit.wires_at(ep).next().is_some()
            })
            .filter_map(|ep| endpoint_name(ctx, ep))
            .collect();
        names.sort_by_key(|n| super::natural_key(n));
        names.dedup();
        if names.len() < 2 {
            continue;
        }
        out.insert(ctx.net_name(net.id).to_string(), (net.id, names));
    }
    out
}

fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', "<br>")
}

/// The full Markdown wiring document.
pub fn markdown(ctx: &DocCtx, ops: &OperatingPoints) -> String {
    let mut s = String::new();
    let title = if ctx.layout.title.is_empty() { ctx.name.to_string() } else { ctx.layout.title.clone() };
    let _ = writeln!(s, "# {title} — net and wire tables\n");
    let _ = writeln!(
        s,
        "Rev {} · {} · generated by WireLab from the simulated project. Board: {} ({}).\n",
        if ctx.layout.revision.is_empty() { "-" } else { &ctx.layout.revision },
        ctx.layout.date,
        ctx.board.name,
        ctx.board_ref()
    );

    if !ctx.layout.flags.is_empty() {
        s.push_str("## Safety flags\n\n");
        for f in &ctx.layout.flags {
            let _ = writeln!(s, "- {f}");
        }
        s.push('\n');
    }
    if !ctx.layout.open_items.is_empty() {
        s.push_str("## Open items (TBD — not guessed)\n\n");
        for f in &ctx.layout.open_items {
            let _ = writeln!(s, "- [ ] {f}");
        }
        s.push('\n');
    }

    s.push_str("## Simulated operating points\n\n");
    s.push_str("Values come from WireLab's transient solver with the part models below; model parameters marked *assumed* are not from datasheets.\n\n");
    let mut rows: Vec<(String, String)> = Vec::new();
    if ops.bank_farads > 0.0 {
        rows.push(("Bank capacitance".into(), super::eng(ops.bank_farads, "F")));
    }
    if let Some(v) = ops.v_set {
        rows.push(("Charger set point (U1 pot)".into(), format!("{v:.0} V")));
        rows.push((
            "Stored energy at set point".into(),
            format!("{:.2} J", 0.5 * ops.bank_farads * v * v),
        ));
    }
    if let Some(t) = ops.charge_time_s {
        rows.push(("Charge time 0 V -> 99 % (relay held closed)".into(), format!("{:.2} s", t)));
    }
    if let Some(r) = ops.bleed_ohms {
        let tau = r * ops.bank_farads;
        rows.push(("Bleed resistance seen by the bank".into(), super::eng(r, "ohm")));
        rows.push(("Bleed time constant".into(), format!("{tau:.1} s")));
        if let Some(v) = ops.v_set {
            rows.push((
                "Bleed time to 36 V / 5 V".into(),
                format!("{:.0} s / {:.0} s", tau * (v / 36.0).ln(), tau * (v / 5.0).ln()),
            ));
        }
    }
    for (comp, vis) in ops.charging.iter().flat_map(|(_, o)| o.visuals.iter()) {
        let name = ctx.circuit.components.get(comp).map(part_ref).unwrap_or_default();
        match vis {
            VisualState::Supply { volts, amps } => rows.push((
                format!("{name} while charging at {:.0} V", ops.charging.as_ref().map(|c| c.0).unwrap_or(0.0)),
                format!("{volts:.2} V, {amps:.2} A ({:.1} W)", volts * amps),
            )),
            VisualState::Boost { mode, volts, amps } => rows.push((
                format!("{name} output while charging"),
                format!(
                    "{volts:.0} V, {:.0} mA, {}",
                    amps * 1000.0,
                    match mode {
                        BoostMode::Off => "off",
                        BoostMode::ConstantCurrent => "current-limited",
                        BoostMode::ConstantPower => "power-limited",
                        BoostMode::Regulating => "regulating",
                    }
                ),
            )),
            _ => {}
        }
    }
    for (name, p) in &ops.pulses {
        rows.push((
            format!("{name} discharge from the ready state"),
            format!(
                "peak {:.0} A, {:.2} J into the contact, {:.0} us wide (>10 % of peak), {}",
                p.peak_a,
                p.joules,
                p.width_us,
                if p.dcen { "DCEN" } else { "DCEP (wrong polarity)" }
            ),
        ));
    }
    s.push_str("| Quantity | Simulated |\n|---|---|\n");
    for (k, v) in rows {
        let _ = writeln!(s, "| {} | {} |", md_cell(&k), md_cell(&v));
    }
    s.push('\n');

    // Model parameters.
    s.push_str("### Part models used\n\n| Ref | Model |\n|---|---|\n");
    for (c, d) in ctx.parts() {
        let p = |k: &str, dflt: f32| c.props.get(k).copied().unwrap_or(f64::from(dflt));
        let model = match &d.sim {
            SimModel::DcSupply { volts, r_int_ohms, i_max_a } => format!(
                "{:.1} V source, {r_int_ohms} ohm internal, {:.1} A rating",
                p("volts", *volts),
                p("i_max_a", *i_max_a)
            ),
            SimModel::HvBoost { v_set, i_max_a, p_max_w, vin_min, vin_max, efficiency } => format!(
                "regulates to {:.0} V; limited to {i_max_a} A and {p_max_w} W out; runs at {vin_min}-{vin_max} V in; efficiency {:.0} % (*assumed*)",
                p("v_set", *v_set),
                efficiency * 100.0
            ),
            SimModel::Capacitor { farads, v_rated, esr_ohms } => format!(
                "{} rated {v_rated:.0} V, ESR {:.2} ohm (*assumed*)",
                super::eng(f64::from(*farads), "F"),
                p("esr_ohms", *esr_ohms)
            ),
            SimModel::Diode { forward_mv, on_ohms } => {
                format!("{:.2} V drop + {on_ohms} ohm when forward biased", forward_mv / 1000.0)
            }
            SimModel::ArcContact { contact_ohms, v_arc } => format!(
                "touch = {:.3} ohm contact in series with a {:.0} V arc drop (*assumed*)",
                p("contact_ohms", *contact_ohms),
                p("v_arc", *v_arc)
            ),
            SimModel::Conductor { ohms } => format!("{:.4} ohm (*assumed*)", p("ohms", *ohms)),
            SimModel::Resistor { .. } => format!("{} ohm", part_value(c, d)),
            SimModel::RelayModule => "contact 0.1 ohm, closes when IN > 1.65 V (high-level trigger)".into(),
            _ => continue,
        };
        let _ = writeln!(s, "| {} | {} |", part_ref(c), md_cell(&model));
    }
    s.push('\n');

    // Net table.
    s.push_str("## Nets\n\n");
    let charge_v = ops.charging.as_ref().map(|c| c.0).unwrap_or(0.0);
    let ready_v = ops.ready.as_ref().map(|c| c.0).unwrap_or(0.0);
    let _ = writeln!(
        s,
        "| Net | Ready, bank {ready_v:.0} V, relay open | Charging, bank {charge_v:.0} V, relay closed | Members |\n|---|---|---|---|"
    );
    for (name, (net, members)) in net_members(ctx) {
        let r = ops.ready.as_ref().map(|(_, o)| volts(o, net)).unwrap_or_default();
        let c = ops.charging.as_ref().map(|(_, o)| volts(o, net)).unwrap_or_default();
        let _ = writeln!(s, "| **{}** | {r} | {c} | {} |", md_cell(&name), md_cell(&members.join(", ")));
    }
    s.push('\n');

    // Resistor stress in the ready state.
    if let Some((_, ready)) = &ops.ready {
        let mut lines = Vec::new();
        for (c, d) in ctx.parts() {
            let SimModel::Resistor { ohms } = d.sim else { continue };
            let ohms = c.props.get("ohms").copied().unwrap_or(f64::from(ohms));
            let net = |i: usize| {
                ctx.nl.net_of(&Endpoint::Terminal { comp: c.id, terminal: d.terminals[i].id.clone() })
            };
            let (Some(a), Some(b)) = (net(0), net(1)) else { continue };
            let (Some(va), Some(vb)) = (ready.net_avg_mv[a], ready.net_avg_mv[b]) else { continue };
            let v = f64::from(va - vb).abs() / 1000.0;
            lines.push(format!(
                "| {} | {} | {v:.1} V | {:.3} W | {:.2} mA |",
                part_ref(c),
                part_value(c, d),
                v * v / ohms,
                v / ohms * 1000.0
            ));
        }
        if !lines.is_empty() {
            let _ = writeln!(
                s,
                "### Resistor stress, ready state\n\n| Ref | Value | Across | Dissipation | Current |\n|---|---|---|---|---|"
            );
            for l in lines {
                let _ = writeln!(s, "{l}");
            }
            s.push('\n');
        }
    }

    // Wires.
    s.push_str("## Wires and jumpers\n\n");
    s.push_str("| ID | From | To | Net | Colour | Gauge / spec | Carries | Status | Notes |\n|---|---|---|---|---|---|---|---|---|\n");
    for w in wire_rows(ctx) {
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            md_cell(&w.id),
            md_cell(&w.from),
            md_cell(&w.to),
            md_cell(&w.net),
            md_cell(&w.color),
            md_cell(&w.spec),
            md_cell(&w.carries),
            md_cell(&w.status),
            md_cell(&w.note)
        );
    }
    s.push('\n');

    // Schematic connections.
    s.push_str("### Schematic connections (one line per drawn wire)\n\n| From | To | Net | Spec |\n|---|---|---|---|\n");
    for w in ctx.circuit.wires.values() {
        let (Some(a), Some(b)) = (endpoint_name(ctx, &w.a), endpoint_name(ctx, &w.b)) else { continue };
        let net = ctx.nl.net_of(&w.a).map(|n| ctx.net_name(n).to_string()).unwrap_or_default();
        let spec = if w.label.is_empty() { w.spec.clone() } else { format!("{} — {}", w.label, w.spec) };
        let _ = writeln!(s, "| {a} | {b} | {} | {} |", md_cell(&net), md_cell(&spec));
    }
    s.push('\n');

    s.push_str("## Bill of materials\n\n| Ref | Part | Value | Rating |\n|---|---|---|---|\n");
    for (c, d) in ctx.parts() {
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} |",
            part_ref(c),
            md_cell(&d.name),
            md_cell(&part_value(c, d)),
            md_cell(&part_rating(d))
        );
    }
    let _ = writeln!(s, "| {} | {} | — | — |", ctx.board_ref(), md_cell(&ctx.board.name));
    s
}

pub struct WireRow {
    pub id: String,
    pub from: String,
    pub to: String,
    pub net: String,
    pub color: String,
    pub spec: String,
    pub carries: String,
    pub status: String,
    pub note: String,
}

/// Physical wires: the perfboard runs when a build exists, else labelled schematic wires.
pub fn wire_rows(ctx: &DocCtx) -> Vec<WireRow> {
    let mut rows = Vec::new();
    if let Some(perf) = &ctx.circuit.perf {
        let nets = crate::perf::run_nets(perf, ctx);
        for (i, r) in perf.runs.iter().enumerate() {
            rows.push(WireRow {
                id: r.id.clone(),
                from: r.from.clone(),
                to: r.to.clone(),
                net: nets.get(i).cloned().unwrap_or_default(),
                color: r.color.clone(),
                spec: r.spec.clone(),
                carries: r.carries.clone(),
                status: format!("{:?}", r.status).to_lowercase(),
                note: r.note.clone(),
            });
        }
        return rows;
    }
    let mut by_label: HashMap<&str, ()> = HashMap::new();
    for w in ctx.circuit.wires.values() {
        if w.label.is_empty() || by_label.insert(&w.label, ()).is_some() {
            continue;
        }
        rows.push(WireRow {
            id: w.label.clone(),
            from: endpoint_name(ctx, &w.a).unwrap_or_default(),
            to: endpoint_name(ctx, &w.b).unwrap_or_default(),
            net: ctx.nl.net_of(&w.a).map(|n| ctx.net_name(n).to_string()).unwrap_or_default(),
            color: String::new(),
            spec: w.spec.clone(),
            carries: String::new(),
            status: "given".into(),
            note: String::new(),
        });
    }
    rows
}

fn csv_cell(s: &str) -> String {
    if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

pub fn wires_csv(ctx: &DocCtx) -> String {
    let mut s = String::from("id,from,to,net,colour,spec,carries,status,notes\n");
    for w in wire_rows(ctx) {
        let cells = [&w.id, &w.from, &w.to, &w.net, &w.color, &w.spec, &w.carries, &w.status, &w.note];
        let line: Vec<String> = cells.iter().map(|c| csv_cell(c)).collect();
        let _ = writeln!(s, "{}", line.join(","));
    }
    s
}

pub fn bom_csv(ctx: &DocCtx) -> String {
    let mut s = String::from("ref,part,value,rating\n");
    for (c, d) in ctx.parts() {
        let _ = writeln!(
            s,
            "{},{},{},{}",
            csv_cell(&part_ref(c)),
            csv_cell(&d.name),
            csv_cell(&part_value(c, d)),
            csv_cell(&part_rating(d))
        );
    }
    let _ = writeln!(s, "{},{},,", csv_cell(&ctx.board_ref()), csv_cell(&ctx.board.name));
    s
}
