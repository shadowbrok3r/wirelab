//! Node-graph programs: a dataflow of event sources, logic and actions that
//! compiles to a single Rhai script run by the ScriptHost as the synthetic
//! "flow" instance. Pulses propagate only during an event walk; Bool/Num/Text
//! values are levels cached in `this` between events.

use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

/// Value type carried by a node pin; wires must agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PinType {
    /// A momentary event edge; never stored.
    Pulse,
    Bool,
    Num,
    Text,
    /// Compatible with everything (Log, Script pins).
    Any,
}

impl PinType {
    pub fn accepts(self, from: PinType) -> bool {
        self == from || self == PinType::Any || from == PinType::Any
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CmpOp {
    Gt,
    Lt,
    Ge,
    Le,
    Eq,
    Ne,
}

impl CmpOp {
    pub fn symbol(self) -> &'static str {
        match self {
            CmpOp::Gt => ">",
            CmpOp::Lt => "<",
            CmpOp::Ge => ">=",
            CmpOp::Le => "<=",
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum NodeKind {
    // ---- events ----
    OnPress { comp: String },
    OnRelease { comp: String },
    /// Logical on/off level of an input component.
    OnChange { comp: String },
    /// Analog reading of a watched component, millivolts.
    OnReading { comp: String },
    OnUart,
    OnPin { gpio: u8 },
    /// Fires a pulse every `ms`.
    Every { ms: f64 },
    // ---- logic ----
    Compare { op: CmpOp, value: f64 },
    /// Hysteresis: on above `high`, off below `low`.
    Threshold { high: f64, low: f64 },
    Not,
    And,
    Or,
    /// Rising edge of a level -> pulse.
    Edge,
    /// Pulse flips the stored bool.
    Toggle,
    Counter { modulo: u32 },
    /// Re-emits the pulse `ms` later.
    Delay { ms: f64 },
    /// Passes the pulse only while the enable level is on.
    Gate,
    MapRange { in_min: f64, in_max: f64, out_min: f64, out_max: f64 },
    /// A Rhai expression over inputs `a`, `b`, `c`.
    Script { code: String, inputs: u8 },
    // ---- actions ----
    /// Level drives the component on/off.
    SetComp { comp: String },
    /// Pulse toggles the component.
    ToggleComp { comp: String },
    SetPin { gpio: u8 },
    Pwm { gpio: u8, freq_hz: u32 },
    /// Drives the on-board RGB LED from three levels (0-255).
    Rgb,
    /// Sends the incoming text on UART1.
    UartSend,
    /// Pulse sends a fixed line on UART1.
    SendText { text: String },
    LcdText { x: u8, y: u8 },
    Log { label: String },
}

/// Palette for the editor's add-node menu.
pub fn flow_categories() -> Vec<(&'static str, Vec<NodeKind>)> {
    vec![
        (
            "Events",
            vec![
            NodeKind::OnPress { comp: String::new() },
            NodeKind::OnRelease { comp: String::new() },
            NodeKind::OnChange { comp: String::new() },
            NodeKind::OnReading { comp: String::new() },
            NodeKind::OnUart,
            NodeKind::OnPin { gpio: 0 },
            NodeKind::Every { ms: 500.0 },
            ],
        ),
        (
            "Logic",
            vec![
            NodeKind::Compare { op: CmpOp::Gt, value: 1500.0 },
            NodeKind::Threshold { high: 2000.0, low: 1500.0 },
            NodeKind::Not,
            NodeKind::And,
            NodeKind::Or,
            NodeKind::Edge,
            NodeKind::Toggle,
            NodeKind::Counter { modulo: 4 },
            NodeKind::Delay { ms: 1000.0 },
            NodeKind::Gate,
            NodeKind::MapRange { in_min: 0.0, in_max: 3100.0, out_min: 0.0, out_max: 1000.0 },
            NodeKind::Script { code: String::from("a"), inputs: 1 },
            ],
        ),
        (
            "Actions",
            vec![
            NodeKind::SetComp { comp: String::new() },
            NodeKind::ToggleComp { comp: String::new() },
            NodeKind::SetPin { gpio: 2 },
            NodeKind::Pwm { gpio: 2, freq_hz: 1000 },
            NodeKind::Rgb,
            NodeKind::UartSend,
            NodeKind::SendText { text: String::from("hello\r\n") },
                NodeKind::LcdText { x: 4, y: 4 },
                NodeKind::Log { label: String::new() },
            ],
        ),
    ]
}

impl NodeKind {
    pub fn title(&self) -> String {
        match self {
            NodeKind::OnPress { comp } => format!("press: {}", pick(comp)),
            NodeKind::OnRelease { comp } => format!("release: {}", pick(comp)),
            NodeKind::OnChange { comp } => format!("level: {}", pick(comp)),
            NodeKind::OnReading { comp } => format!("reading: {}", pick(comp)),
            NodeKind::OnUart => "uart line".into(),
            NodeKind::OnPin { gpio } => format!("pin GPIO{gpio}"),
            NodeKind::Every { ms } => format!("every {ms} ms"),
            NodeKind::Compare { op, value } => format!("{} {value}", op.symbol()),
            NodeKind::Threshold { .. } => "threshold".into(),
            NodeKind::Not => "not".into(),
            NodeKind::And => "and".into(),
            NodeKind::Or => "or".into(),
            NodeKind::Edge => "rising edge".into(),
            NodeKind::Toggle => "toggle".into(),
            NodeKind::Counter { modulo } => format!("counter %{modulo}"),
            NodeKind::Delay { ms } => format!("delay {ms} ms"),
            NodeKind::Gate => "gate".into(),
            NodeKind::MapRange { .. } => "map range".into(),
            NodeKind::Script { .. } => "script".into(),
            NodeKind::SetComp { comp } => format!("set: {}", pick(comp)),
            NodeKind::ToggleComp { comp } => format!("toggle: {}", pick(comp)),
            NodeKind::SetPin { gpio } => format!("set GPIO{gpio}"),
            NodeKind::Pwm { gpio, .. } => format!("pwm GPIO{gpio}"),
            NodeKind::Rgb => "rgb led".into(),
            NodeKind::UartSend => "uart send".into(),
            NodeKind::SendText { .. } => "send text".into(),
            NodeKind::LcdText { .. } => "lcd text".into(),
            NodeKind::Log { .. } => "log".into(),
        }
    }

    pub fn inputs(&self) -> Vec<(&'static str, PinType)> {
        use PinType::*;
        match self {
            NodeKind::OnPress { .. }
            | NodeKind::OnRelease { .. }
            | NodeKind::OnChange { .. }
            | NodeKind::OnReading { .. }
            | NodeKind::OnUart
            | NodeKind::OnPin { .. }
            | NodeKind::Every { .. } => vec![],
            NodeKind::Compare { .. } | NodeKind::Threshold { .. } | NodeKind::MapRange { .. } => {
                vec![("in", Num)]
            }
            NodeKind::Not | NodeKind::Edge => vec![("in", Bool)],
            NodeKind::And | NodeKind::Or => vec![("a", Bool), ("b", Bool)],
            NodeKind::Toggle | NodeKind::Counter { .. } | NodeKind::Delay { .. } => {
                vec![("in", Pulse)]
            }
            NodeKind::Gate => vec![("in", Pulse), ("enable", Bool)],
            NodeKind::Script { inputs, .. } => ["a", "b", "c"]
                .iter()
                .take(*inputs as usize)
                .map(|n| (*n, Any))
                .collect(),
            NodeKind::SetComp { .. } | NodeKind::SetPin { .. } => vec![("on", Bool)],
            NodeKind::ToggleComp { .. } | NodeKind::SendText { .. } => vec![("in", Pulse)],
            NodeKind::Pwm { .. } => vec![("duty", Num)],
            NodeKind::Rgb => vec![("r", Num), ("g", Num), ("b", Num)],
            NodeKind::UartSend | NodeKind::LcdText { .. } => vec![("text", Text)],
            NodeKind::Log { .. } => vec![("in", Any)],
        }
    }

    pub fn outputs(&self) -> Vec<(&'static str, PinType)> {
        use PinType::*;
        match self {
            NodeKind::OnPress { .. }
            | NodeKind::OnRelease { .. }
            | NodeKind::Every { .. }
            | NodeKind::Edge
            | NodeKind::Delay { .. }
            | NodeKind::Gate => vec![("out", Pulse)],
            NodeKind::OnChange { .. }
            | NodeKind::OnPin { .. }
            | NodeKind::Compare { .. }
            | NodeKind::Threshold { .. }
            | NodeKind::Not
            | NodeKind::And
            | NodeKind::Or
            | NodeKind::Toggle => vec![("out", Bool)],
            NodeKind::OnReading { .. } | NodeKind::Counter { .. } | NodeKind::MapRange { .. } => {
                vec![("out", Num)]
            }
            NodeKind::OnUart => vec![("line", Text)],
            NodeKind::Script { .. } => vec![("out", Any)],
            _ => vec![],
        }
    }

    pub fn is_event(&self) -> bool {
        self.inputs().is_empty() && !self.outputs().is_empty()
    }
}

fn pick(comp: &str) -> &str {
    if comp.is_empty() { "?" } else { comp }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowNode {
    pub kind: NodeKind,
    pub pos: [f32; 2],
}

/// A wire from `(node, output)` to `(node, input)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowWire {
    pub from: (usize, usize),
    pub to: (usize, usize),
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FlowGraph {
    pub nodes: Vec<FlowNode>,
    pub wires: Vec<FlowWire>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FlowError {
    /// Index into `nodes`, when attributable.
    pub node: Option<usize>,
    pub msg: String,
}

/// Everything the Rhai generator needs, resolved once up front.
struct Ctx<'a> {
    graph: &'a FlowGraph,
    /// Nodes in dependency order.
    topo: Vec<usize>,
    /// wires into (node, input): at most one after validation.
    input_wire: std::collections::HashMap<(usize, usize), (usize, usize)>,
}

impl Ctx<'_> {
    /// Rhai expression for a level input; unwired pins read their default.
    fn input_expr(&self, node: usize, input: usize) -> String {
        let kind = &self.graph.nodes[node].kind;
        let (_, ty) = kind.inputs()[input];
        match self.input_wire.get(&(node, input)) {
            Some(&(src, out)) => format!("this.n{src}_{out}"),
            None => match ty {
                PinType::Bool => "false".into(),
                PinType::Num => "0".into(),
                PinType::Text => "\"\"".into(),
                _ => "()".into(),
            },
        }
    }

    /// Downstream consumers of one output pin.
    fn consumers(&self, node: usize, output: usize) -> Vec<(usize, usize)> {
        self.graph
            .wires
            .iter()
            .filter(|w| w.from == (node, output))
            .map(|w| (w.to.0, w.to.1))
            .collect()
    }
}

pub fn compile(graph: &FlowGraph) -> Result<String, Vec<FlowError>> {
    let mut errors = Vec::new();

    for (i, n) in graph.nodes.iter().enumerate() {
        let needs_comp = match &n.kind {
            NodeKind::OnPress { comp }
            | NodeKind::OnRelease { comp }
            | NodeKind::OnChange { comp }
            | NodeKind::OnReading { comp }
            | NodeKind::SetComp { comp }
            | NodeKind::ToggleComp { comp } => comp.is_empty(),
            _ => false,
        };
        if needs_comp {
            errors.push(FlowError {
                node: Some(i),
                msg: format!("'{}' needs a component picked", n.kind.title()),
            });
        }
    }

    let mut input_wire = std::collections::HashMap::new();
    for w in &graph.wires {
        let ok = w.from.0 < graph.nodes.len()
            && w.to.0 < graph.nodes.len()
            && w.from.1 < graph.nodes[w.from.0].kind.outputs().len()
            && w.to.1 < graph.nodes[w.to.0].kind.inputs().len();
        if !ok {
            errors.push(FlowError { node: None, msg: "dangling wire".into() });
            continue;
        }
        let from_ty = graph.nodes[w.from.0].kind.outputs()[w.from.1].1;
        let to_ty = graph.nodes[w.to.0].kind.inputs()[w.to.1].1;
        if !to_ty.accepts(from_ty) {
            errors.push(FlowError {
                node: Some(w.to.0),
                msg: format!(
                    "type mismatch into '{}': {from_ty:?} -> {to_ty:?}",
                    graph.nodes[w.to.0].kind.title()
                ),
            });
        }
        if input_wire.insert(w.to, w.from).is_some() {
            errors.push(FlowError {
                node: Some(w.to.0),
                msg: format!("two wires into one input of '{}'", graph.nodes[w.to.0].kind.title()),
            });
        }
    }

    // Kahn topological order over node-level dependencies.
    let n = graph.nodes.len();
    let mut indeg = vec![0usize; n];
    for w in &graph.wires {
        if w.from.0 < n && w.to.0 < n {
            indeg[w.to.0] += 1;
        }
    }
    let mut queue: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut topo = Vec::with_capacity(n);
    while let Some(i) = queue.pop() {
        topo.push(i);
        for w in graph.wires.iter().filter(|w| w.from.0 == i && w.to.0 < n) {
            indeg[w.to.0] -= 1;
            if indeg[w.to.0] == 0 {
                queue.push(w.to.0);
            }
        }
    }
    if topo.len() < n {
        errors.push(FlowError {
            node: (0..n).find(|i| indeg[*i] > 0),
            msg: "the flow has a cycle — wires must not loop back".into(),
        });
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    let ctx = Ctx { graph, topo, input_wire };
    let mut out = String::from(
        "// Generated from the Flow tab — edits here are overwritten.\n\
         fn as_int(x) { if type_of(x) == \"f64\" { x.to_int() } else { x } }\n\n",
    );

    emit_on_start(&mut out, &ctx);
    emit_event_group(&mut out, &ctx, "on_any_press", "who", |k| match k {
        NodeKind::OnPress { comp } => Some(comp.clone()),
        _ => None,
    });
    emit_event_group(&mut out, &ctx, "on_any_release", "who", |k| match k {
        NodeKind::OnRelease { comp } => Some(comp.clone()),
        _ => None,
    });
    emit_on_change(&mut out, &ctx);
    emit_on_reading(&mut out, &ctx);
    emit_on_uart(&mut out, &ctx);
    emit_on_pin(&mut out, &ctx);
    emit_on_tick(&mut out, &ctx);

    Ok(out)
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn state_keys(graph: &FlowGraph) -> Vec<(String, &'static str)> {
    let mut keys = Vec::new();
    for (i, node) in graph.nodes.iter().enumerate() {
        for (j, (_, ty)) in node.kind.outputs().iter().enumerate() {
            let init = match ty {
                PinType::Bool => "false",
                PinType::Num => "0",
                PinType::Text => "\"\"",
                PinType::Any => "()",
                PinType::Pulse => continue,
            };
            keys.push((format!("n{i}_{j}"), init));
        }
        match node.kind {
            NodeKind::Edge => keys.push((format!("n{i}_prev"), "false")),
            NodeKind::Delay { .. } => keys.push((format!("n{i}_t"), "0.0")),
            NodeKind::Every { .. } => keys.push((format!("n{i}_acc"), "0.0")),
            _ => {}
        }
    }
    keys
}

fn emit_on_start(out: &mut String, ctx: &Ctx) {
    let _ = writeln!(out, "fn on_start() {{");
    for (key, init) in state_keys(ctx.graph) {
        let _ = writeln!(out, "    this.{key} = {init};");
    }
    let _ = writeln!(out, "}}\n");
}

/// Emit the statements that react to node `i`'s outputs having just changed
/// (levels updated, and `pulse_expr` = Rhai bool expr for the pulse, if any).
fn emit_downstream(out: &mut String, ctx: &Ctx, i: usize, pulse_expr: &str, indent: usize) {
    // Recompute every level node reachable from i in topo order, firing
    // pulse consumers along the way.
    let mut live = std::collections::HashSet::new();
    live.insert(i);
    let mut stack = vec![i];
    while let Some(k) = stack.pop() {
        // A delay's downstream fires from on_tick, not in this walk — stop
        // there (unless the walk IS the delay firing).
        if k != i && matches!(ctx.graph.nodes[k].kind, NodeKind::Delay { .. }) {
            continue;
        }
        for (j, _) in ctx.graph.nodes[k].kind.outputs().iter().enumerate() {
            for (m, _) in ctx.consumers(k, j) {
                if live.insert(m) {
                    stack.push(m);
                }
            }
        }
    }
    // Pulse locals per node, valid within this walk only.
    let pad = " ".repeat(indent);
    let _ = writeln!(out, "{pad}let p{i} = {pulse_expr};");
    for &k in &ctx.topo {
        if k == i || !live.contains(&k) {
            continue;
        }
        emit_node(out, ctx, &live, k, indent);
    }
}

/// The Rhai bool expression for the pulse arriving at node `k`'s pulse input:
/// false when the feeding source is not part of this walk.
fn pulse_in(
    ctx: &Ctx,
    live: &std::collections::HashSet<usize>,
    k: usize,
    input: usize,
) -> String {
    match ctx.input_wire.get(&(k, input)) {
        Some(&(src, _)) if live.contains(&src) => format!("p{src}"),
        _ => "false".into(),
    }
}

fn emit_node(
    out: &mut String,
    ctx: &Ctx,
    live: &std::collections::HashSet<usize>,
    k: usize,
    indent: usize,
) {
    let pad = " ".repeat(indent);
    let kind = &ctx.graph.nodes[k].kind;
    match kind {
        // Level logic: recompute the cached output.
        NodeKind::Compare { op, value } => {
            let e = ctx.input_expr(k, 0);
            let _ = writeln!(out, "{pad}this.n{k}_0 = {e} {} {value};", op.symbol());
        }
        NodeKind::Threshold { high, low } => {
            let e = ctx.input_expr(k, 0);
            let _ = writeln!(
                out,
                "{pad}if {e} >= {high} {{ this.n{k}_0 = true; }} else if {e} <= {low} {{ this.n{k}_0 = false; }}"
            );
        }
        NodeKind::Not => {
            let e = ctx.input_expr(k, 0);
            let _ = writeln!(out, "{pad}this.n{k}_0 = !{e};");
        }
        NodeKind::And => {
            let (a, b) = (ctx.input_expr(k, 0), ctx.input_expr(k, 1));
            let _ = writeln!(out, "{pad}this.n{k}_0 = {a} && {b};");
        }
        NodeKind::Or => {
            let (a, b) = (ctx.input_expr(k, 0), ctx.input_expr(k, 1));
            let _ = writeln!(out, "{pad}this.n{k}_0 = {a} || {b};");
        }
        NodeKind::MapRange { in_min, in_max, out_min, out_max } => {
            let e = ctx.input_expr(k, 0);
            let span_in = in_max - in_min;
            let span = if span_in.abs() < f64::EPSILON { 1.0 } else { span_in };
            let _ = writeln!(
                out,
                "{pad}this.n{k}_0 = ({e} - {in_min}) * {:.6} + {out_min};",
                (out_max - out_min) / span
            );
        }
        NodeKind::Script { code, inputs } => {
            let mut binds = String::new();
            for (idx, name) in ["a", "b", "c"].iter().take(*inputs as usize).enumerate() {
                let _ = write!(binds, "let {name} = {}; ", ctx.input_expr(k, idx));
            }
            let _ = writeln!(out, "{pad}this.n{k}_0 = {{ {binds}({code}) }};");
        }
        // Edge: level in, pulse out.
        NodeKind::Edge => {
            let e = ctx.input_expr(k, 0);
            let _ = writeln!(out, "{pad}let p{k} = {e} && !this.n{k}_prev; this.n{k}_prev = {e};");
        }
        // Pulse-driven state.
        NodeKind::Toggle => {
            let p = pulse_in(ctx, live, k, 0);
            let _ = writeln!(out, "{pad}if {p} {{ this.n{k}_0 = !this.n{k}_0; }}");
        }
        NodeKind::Counter { modulo } => {
            let p = pulse_in(ctx, live, k, 0);
            let m = (*modulo).max(1);
            let _ = writeln!(out, "{pad}if {p} {{ this.n{k}_0 = (this.n{k}_0 + 1) % {m}; }}");
        }
        NodeKind::Delay { ms } => {
            let p = pulse_in(ctx, live, k, 0);
            let _ = writeln!(out, "{pad}if {p} {{ this.n{k}_t = {ms:.1}; }}");
            // Its pulse fires from on_tick, never within this walk.
            let _ = writeln!(out, "{pad}let p{k} = false;");
        }
        NodeKind::Gate => {
            let p = pulse_in(ctx, live, k, 0);
            let e = ctx.input_expr(k, 1);
            let _ = writeln!(out, "{pad}let p{k} = {p} && {e};");
        }
        // Actions.
        NodeKind::SetComp { comp } => {
            let e = ctx.input_expr(k, 0);
            let c = esc(comp);
            let _ = writeln!(
                out,
                "{pad}if {e} {{ comp(\"{c}\").on(); }} else {{ comp(\"{c}\").off(); }}"
            );
        }
        NodeKind::ToggleComp { comp } => {
            let p = pulse_in(ctx, live, k, 0);
            let _ = writeln!(out, "{pad}if {p} {{ comp(\"{}\").toggle(); }}", esc(comp));
        }
        NodeKind::SetPin { gpio } => {
            let e = ctx.input_expr(k, 0);
            let _ = writeln!(out, "{pad}pin({gpio}).set({e});");
        }
        NodeKind::Pwm { gpio, freq_hz } => {
            let e = ctx.input_expr(k, 0);
            let _ = writeln!(out, "{pad}pin({gpio}).pwm({freq_hz}, as_int({e}));");
        }
        NodeKind::Rgb => {
            let (r, g, b) =
                (ctx.input_expr(k, 0), ctx.input_expr(k, 1), ctx.input_expr(k, 2));
            let _ = writeln!(out, "{pad}rgb(as_int({r}), as_int({g}), as_int({b}));");
        }
        NodeKind::UartSend => {
            let e = ctx.input_expr(k, 0);
            let _ = writeln!(out, "{pad}uart_send({e});");
        }
        NodeKind::SendText { text } => {
            let p = pulse_in(ctx, live, k, 0);
            let _ = writeln!(out, "{pad}if {p} {{ uart_send(\"{}\"); }}", esc(text));
        }
        NodeKind::LcdText { x, y } => {
            let e = ctx.input_expr(k, 0);
            let _ = writeln!(out, "{pad}lcd_text({x}, {y}, `${{{e}}}`, 255, 255, 255);");
        }
        NodeKind::Log { label } => {
            let has_pulse_in = matches!(
                ctx.input_wire
                    .get(&(k, 0))
                    .map(|&(src, o)| ctx.graph.nodes[src].kind.outputs()[o].1),
                Some(PinType::Pulse)
            );
            let l = esc(label);
            if has_pulse_in {
                let p = pulse_in(ctx, live, k, 0);
                let _ = writeln!(out, "{pad}if {p} {{ log(\"{l}\"); }}");
            } else {
                let e = ctx.input_expr(k, 0);
                if label.is_empty() {
                    let _ = writeln!(out, "{pad}log(`${{{e}}}`);");
                } else {
                    let _ = writeln!(out, "{pad}log(`{l}: ${{{e}}}`);");
                }
            }
        }
        // Event sources emit nothing downstream of themselves.
        _ => {}
    }
}

fn emit_event_group(
    out: &mut String,
    ctx: &Ctx,
    callback: &str,
    arg: &str,
    comp_of: impl Fn(&NodeKind) -> Option<String>,
) {
    let sources: Vec<(usize, String)> = ctx
        .graph
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| comp_of(&n.kind).map(|c| (i, c)))
        .collect();
    if sources.is_empty() {
        return;
    }
    let _ = writeln!(out, "fn {callback}({arg}) {{");
    for (i, comp) in sources {
        let _ = writeln!(out, "    if {arg} == \"{}\" {{", esc(&comp));
        emit_downstream(out, ctx, i, "true", 8);
        let _ = writeln!(out, "    }}");
    }
    let _ = writeln!(out, "}}\n");
}

fn emit_on_change(out: &mut String, ctx: &Ctx) {
    let sources: Vec<(usize, String)> = ctx
        .graph
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| match &n.kind {
            NodeKind::OnChange { comp } => Some((i, comp.clone())),
            _ => None,
        })
        .collect();
    if sources.is_empty() {
        return;
    }
    let _ = writeln!(out, "fn on_any_change(who, on) {{");
    for (i, comp) in sources {
        let _ = writeln!(out, "    if who == \"{}\" {{", esc(&comp));
        let _ = writeln!(out, "        this.n{i}_0 = on;");
        emit_downstream(out, ctx, i, "false", 8);
        let _ = writeln!(out, "    }}");
    }
    let _ = writeln!(out, "}}\n");
}

fn emit_on_reading(out: &mut String, ctx: &Ctx) {
    let sources: Vec<(usize, String)> = ctx
        .graph
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| match &n.kind {
            NodeKind::OnReading { comp } => Some((i, comp.clone())),
            _ => None,
        })
        .collect();
    if sources.is_empty() {
        return;
    }
    let _ = writeln!(out, "fn on_any_reading(who, mv) {{");
    for (i, comp) in sources {
        let _ = writeln!(out, "    if who == \"{}\" {{", esc(&comp));
        let _ = writeln!(out, "        this.n{i}_0 = mv;");
        emit_downstream(out, ctx, i, "false", 8);
        let _ = writeln!(out, "    }}");
    }
    let _ = writeln!(out, "}}\n");
}

fn emit_on_uart(out: &mut String, ctx: &Ctx) {
    let sources: Vec<usize> = ctx
        .graph
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| matches!(n.kind, NodeKind::OnUart).then_some(i))
        .collect();
    if sources.is_empty() {
        return;
    }
    let _ = writeln!(out, "fn on_uart(line) {{");
    for i in sources {
        let _ = writeln!(out, "    this.n{i}_0 = line;");
        emit_downstream(out, ctx, i, "false", 4);
    }
    let _ = writeln!(out, "}}\n");
}

fn emit_on_pin(out: &mut String, ctx: &Ctx) {
    let sources: Vec<(usize, u8)> = ctx
        .graph
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| match n.kind {
            NodeKind::OnPin { gpio } => Some((i, gpio)),
            _ => None,
        })
        .collect();
    if sources.is_empty() {
        return;
    }
    let _ = writeln!(out, "fn on_pin(gpio, high) {{");
    for (i, g) in sources {
        let _ = writeln!(out, "    if gpio == {g} {{");
        let _ = writeln!(out, "        this.n{i}_0 = high;");
        emit_downstream(out, ctx, i, "false", 8);
        let _ = writeln!(out, "    }}");
    }
    let _ = writeln!(out, "}}\n");
}

fn emit_on_tick(out: &mut String, ctx: &Ctx) {
    let every: Vec<(usize, f64)> = ctx
        .graph
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| match n.kind {
            NodeKind::Every { ms } => Some((i, ms)),
            _ => None,
        })
        .collect();
    let delays: Vec<usize> = ctx
        .graph
        .nodes
        .iter()
        .enumerate()
        .filter_map(|(i, n)| matches!(n.kind, NodeKind::Delay { .. }).then_some(i))
        .collect();
    if every.is_empty() && delays.is_empty() {
        return;
    }
    let _ = writeln!(out, "fn on_tick(dt) {{");
    for (i, ms) in every {
        let period = ms.max(10.0);
        let _ = writeln!(out, "    this.n{i}_acc += dt;");
        let _ = writeln!(out, "    if this.n{i}_acc >= {period:.1} {{");
        let _ = writeln!(out, "        this.n{i}_acc = 0.0;");
        emit_downstream(out, ctx, i, "true", 8);
        let _ = writeln!(out, "    }}");
    }
    for i in delays {
        let _ = writeln!(out, "    if this.n{i}_t > 0.0 {{");
        let _ = writeln!(out, "        this.n{i}_t -= dt;");
        let _ = writeln!(out, "        if this.n{i}_t <= 0.0 {{");
        emit_downstream(out, ctx, i, "true", 12);
        let _ = writeln!(out, "        }}");
        let _ = writeln!(out, "    }}");
    }
    let _ = writeln!(out, "}}\n");
}
