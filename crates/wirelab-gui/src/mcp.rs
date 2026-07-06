//! Embedded MCP server (rmcp, streamable HTTP): lets an AI inspect the live
//! circuit, place components, draw verdict-checked wires, run validation
//! with fixes, write scripts, edit the flow node-graph and poke the
//! simulator — against the running app. Tools marshal onto the GUI thread
//! through a channel; the UI drains it once per frame.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use serde_json::{Value, json};
use wirelab_core::circuit::{CompId, Endpoint, PlacedComponent, WireId};
use wirelab_core::component::CompState;
use wirelab_core::flow::FlowGraph;
use wirelab_core::netlist::{Netlist, WireVerdict, wire_verdict};
use wirelab_core::validate::{LintFix, Severity, validate};

use crate::app::WireLabApp;

pub const DEFAULT_PORT: u16 = 4517;

// ---------------------------------------------------------------- bridge --

#[derive(Debug)]
pub enum Cmd {
    GetCircuit,
    ListLibrary,
    Validate,
    AddComponent { def_id: String, x: f32, y: f32, label: Option<String> },
    RemoveComponent { comp: String },
    AddWire { a: EndpointArg, b: EndpointArg },
    RemoveWire { id: u32 },
    AutoWire { comps: Vec<String> },
    FixLints,
    SetScript { comp: String, source: String },
    GetScript { comp: String },
    ConnectSimulator,
    Disconnect,
    SetComponentState { comp: String, pressed: Option<bool>, on: Option<bool>, value: Option<f32> },
    ReadLive,
    OpenExample { name: String },
    GetFlow,
    SetFlow { nodes: Value, wires: Value },
}

#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
pub struct EndpointArg {
    /// Board pin key, e.g. "GPIO2", "GND1", "3V3". Mutually exclusive with comp.
    pub pin: Option<String>,
    /// Component id (number) or script name / label, e.g. "red_led".
    pub comp: Option<String>,
    /// Terminal id on that component, e.g. "anode", "a", "signal".
    pub terminal: Option<String>,
}

pub struct McpRequest {
    pub cmd: Cmd,
    pub reply: tokio::sync::oneshot::Sender<Result<Value, String>>,
}

// ---------------------------------------------------------------- server --

pub struct McpServer {
    pub rx: Receiver<McpRequest>,
    pub addr: String,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl McpServer {
    pub fn start(port: u16, ctx: egui::Context) -> Result<McpServer, String> {
        let (tx, rx) = channel::<McpRequest>();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let (ready_tx, ready_rx) = channel::<Result<(), String>>();

        let thread = std::thread::Builder::new()
            .name("wirelab-mcp".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                rt.block_on(async move {
                    let mut config = StreamableHttpServerConfig::default();
                    config.stateful_mode = false;
                    config.json_response = true;
                    let service = StreamableHttpService::new(
                        move || Ok(WirelabTools::new(tx.clone(), ctx.clone())),
                        Arc::new(LocalSessionManager::default()),
                        config,
                    );
                    let router = axum::Router::new().nest_service("/mcp", service);
                    let listener =
                        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                            Ok(l) => l,
                            Err(e) => {
                                let _ = ready_tx.send(Err(e.to_string()));
                                return;
                            }
                        };
                    let _ = ready_tx.send(Ok(()));
                    let _ = axum::serve(listener, router)
                        .with_graceful_shutdown(async {
                            let _ = stop_rx.await;
                        })
                        .await;
                });
            })
            .map_err(|e| e.to_string())?;

        match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(McpServer {
                rx,
                addr: format!("http://127.0.0.1:{port}/mcp"),
                shutdown: Some(stop_tx),
                thread: Some(thread),
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("MCP server did not start in time".into()),
        }
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ----------------------------------------------------------------- tools --

#[derive(Clone)]
pub struct WirelabTools {
    tx: Sender<McpRequest>,
    /// Wakes the (otherwise idle) UI so the bridge drains promptly.
    ctx: egui::Context,
    #[allow(dead_code)] // read by the #[tool_handler] macro expansion
    tool_router: ToolRouter<Self>,
}

impl WirelabTools {
    fn new(tx: Sender<McpRequest>, ctx: egui::Context) -> Self {
        Self { tx, ctx, tool_router: Self::tool_router() }
    }

    async fn ask(&self, cmd: Cmd) -> String {
        let (reply, rx) = tokio::sync::oneshot::channel();
        if self.tx.send(McpRequest { cmd, reply }).is_err() {
            return "error: WireLab is shutting down".into();
        }
        self.ctx.request_repaint();
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(Ok(v))) => serde_json::to_string_pretty(&v).unwrap_or_default(),
            Ok(Ok(Err(e))) => format!("error: {e}"),
            _ => "error: the WireLab UI did not respond (is the window open?)".into(),
        }
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct AddComponentArgs {
    /// Library definition id, e.g. "led-red", "resistor-220", "push-button".
    pub def_id: String,
    /// Canvas position in millimetres (the board sits at 0,0; free space is x > 55).
    pub x: f32,
    pub y: f32,
    /// Label; becomes the script name (e.g. "red_led").
    pub label: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct CompArg {
    /// Component id (number) or its script name / label.
    pub comp: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct AddWireArgs {
    pub a: EndpointArg,
    pub b: EndpointArg,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct RemoveWireArgs {
    pub id: u32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct AutoWireArgs {
    /// Components to wire (ids or names); empty = every unwired component.
    pub comps: Option<Vec<String>>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct SetScriptArgs {
    /// Component id (number) or script name / label.
    pub comp: String,
    /// Rhai source; callbacks like on_press/on_tick run live once applied.
    pub source: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct SetStateArgs {
    /// Component id (number) or script name / label.
    pub comp: String,
    /// Push buttons: hold state (true/false).
    pub pressed: Option<Value>,
    /// Switches / digital sensors (true/false).
    pub on: Option<Value>,
    /// Pots / sensors: 0.0..=1.0.
    pub value: Option<Value>,
}

/// Clients following a loose schema may send "true" or "0.5" as strings.
fn as_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => Some(n.as_f64().unwrap_or(0.0) != 0.0),
        _ => None,
    }
}

fn as_f32(v: &Value) -> Option<f32> {
    match v {
        Value::Number(n) => n.as_f64().map(|f| f as f32),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct OpenExampleArgs {
    /// Substring of the example name, e.g. "reaction" or "night".
    pub name: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct SetFlowArgs {
    /// Node list: [{"kind": {"t": "OnPress", "comp": "btn"}, "pos": [x, y]}, ...].
    pub nodes: Value,
    /// Wire list: [{"from": [node, output_pin], "to": [node, input_pin]}, ...].
    pub wires: Value,
}

/// Positions nodes still at [0,0]: column = topological depth, row = order within the column.
fn auto_layout_new_nodes(graph: &mut FlowGraph) {
    let n = graph.nodes.len();
    let mut depth = vec![0usize; n];
    for _ in 0..n {
        for w in &graph.wires {
            if w.from.0 < n && w.to.0 < n && depth[w.to.0] < depth[w.from.0] + 1 {
                depth[w.to.0] = depth[w.from.0] + 1;
            }
        }
    }
    let mut rows: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    for (i, node) in graph.nodes.iter_mut().enumerate() {
        if node.pos == [0.0, 0.0] {
            let row = rows.entry(depth[i]).or_insert(0);
            node.pos = [40.0 + 220.0 * depth[i] as f32, 40.0 + 110.0 * *row as f32];
            *row += 1;
        }
    }
}

#[tool_router]
impl WirelabTools {
    #[tool(description = "Current project: board, components (with script names), wires, GPIO bindings and validation results.")]
    async fn get_circuit(&self) -> String {
        self.ask(Cmd::GetCircuit).await
    }

    #[tool(description = "Available boards and the component library (ids, terminals, roles) for add_component / add_wire.")]
    async fn list_library(&self) -> String {
        self.ask(Cmd::ListLibrary).await
    }

    #[tool(description = "Run electrical validation: shorts, floating parts, missing LED resistors (with computed values and fixability).")]
    async fn validate_circuit(&self) -> String {
        self.ask(Cmd::Validate).await
    }

    #[tool(description = "Place a component from the library on the canvas.")]
    async fn add_component(&self, Parameters(a): Parameters<AddComponentArgs>) -> String {
        self.ask(Cmd::AddComponent { def_id: a.def_id, x: a.x, y: a.y, label: a.label }).await
    }

    #[tool(description = "Remove a component (and its wires).")]
    async fn remove_component(&self, Parameters(a): Parameters<CompArg>) -> String {
        self.ask(Cmd::RemoveComponent { comp: a.comp }).await
    }

    #[tool(description = "Wire two points together. Endpoints are a board pin OR a component terminal. Electrically dangerous wires (rail shorts) are refused with the reason.")]
    async fn add_wire(&self, Parameters(a): Parameters<AddWireArgs>) -> String {
        self.ask(Cmd::AddWire { a: a.a, b: a.b }).await
    }

    #[tool(description = "Delete a wire by id (ids come from get_circuit).")]
    async fn remove_wire(&self, Parameters(a): Parameters<RemoveWireArgs>) -> String {
        self.ask(Cmd::RemoveWire { id: a.id }).await
    }

    #[tool(description = "Automatically wire components to sensible free board pins (buttons get pull-up inputs, LEDs pair with selected resistors, pots land on ADC pins).")]
    async fn auto_wire(&self, Parameters(a): Parameters<AutoWireArgs>) -> String {
        self.ask(Cmd::AutoWire { comps: a.comps.unwrap_or_default() }).await
    }

    #[tool(description = "Apply every fixable validation warning (e.g. splice computed series resistors into LED wires).")]
    async fn fix_lints(&self) -> String {
        self.ask(Cmd::FixLints).await
    }

    #[tool(description = "Attach or replace a component's Rhai script; returns analyzer diagnostics. Scripts run live once the device is connected.")]
    async fn set_script(&self, Parameters(a): Parameters<SetScriptArgs>) -> String {
        self.ask(Cmd::SetScript { comp: a.comp, source: a.source }).await
    }

    #[tool(description = "Read a component's current script.")]
    async fn get_script(&self, Parameters(a): Parameters<CompArg>) -> String {
        self.ask(Cmd::GetScript { comp: a.comp }).await
    }

    #[tool(description = "Connect the in-process circuit simulator (auto pin setup applies; scripts go live).")]
    async fn connect_simulator(&self) -> String {
        self.ask(Cmd::ConnectSimulator).await
    }

    #[tool(description = "Disconnect the current device or simulator.")]
    async fn disconnect(&self) -> String {
        self.ask(Cmd::Disconnect).await
    }

    #[tool(description = "Poke a component's live state: press/release a button, flip a switch, move a pot (0..1).")]
    async fn set_component_state(&self, Parameters(a): Parameters<SetStateArgs>) -> String {
        self.ask(Cmd::SetComponentState {
            comp: a.comp,
            pressed: a.pressed.as_ref().and_then(as_bool),
            on: a.on.as_ref().and_then(as_bool),
            value: a.value.as_ref().and_then(as_f32),
        })
        .await
    }

    #[tool(description = "Live readout: pin levels, analog millivolts, component visual states (LED brightness…), RGB LED color, recent console lines.")]
    async fn read_live(&self) -> String {
        self.ask(Cmd::ReadLive).await
    }

    #[tool(description = "Open one of the shipped example projects by name substring.")]
    async fn open_example(&self, Parameters(a): Parameters<OpenExampleArgs>) -> String {
        self.ask(Cmd::OpenExample { name: a.name }).await
    }

    #[tool(description = "The Rhai scripting reference: callbacks, component verbs, pin/rgb/timer APIs and language notes.")]
    async fn get_scripting_reference(&self) -> String {
        crate::rhai_docs::reference_text()
    }

    #[tool(description = "Read the flow node-graph: nodes, wires, the Rhai it compiles to, and any compile errors.")]
    async fn get_flow(&self) -> String {
        self.ask(Cmd::GetFlow).await
    }

    #[tool(description = "Replace the flow node-graph. Nodes are {\"kind\": {...}, \"pos\": [x, y]} where kind is \"t\"-tagged JSON, e.g. {\"t\":\"OnPress\",\"comp\":\"btn\"}, {\"t\":\"Every\",\"ms\":500.0}, {\"t\":\"Compare\",\"op\":\"Gt\",\"value\":1500.0}, {\"t\":\"Toggle\"}, {\"t\":\"Delay\",\"ms\":1000.0}, {\"t\":\"SetComp\",\"comp\":\"led\"}, {\"t\":\"Log\",\"label\":\"hi\"}. Kinds — events: OnPress/OnRelease/OnChange/OnReading/OnUart/OnPin/Every; logic: Compare/Threshold/Not/And/Or/Edge/Toggle/Counter/Delay/Gate/MapRange/Script; actions: SetComp/ToggleComp/SetPin/Pwm/Rgb/UartSend/SendText/LcdText/Log. Wires are {\"from\": [node_index, output_pin], \"to\": [node_index, input_pin]}. Nodes at pos [0,0] are auto-laid out. A graph with compile errors is rejected (errors returned); an empty graph clears the flow. On success returns the generated Rhai.")]
    async fn set_flow(&self, Parameters(a): Parameters<SetFlowArgs>) -> String {
        self.ask(Cmd::SetFlow { nodes: a.nodes, wires: a.wires }).await
    }
}

#[tool_handler]
impl ServerHandler for WirelabTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "WireLab: a live ESP32 circuit builder. Inspect with get_circuit / \
             validate_circuit, build with add_component / add_wire / auto_wire / \
             fix_lints, program with set_script (see get_scripting_reference) or \
             the node-graph via get_flow / set_flow, and test with \
             connect_simulator + set_component_state + read_live.",
        )
    }
}

// -------------------------------------------------------------- executor --

impl WireLabApp {
    /// Drain pending MCP requests on the UI thread; true when any ran.
    pub fn pump_mcp(&mut self) -> bool {
        let reqs: Vec<McpRequest> = match &self.mcp {
            Some(server) => server.rx.try_iter().collect(),
            None => return false,
        };
        let any = !reqs.is_empty();
        for req in reqs {
            let result = self.exec_mcp(req.cmd);
            let _ = req.reply.send(result);
        }
        any
    }

    fn resolve_comp(&self, spec: &str) -> Result<CompId, String> {
        if let Ok(n) = spec.parse::<u32>() {
            let id = CompId(n);
            if self.project.circuit.components.contains_key(&id) {
                return Ok(id);
            }
        }
        let names = wirelab_core::script::component_names(&self.project.circuit, &self.lib);
        if let Some((id, _)) = names.iter().find(|(_, n)| n.as_str() == spec) {
            return Ok(*id);
        }
        if let Some(c) = self.project.circuit.components.values().find(|c| c.label == spec) {
            return Ok(c.id);
        }
        Err(format!("no component '{spec}' (use an id or a script name from get_circuit)"))
    }

    fn resolve_endpoint(&self, arg: &EndpointArg) -> Result<Endpoint, String> {
        match (&arg.pin, &arg.comp) {
            (Some(pin), None) => {
                let board = self.lib.board(&self.project.circuit.board_id).ok_or("no board")?;
                let key = board
                    .pins
                    .iter()
                    .find(|p| p.key.eq_ignore_ascii_case(pin))
                    .map(|p| p.key.clone())
                    .ok_or_else(|| format!("board has no pin '{pin}'"))?;
                Ok(Endpoint::BoardPin { key })
            }
            (None, Some(comp)) => {
                let id = self.resolve_comp(comp)?;
                let def = self
                    .project
                    .circuit
                    .components
                    .get(&id)
                    .and_then(|c| self.lib.component(&c.def_id))
                    .ok_or("component definition missing")?;
                let want = arg.terminal.as_deref().ok_or("terminal is required with comp")?;
                let t = def
                    .terminals
                    .iter()
                    .find(|t| t.id.eq_ignore_ascii_case(want))
                    .ok_or_else(|| {
                        format!(
                            "'{want}' is not a terminal of {} (has: {})",
                            def.name,
                            def.terminals.iter().map(|t| t.id.as_str()).collect::<Vec<_>>().join(", ")
                        )
                    })?;
                Ok(Endpoint::Terminal { comp: id, terminal: t.id.clone() })
            }
            _ => Err("endpoint needs either pin OR comp+terminal".into()),
        }
    }

    fn describe_endpoint(&self, ep: &Endpoint, names: &std::collections::HashMap<CompId, String>) -> Value {
        match ep {
            Endpoint::BoardPin { key } => json!({ "pin": key }),
            Endpoint::Terminal { comp, terminal } => json!({
                "comp": names.get(comp).cloned().unwrap_or_else(|| comp.0.to_string()),
                "terminal": terminal,
            }),
        }
    }

    fn lints_json(&self) -> Value {
        let Some(board) = self.lib.board(&self.project.circuit.board_id) else {
            return json!([]);
        };
        let nl = Netlist::build(&self.project.circuit, board, &self.lib);
        let lints = validate(&self.project.circuit, board, &self.lib, &nl);
        let names = wirelab_core::script::component_names(&self.project.circuit, &self.lib);
        Value::Array(
            lints
                .iter()
                .map(|l| {
                    json!({
                        "severity": match l.severity {
                            Severity::Error => "error",
                            Severity::Warning => "warning",
                            Severity::Info => "info",
                        },
                        "message": l.message,
                        "components": l.comps.iter()
                            .filter_map(|c| names.get(c).cloned()).collect::<Vec<_>>(),
                        "fixable": l.fix.is_some(),
                    })
                })
                .collect(),
        )
    }

    fn exec_mcp(&mut self, cmd: Cmd) -> Result<Value, String> {
        let names = wirelab_core::script::component_names(&self.project.circuit, &self.lib);
        match cmd {
            Cmd::GetCircuit => {
                let comps: Vec<Value> = self
                    .project
                    .circuit
                    .components
                    .values()
                    .map(|c| {
                        json!({
                            "id": c.id.0,
                            "def_id": c.def_id,
                            "name": names.get(&c.id),
                            "label": c.label,
                            "pos": c.pos,
                            "has_script": c.script.is_some(),
                        })
                    })
                    .collect();
                let wires: Vec<Value> = self
                    .project
                    .circuit
                    .wires
                    .values()
                    .map(|w| {
                        json!({
                            "id": w.id.0,
                            "a": self.describe_endpoint(&w.a, &names),
                            "b": self.describe_endpoint(&w.b, &names),
                        })
                    })
                    .collect();
                Ok(json!({
                    "project": self.project.name,
                    "board": self.project.circuit.board_id,
                    "components": comps,
                    "wires": wires,
                    "connected": self.live.connected(),
                    "lints": self.lints_json(),
                }))
            }
            Cmd::ListLibrary => {
                let comps: Vec<Value> = self
                    .lib
                    .components
                    .values()
                    .map(|d| {
                        json!({
                            "def_id": d.id,
                            "name": d.name,
                            "category": d.category,
                            "description": d.description,
                            "terminals": d.terminals.iter()
                                .map(|t| json!({"id": t.id, "role": format!("{:?}", t.role)}))
                                .collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                let boards: Vec<Value> = self
                    .lib
                    .boards
                    .values()
                    .map(|b| json!({"id": b.id, "name": b.name, "chip": b.chip.name()}))
                    .collect();
                Ok(json!({ "components": comps, "boards": boards }))
            }
            Cmd::Validate => Ok(json!({
                "lints": self.lints_json(),
                "setup_warnings": self.cache.bindings.warnings,
            })),
            Cmd::AddComponent { def_id, x, y, label } => {
                let def = self
                    .lib
                    .component(&def_id)
                    .ok_or_else(|| format!("unknown def_id '{def_id}' (see list_library)"))?
                    .clone();
                let id = self.project.circuit.add_component(PlacedComponent {
                    id: CompId(0),
                    def_id,
                    pos: [x.round(), y.round()],
                    rotation: 0,
                    label: label.unwrap_or_default(),
                    props: Default::default(),
                    state: CompState::initial(&def.sim),
                    script: None,
                });
                self.topo_rev += 1;
                let names =
                    wirelab_core::script::component_names(&self.project.circuit, &self.lib);
                Ok(json!({ "id": id.0, "script_name": names.get(&id) }))
            }
            Cmd::RemoveComponent { comp } => {
                let id = self.resolve_comp(&comp)?;
                self.project.circuit.remove_component(id);
                self.topo_rev += 1;
                Ok(json!({ "removed": id.0 }))
            }
            Cmd::AddWire { a, b } => {
                let ea = self.resolve_endpoint(&a)?;
                let eb = self.resolve_endpoint(&b)?;
                let board = self.lib.board(&self.project.circuit.board_id).ok_or("no board")?;
                let nl = Netlist::build(&self.project.circuit, board, &self.lib);
                let outs: Vec<u8> =
                    self.cache.bindings.outputs.values().map(|o| o.gpio).collect();
                match wire_verdict(&nl, board, &ea, &eb, &outs) {
                    WireVerdict::Blocked(why) => Err(format!("refused: {why}")),
                    WireVerdict::Redundant => {
                        Err("those points are already connected".into())
                    }
                    WireVerdict::Ok => {
                        let color = [190, 120, 255];
                        let id = self.project.circuit.add_wire(ea, eb, color);
                        self.topo_rev += 1;
                        Ok(json!({ "wire": id.0 }))
                    }
                }
            }
            Cmd::RemoveWire { id } => {
                let wid = WireId(id);
                if !self.project.circuit.wires.contains_key(&wid) {
                    return Err(format!("no wire {id}"));
                }
                self.project.circuit.remove_wire(wid);
                self.topo_rev += 1;
                Ok(json!({ "removed": id }))
            }
            Cmd::AutoWire { comps } => {
                let ids: Vec<CompId> = if comps.is_empty() {
                    self.project.circuit.components.keys().copied().collect()
                } else {
                    comps
                        .iter()
                        .map(|c| self.resolve_comp(c))
                        .collect::<Result<_, _>>()?
                };
                let board = self
                    .lib
                    .board(&self.project.circuit.board_id)
                    .ok_or("no board")?
                    .clone();
                let plan = wirelab_core::autowire::auto_wire(
                    &self.project.circuit,
                    &board,
                    &self.lib,
                    &ids,
                );
                let n = plan.wires.len();
                for (a, b) in plan.wires {
                    self.project.circuit.add_wire(a, b, [90, 210, 200]);
                }
                if n > 0 {
                    self.topo_rev += 1;
                }
                Ok(json!({ "wires_added": n, "notes": plan.notes }))
            }
            Cmd::FixLints => {
                let mut applied = Vec::new();
                for _ in 0..10 {
                    let Some(board) = self.lib.board(&self.project.circuit.board_id) else {
                        break;
                    };
                    let nl = Netlist::build(&self.project.circuit, board, &self.lib);
                    let lints = validate(&self.project.circuit, board, &self.lib, &nl);
                    let Some(lint) = lints.into_iter().find(|l| l.fix.is_some()) else {
                        break;
                    };
                    let fix = lint.fix.unwrap();
                    let LintFix::SpliceResistor { ohms, .. } = &fix;
                    applied.push(format!("{} → spliced ≈{ohms:.0} Ω", lint.message));
                    self.apply_lint_fix(&fix);
                }
                Ok(json!({ "applied": applied, "remaining": self.lints_json() }))
            }
            Cmd::SetScript { comp, source } => {
                let id = self.resolve_comp(&comp)?;
                let mut sorted: Vec<String> = names.values().cloned().collect();
                sorted.sort();
                self.linter.set_api(&sorted);
                let diags: Vec<Value> = self
                    .linter
                    .lint(&source)
                    .iter()
                    .map(|d| json!({"line": d.line, "col": d.col, "message": d.message}))
                    .collect();
                if let Some(c) = self.project.circuit.components.get_mut(&id) {
                    c.script = Some(source);
                }
                self.script_rev += 1;
                if !self.live.connected() {
                    self.live.scripts.sync(&self.project.circuit, &self.lib);
                }
                Ok(json!({
                    "applied_to": names.get(&id),
                    "diagnostics": diags,
                    "note": if self.live.connected() {
                        "live — runtime errors will appear in read_live console"
                    } else {
                        "compiled; connect_simulator to run it"
                    },
                }))
            }
            Cmd::GetScript { comp } => {
                let id = self.resolve_comp(&comp)?;
                let script = self
                    .project
                    .circuit
                    .components
                    .get(&id)
                    .and_then(|c| c.script.clone());
                Ok(json!({ "comp": names.get(&id), "script": script }))
            }
            Cmd::ConnectSimulator => {
                if self.live.connected() {
                    return Ok(json!({ "status": "already connected" }));
                }
                let lib = self.lib.clone();
                let circuit = self.project.circuit.clone();
                self.live.connect_sim(&lib, &circuit, &mut self.console);
                self.live.backend = crate::live::Backend::Simulator;
                Ok(json!({ "status": "simulator connected" }))
            }
            Cmd::Disconnect => {
                self.live.disconnect(&mut self.console);
                Ok(json!({ "status": "disconnected" }))
            }
            Cmd::SetComponentState { comp, pressed, on, value } => {
                let id = self.resolve_comp(&comp)?;
                let c = self
                    .project
                    .circuit
                    .components
                    .get_mut(&id)
                    .ok_or("component vanished")?;
                if let Some(p) = pressed {
                    c.state = CompState::Button { pressed: p };
                } else if let Some(o) = on {
                    c.state = CompState::Toggle { on: o };
                } else if let Some(v) = value {
                    c.state = CompState::Fraction { value: v.clamp(0.0, 1.0) };
                } else {
                    return Err("give one of: pressed, on, value".into());
                }
                self.state_rev += 1;
                Ok(json!({ "comp": names.get(&id), "state": format!("{:?}", self.project.circuit.components[&id].state) }))
            }
            Cmd::ReadLive => {
                if !self.live.connected() {
                    return Err("not connected — call connect_simulator first".into());
                }
                let out = self.live.live_output.clone();
                let visuals: Value = out
                    .as_ref()
                    .map(|o| {
                        Value::Object(
                            o.visuals
                                .iter()
                                .filter_map(|(id, v)| {
                                    Some((
                                        names.get(id)?.clone(),
                                        Value::String(format!("{v:?}")),
                                    ))
                                })
                                .collect(),
                        )
                    })
                    .unwrap_or(Value::Null);
                let high_pins: Vec<u8> =
                    (0..64).filter(|g| self.live.telemetry_levels & (1u64 << g) != 0).collect();
                Ok(json!({
                    "pins_high": high_pins,
                    "analog_mv": out.as_ref().map(|o| o.analog_mv.clone()),
                    "visuals": visuals,
                    "rgb": out.as_ref().and_then(|o| o.rgb),
                    "warnings": out.as_ref().map(|o| o.warnings.clone()),
                    "console_tail": self.console.iter().rev().take(12).rev().collect::<Vec<_>>(),
                }))
            }
            Cmd::OpenExample { name } => {
                let needle = name.to_lowercase();
                let found = self
                    .examples
                    .iter()
                    .find(|(n, _)| n.to_lowercase().contains(&needle))
                    .cloned();
                match found {
                    Some((n, path)) => {
                        self.load_project_path(&path, false);
                        Ok(json!({ "opened": n }))
                    }
                    None => Err(format!(
                        "no example matching '{name}'; have: {}",
                        self.examples.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(" · ")
                    )),
                }
            }
            Cmd::GetFlow => {
                let graph = serde_json::to_value(&self.project.flow).map_err(|e| e.to_string())?;
                let (code, errors) = match wirelab_core::flow::compile(&self.project.flow) {
                    Ok(code) => (Some(code), Vec::new()),
                    Err(errs) => (None, errs),
                };
                Ok(json!({
                    "nodes": graph["nodes"],
                    "wires": graph["wires"],
                    "generated_rhai": code,
                    "errors": errors.iter()
                        .map(|e| json!({"node": e.node, "msg": e.msg}))
                        .collect::<Vec<_>>(),
                }))
            }
            Cmd::SetFlow { nodes, wires } => {
                let mut graph: FlowGraph =
                    serde_json::from_value(json!({ "nodes": nodes, "wires": wires }))
                        .map_err(|e| format!("flow JSON does not match the schema: {e}"))?;
                auto_layout_new_nodes(&mut graph);
                match wirelab_core::flow::compile(&graph) {
                    // An empty graph clears the flow even if compile complains.
                    Err(errors) if !graph.nodes.is_empty() => Ok(json!({
                        "applied": false,
                        "errors": errors.iter()
                            .map(|e| json!({"node": e.node, "msg": e.msg}))
                            .collect::<Vec<_>>(),
                    })),
                    result => {
                        let code = result.ok();
                        let n = graph.nodes.len();
                        self.project.flow = graph;
                        self.flow_rev += 1;
                        Ok(json!({ "applied": true, "nodes": n, "generated_rhai": code }))
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wirelab_core::flow::{FlowNode, FlowWire, NodeKind};

    #[test]
    fn auto_layout_columns_by_depth() {
        let mut g = FlowGraph {
            nodes: vec![
                FlowNode { kind: NodeKind::OnUart, pos: [0.0, 0.0] },
                FlowNode { kind: NodeKind::UartSend, pos: [0.0, 0.0] },
                FlowNode { kind: NodeKind::Every { ms: 500.0 }, pos: [0.0, 0.0] },
                FlowNode { kind: NodeKind::Toggle, pos: [7.0, 9.0] },
            ],
            wires: vec![FlowWire { from: (0, 0), to: (1, 0) }],
        };
        auto_layout_new_nodes(&mut g);
        assert_eq!(g.nodes[0].pos, [40.0, 40.0]);
        assert_eq!(g.nodes[1].pos, [260.0, 40.0]);
        assert_eq!(g.nodes[2].pos, [40.0, 150.0]);
        assert_eq!(g.nodes[3].pos, [7.0, 9.0]);
    }

    #[test]
    fn flow_graph_json_round_trip_with_t_tag() {
        let v = json!({
            "nodes": [
                { "kind": { "t": "OnPress", "comp": "btn" }, "pos": [10.0, 20.0] },
                { "kind": { "t": "ToggleComp", "comp": "led" }, "pos": [200.0, 20.0] },
            ],
            "wires": [ { "from": [0, 0], "to": [1, 0] } ],
        });
        let g: FlowGraph = serde_json::from_value(v).unwrap();
        assert_eq!(g.nodes[0].kind, NodeKind::OnPress { comp: "btn".into() });
        assert_eq!(g.wires[0].to, (1, 0));
        let back = serde_json::to_value(&g).unwrap();
        assert_eq!(back["nodes"][1]["kind"]["t"], "ToggleComp");
        assert_eq!(back["wires"][0]["from"], json!([0, 0]));
    }
}
