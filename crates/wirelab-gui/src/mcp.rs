//! Embedded MCP server (rmcp, streamable HTTP): lets an AI inspect the live
//! circuit, place components, draw verdict-checked wires, run validation
//! with fixes, write scripts, edit the flow node-graph and poke the
//! simulator — against the running app. Tools marshal onto the GUI thread
//! through a channel; the UI drains it once per frame.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{ServerCapabilities, ServerConfig};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use serde_json::{Value, json};
use wirelab_core::circuit::{CompId, Endpoint, PlacedComponent, WireId};
use wirelab_core::component::CompState;
use wirelab_core::edit::{EditOp, apply_edit};
use wirelab_core::flow::FlowGraph;
use wirelab_core::netlist::{Netlist, WireVerdict, wire_verdict};
use wirelab_core::project::Project;
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
    AddWire { a: EndpointArg, b: EndpointArg, waypoints: Vec<[f32; 2]> },
    RemoveWire { id: u32 },
    AutoWire { comps: Vec<String> },
    FixLints,
    SetScript { comp: String, source: String },
    GetScript { comp: String },
    ConnectSimulator,
    Disconnect,
    SetComponentState { comp: String, pressed: Option<bool>, on: Option<bool>, value: Option<f32> },
    SetProp { comp: String, key: String, value: Option<f64> },
    ReadLive,
    OpenExample { name: String },
    GetFlow,
    SetFlow { nodes: Value, wires: Value },
    /// Full project snapshot + the board profiles / component defs it uses,
    /// served to remote viewers (the iPad plugin) over the LAN listener.
    GetProjectSnapshot,
    /// Remote flow edit: applies when `base` matches the board's current
    /// flow hash (a stale base means the desktop changed — 'conflict').
    SetBoardFlow { board_id: u64, base: u64, nodes: Value, wires: Value },
    /// Remote component drags; positions are last-writer-wins.
    MoveComponents { board_id: u64, moves: Vec<(u32, [f32; 2])> },
    /// Remote structural edit (add/remove component/wire) on any board.
    EditBoard { board_id: u64, op: EditOp },
    /// Remote script edit on any board: sets the component's script and lints it.
    SetBoardScript { board_id: u64, comp: u32, source: String },
    /// Whole project pushed from a remote editor; parked as a pending import
    /// until the desktop user accepts it. Never applied by the network alone.
    ImportProject { project: Project, from: Option<String> },
    AddBreadboard { profile: String, x: f32, y: f32, rotation: u16, label: Option<String> },
    MoveBreadboard { bb: u32, x: Option<f32>, y: Option<f32>, rotation: Option<u16> },
    PlaceBoard { x: Option<f32>, y: Option<f32>, rotation: Option<u16>, display_rotation: Option<u16> },
    RemoveBreadboard { bb: u32 },
    SeatComponent { comp: String, terminal: String, hole: String, bb: Option<u32>, rotation: Option<u16>, span: Option<f32> },
    InspectHole { hole: String, bb: Option<u32> },
}

/// Parse a remote structural-edit body: `{board_id, op, ...}`.
pub fn parse_edit_push(body: &str) -> Result<Cmd, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let board_id = v.get("board_id").and_then(Value::as_u64).ok_or("missing board_id")?;
    let op: EditOp = serde_json::from_value(v).map_err(|e| format!("bad edit: {e}"))?;
    Ok(Cmd::EditBoard { board_id, op })
}

/// Parse a remote script-edit body: `{board_id, comp, source}`.
pub fn parse_script_push(body: &str) -> Result<Cmd, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    Ok(Cmd::SetBoardScript {
        board_id: v.get("board_id").and_then(Value::as_u64).ok_or("missing board_id")?,
        comp: v
            .get("comp")
            .and_then(Value::as_u64)
            .map(|n| n as u32)
            .ok_or("missing comp")?,
        source: v.get("source").and_then(Value::as_str).ok_or("missing source")?.to_string(),
    })
}

/// Parse a remote flow-edit body: `{board_id, base, nodes, wires}`.
pub fn parse_flow_push(body: &str) -> Result<Cmd, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    Ok(Cmd::SetBoardFlow {
        board_id: v.get("board_id").and_then(Value::as_u64).ok_or("missing board_id")?,
        base: v.get("base").and_then(Value::as_u64).ok_or("missing base")?,
        nodes: v.get("nodes").cloned().ok_or("missing nodes")?,
        wires: v.get("wires").cloned().ok_or("missing wires")?,
    })
}

/// Parse a remote drag body: `{board_id, moves: [[comp, [x, y]], ...]}`.
pub fn parse_move_push(body: &str) -> Result<Cmd, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let board_id = v.get("board_id").and_then(Value::as_u64).ok_or("missing board_id")?;
    let moves: Vec<(u32, [f32; 2])> =
        serde_json::from_value(v.get("moves").cloned().ok_or("missing moves")?)
            .map_err(|e| format!("bad moves: {e}"))?;
    Ok(Cmd::MoveComponents { board_id, moves })
}

/// Parse a whole-project import body: the project-file shape
/// `{name, active, boards}`, plus an optional `from` naming the sender.
/// At least one board is required — an import must never wipe the desktop's
/// project with an empty one.
pub fn parse_import_push(body: &str) -> Result<Cmd, String> {
    let v: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if !v.get("name").is_some_and(Value::is_string) {
        return Err("missing name".into());
    }
    if !v.get("boards").and_then(Value::as_array).is_some_and(|b| !b.is_empty()) {
        return Err("missing boards".into());
    }
    let from = v.get("from").and_then(Value::as_str).map(str::to_string);
    let project = Project::from_json(body).map_err(|e| format!("bad project: {e}"))?;
    Ok(Cmd::ImportProject { project, from })
}

/// Auto-discovery beacon payload: `WIRELAB-HOST <view_port> <hostname>`.
pub fn host_beacon(port: u16, name: &str) -> String {
    format!("WIRELAB-HOST {port} {name}")
}

/// Content hash used as the optimistic-concurrency base for remote flow edits.
pub fn flow_hash(flow: &wirelab_core::flow::FlowGraph) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    serde_json::to_string(flow).unwrap_or_default().hash(&mut h);
    h.finish()
}

#[derive(Debug, Clone, serde::Deserialize, schemars::JsonSchema)]
pub struct EndpointArg {
    /// Board pin key, e.g. "GPIO2", "GND1", "3V3". Mutually exclusive with comp.
    pub pin: Option<String>,
    /// Component id (number) or script name / label, e.g. "red_led".
    pub comp: Option<String>,
    /// Terminal id on that component, e.g. "anode", "a", "signal".
    pub terminal: Option<String>,
    /// Breadboard hole by printed name, e.g. "E12" or "+A3". Mutually exclusive with pin/comp.
    pub hole: Option<String>,
    /// Breadboard id for `hole`; optional when only one breadboard is placed.
    pub bb: Option<u32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct AddBreadboardArgs {
    /// Breadboard profile id from list_library, e.g. "electrocookie-half".
    pub profile: String,
    /// Board centre on the canvas, mm.
    pub x: f32,
    pub y: f32,
    /// 0, 90, 180 or 270 (clockwise).
    pub rotation: Option<u16>,
    pub label: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct MoveBreadboardArgs {
    pub bb: u32,
    pub x: Option<f32>,
    pub y: Option<f32>,
    /// 0, 90, 180 or 270 (clockwise).
    pub rotation: Option<u16>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct SetPropArgs {
    /// Component id or label, e.g. "PS1".
    pub comp: String,
    /// Prop key from list_library, e.g. "volts", "i_max_a", "v_set".
    pub key: String,
    /// New value, clamped to the prop's range; omit to restore the default.
    pub value: Option<f64>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct PlaceBoardArgs {
    /// Top-left corner of the unrotated board, mm.
    pub x: Option<f32>,
    pub y: Option<f32>,
    /// 0, 90, 180 or 270 (clockwise) about the board's centre; 180 puts the USB end on top.
    pub rotation: Option<u16>,
    /// On-board display picture turn relative to the board: 0, 90, 180 or 270; 180 with a 180 board reads upright.
    pub display_rotation: Option<u16>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct BreadboardArg {
    pub bb: u32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct SeatArgs {
    /// Component id (number) or script name / label.
    pub comp: String,
    /// Terminal to put in `hole`, e.g. "a", "anode".
    pub terminal: String,
    /// Hole name, e.g. "E12".
    pub hole: String,
    /// Breadboard id; optional when only one breadboard is placed.
    pub bb: Option<u32>,
    /// Part rotation to apply first: 0, 90, 180 or 270.
    pub rotation: Option<u16>,
    /// Lead span in holes for bendable two-lead parts (resistors, diodes, caps).
    pub span: Option<f32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct HoleArgs {
    /// Hole name, e.g. "E12".
    pub hole: String,
    pub bb: Option<u32>,
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
    beacon_stop: Arc<AtomicBool>,
    beacon_thread: Option<std::thread::JoinHandle<()>>,
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
                    config.legacy_session_mode = false;
                    config.json_response = true;
                    let (view_tx, view_ctx) = (tx.clone(), ctx.clone());
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

                    // Project snapshot + remote-edit endpoints for LAN viewers
                    // (the iPad plugin) — unlike the localhost-only MCP.
                    let ask_ui = {
                        let tx = view_tx.clone();
                        let ctx = view_ctx.clone();
                        move |cmd: Cmd| {
                            let tx = tx.clone();
                            let ctx = ctx.clone();
                            async move {
                                let (reply, rx) = tokio::sync::oneshot::channel();
                                if tx.send(McpRequest { cmd, reply }).is_err() {
                                    return r#"{"ok":false,"error":"WireLab is shutting down"}"#
                                        .to_string();
                                }
                                ctx.request_repaint();
                                match tokio::time::timeout(
                                    std::time::Duration::from_secs(5),
                                    rx,
                                )
                                .await
                                {
                                    Ok(Ok(Ok(v))) => serde_json::to_string(&v).unwrap_or_default(),
                                    Ok(Ok(Err(e))) => serde_json::json!({
                                        "ok": false, "error": e
                                    })
                                    .to_string(),
                                    _ => r#"{"ok":false,"error":"the WireLab UI did not respond"}"#
                                        .to_string(),
                                }
                            }
                        }
                    };
                    let get_ask = ask_ui.clone();
                    let flow_ask = ask_ui.clone();
                    let edit_ask = ask_ui.clone();
                    let script_ask = ask_ui.clone();
                    let move_ask = ask_ui.clone();
                    let import_ask = ask_ui;
                    let view_router = axum::Router::new()
                        .route(
                            "/project",
                            axum::routing::get(move || get_ask(Cmd::GetProjectSnapshot)),
                        )
                        .route(
                            "/project/flow",
                            axum::routing::post(move |body: String| async move {
                                match parse_flow_push(&body) {
                                    Ok(cmd) => flow_ask(cmd).await,
                                    Err(e) => serde_json::json!({ "ok": false, "error": e })
                                        .to_string(),
                                }
                            }),
                        )
                        .route(
                            "/project/edit",
                            axum::routing::post(move |body: String| async move {
                                match parse_edit_push(&body) {
                                    Ok(cmd) => edit_ask(cmd).await,
                                    Err(e) => serde_json::json!({ "ok": false, "error": e })
                                        .to_string(),
                                }
                            }),
                        )
                        .route(
                            "/project/script",
                            axum::routing::post(move |body: String| async move {
                                match parse_script_push(&body) {
                                    Ok(cmd) => script_ask(cmd).await,
                                    Err(e) => serde_json::json!({ "ok": false, "error": e })
                                        .to_string(),
                                }
                            }),
                        )
                        .route(
                            "/project/positions",
                            axum::routing::post(move |body: String| async move {
                                match parse_move_push(&body) {
                                    Ok(cmd) => move_ask(cmd).await,
                                    Err(e) => serde_json::json!({ "ok": false, "error": e })
                                        .to_string(),
                                }
                            }),
                        )
                        // Queues the project for confirmation; the reply says
                        // "pending", never "applied".
                        .route(
                            "/project/import",
                            axum::routing::post(move |body: String| async move {
                                match parse_import_push(&body) {
                                    Ok(cmd) => import_ask(cmd).await,
                                    Err(e) => serde_json::json!({ "ok": false, "error": e })
                                        .to_string(),
                                }
                            }),
                        );
                    let view_port = port + 3;
                    if let Ok(l) =
                        tokio::net::TcpListener::bind(("0.0.0.0", view_port)).await
                    {
                        tokio::spawn(async move {
                            let _ = axum::serve(l, view_router).await;
                        });
                    }

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
            Ok(Ok(())) => {
                let view_port = port + 3;
                let beacon_stop = Arc::new(AtomicBool::new(false));
                let beacon_thread = spawn_beacon(view_port, beacon_stop.clone());
                Ok(McpServer {
                    rx,
                    addr: format!("http://127.0.0.1:{port}/mcp"),
                    shutdown: Some(stop_tx),
                    thread: Some(thread),
                    beacon_stop,
                    beacon_thread,
                })
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err("MCP server did not start in time".into()),
        }
    }
}

/// Broadcast a UDP discovery beacon every 2 s until `stop` is set.
fn spawn_beacon(
    view_port: u16,
    stop: Arc<AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.set_broadcast(true).ok()?;
    let name = std::env::var("HOSTNAME").unwrap_or_else(|_| "wirelab".to_string());
    let payload = host_beacon(view_port, &name);
    let targets = [
        std::net::SocketAddr::from(([255, 255, 255, 255], 4521)),
        std::net::SocketAddr::from(([127, 0, 0, 1], 4521)),
    ];
    Some(std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            for target in &targets {
                let _ = socket.send_to(payload.as_bytes(), target);
            }
            for _ in 0..20 {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }))
}

impl Drop for McpServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        self.beacon_stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if let Some(t) = self.beacon_thread.take() {
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
    /// Optional routing points in canvas mm; the wire runs through them with orthogonal legs.
    #[serde(default)]
    pub waypoints: Vec<[f32; 2]>,
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

    #[tool(description = "Wire two points together. Endpoints are a board pin, a component terminal, or a breadboard hole ({\"hole\": \"E12\"}). Electrically dangerous wires (rail shorts) are refused with the reason.")]
    async fn add_wire(&self, Parameters(a): Parameters<AddWireArgs>) -> String {
        self.ask(Cmd::AddWire { a: a.a, b: a.b, waypoints: a.waypoints }).await
    }

    #[tool(description = "Place a breadboard (profile ids in list_library) centred at x, y mm. Parts with leads snap into its holes; holes in one strip or rail share copper.")]
    async fn add_breadboard(&self, Parameters(a): Parameters<AddBreadboardArgs>) -> String {
        self.ask(Cmd::AddBreadboard { profile: a.profile, x: a.x, y: a.y, rotation: a.rotation.unwrap_or(0), label: a.label }).await
    }

    #[tool(description = "Move or rotate a breadboard; parts fully seated in it travel with it.")]
    async fn move_breadboard(&self, Parameters(a): Parameters<MoveBreadboardArgs>) -> String {
        self.ask(Cmd::MoveBreadboard { bb: a.bb, x: a.x, y: a.y, rotation: a.rotation }).await
    }

    #[tool(description = "Move or rotate the dev board, or turn its on-board display picture (display_rotation). Its header pins seat into breadboard holes they land on.")]
    async fn place_board(&self, Parameters(a): Parameters<PlaceBoardArgs>) -> String {
        self.ask(Cmd::PlaceBoard { x: a.x, y: a.y, rotation: a.rotation, display_rotation: a.display_rotation }).await
    }

    #[tool(description = "Delete a breadboard and the wires landing on its holes.")]
    async fn remove_breadboard(&self, Parameters(a): Parameters<BreadboardArg>) -> String {
        self.ask(Cmd::RemoveBreadboard { bb: a.bb }).await
    }

    #[tool(description = "Insert a part into a breadboard: moves it so `terminal` lands in `hole` (after the optional rotation and lead span). Returns where every lead ended up; leads that miss a hole are listed.")]
    async fn seat_component(&self, Parameters(a): Parameters<SeatArgs>) -> String {
        self.ask(Cmd::SeatComponent { comp: a.comp, terminal: a.terminal, hole: a.hole, bb: a.bb, rotation: a.rotation, span: a.span }).await
    }

    #[tool(description = "What a breadboard hole connects to: its strip or rail, every hole on that copper, and the part leads, pins and wires on the same net.")]
    async fn inspect_hole(&self, Parameters(a): Parameters<HoleArgs>) -> String {
        self.ask(Cmd::InspectHole { hole: a.hole, bb: a.bb }).await
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

    #[tool(description = "Set a component value prop (supply volts / current rating, boost set point, resistance, ...). The simulator picks it up live.")]
    async fn set_prop(&self, Parameters(a): Parameters<SetPropArgs>) -> String {
        self.ask(Cmd::SetProp { comp: a.comp, key: a.key, value: a.value }).await
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
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
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

    fn resolve_bb(&self, bb: Option<u32>) -> Result<wirelab_core::circuit::BbId, String> {
        let boards = &self.project.circuit.breadboards;
        match bb {
            Some(n) => {
                let id = wirelab_core::circuit::BbId(n);
                boards.contains_key(&id).then_some(id).ok_or_else(|| format!("no breadboard {n}"))
            }
            None if boards.len() == 1 => Ok(*boards.keys().next().unwrap()),
            None if boards.is_empty() => Err("no breadboard placed (add_breadboard first)".into()),
            None => Err("several breadboards placed: pass bb".into()),
        }
    }

    fn resolve_hole(&self, bb: Option<u32>, hole: &str) -> Result<(wirelab_core::circuit::BbId, wirelab_core::breadboard::HoleRef, String), String> {
        let id = self.resolve_bb(bb)?;
        let profile = self
            .lib
            .breadboard(&self.project.circuit.breadboards[&id].profile)
            .ok_or("breadboard profile missing from the library")?;
        let h = profile.parse_hole(hole).ok_or_else(|| {
            let lines: Vec<String> = profile.lines.iter().map(|l| format!("{}{}-{}", l.name, l.first, l.first + l.count - 1)).collect();
            format!("no hole '{hole}' on {} (lines: {})", profile.name, lines.join(", "))
        })?;
        Ok((id, h, profile.hole_name(h)))
    }

    fn seats_json(&self, only: Option<CompId>) -> (Vec<Value>, Vec<Value>) {
        let board = self.lib.board(&self.project.circuit.board_id);
        let seating = wirelab_core::breadboard::seating(&self.project.circuit, board, &self.lib);
        let names = wirelab_core::script::component_names(&self.project.circuit, &self.lib);
        let keep = |ep: &Endpoint| match (only, ep) {
            (None, _) => true,
            (Some(id), Endpoint::Terminal { comp, .. }) => *comp == id,
            _ => false,
        };
        let seats = seating
            .seats
            .iter()
            .filter(|s| keep(&s.ep))
            .map(|s| json!({ "lead": self.describe_endpoint(&s.ep, &names), "bb": s.bb.0, "hole": s.hole_name }))
            .collect();
        let misses = seating
            .misses
            .iter()
            .filter(|s| keep(&s.ep))
            .map(|s| json!({ "lead": self.describe_endpoint(&s.ep, &names), "bb": s.bb.0, "at": s.world }))
            .collect();
        (seats, misses)
    }

    fn resolve_endpoint(&self, arg: &EndpointArg) -> Result<Endpoint, String> {
        if let Some(hole) = &arg.hole {
            if arg.pin.is_some() || arg.comp.is_some() {
                return Err("endpoint needs exactly one of pin, comp+terminal or hole".into());
            }
            let (bb, _, name) = self.resolve_hole(arg.bb, hole)?;
            return Ok(Endpoint::Hole { bb, hole: name });
        }
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
            Endpoint::Hole { bb, hole } => json!({ "bb": bb.0, "hole": hole }),
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
                let breadboards: Vec<Value> = self
                    .project
                    .circuit
                    .breadboards
                    .values()
                    .map(|b| json!({ "id": b.id.0, "profile": b.profile, "pos": b.pos, "rotation": b.rotation, "label": b.label }))
                    .collect();
                let (seats, misses) = self.seats_json(None);
                Ok(json!({
                    "project": self.project.name,
                    "board": self.project.circuit.board_id,
                    "board_rotation": self.project.circuit.board_rotation,
                    "display_rotation": self.project.circuit.display_rotation,
                    "breadboards": breadboards,
                    "seated_leads": seats,
                    "leads_missing_holes": misses,
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
                let breadboards: Vec<Value> = self
                    .lib
                    .breadboards
                    .values()
                    .map(|p| {
                        json!({
                            "id": p.id,
                            "name": p.name,
                            "size_mm": [p.width_mm, p.height_mm],
                            "lines": p.lines.iter().map(|l| json!({
                                "name": l.name, "holes": format!("{}{}-{}{}", l.name, l.first, l.name, l.first + l.count - 1),
                                "bus": l.bus, "polarity": l.polarity,
                            })).collect::<Vec<_>>(),
                            "notes": p.notes,
                        })
                    })
                    .collect();
                let leaded: Vec<&str> =
                    self.lib.components.values().filter(|d| !d.leads.is_empty()).map(|d| d.id.as_str()).collect();
                Ok(json!({ "components": comps, "boards": boards, "breadboards": breadboards, "breadboard_insertable": leaded }))
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
            Cmd::AddWire { a, b, waypoints } => {
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
                        if let Some(w) = self.project.circuit.wires.get_mut(&id) {
                            w.waypoints = waypoints;
                        }
                        self.topo_rev += 1;
                        Ok(json!({ "wire": id.0 }))
                    }
                }
            }
            Cmd::AddBreadboard { profile, x, y, rotation, label } => {
                let id = self.add_breadboard_at(&profile, [x, y]).ok_or_else(|| format!("unknown breadboard profile '{profile}' (see list_library)"))?;
                if let Some(b) = self.project.circuit.breadboards.get_mut(&id) {
                    b.pos = [x, y];
                    b.rotation = rotation % 360 / 90 * 90;
                    b.label = label.unwrap_or_default();
                }
                self.topo_rev += 1;
                Ok(json!({ "bb": id.0 }))
            }
            Cmd::PlaceBoard { x, y, rotation, display_rotation } => {
                let c = &mut self.project.circuit;
                c.board_pos = [x.unwrap_or(c.board_pos[0]), y.unwrap_or(c.board_pos[1])];
                if let Some(rot) = rotation {
                    c.board_rotation = rot % 360 / 90 * 90;
                }
                if let Some(rot) = display_rotation {
                    c.display_rotation = rot % 360 / 90 * 90;
                }
                let (pos, rot, disp) = (c.board_pos, c.board_rotation, c.display_rotation);
                self.topo_rev += 1;
                Ok(json!({ "board_pos": pos, "rotation": rot, "display_rotation": disp }))
            }
            Cmd::MoveBreadboard { bb, x, y, rotation } => {
                let id = self.resolve_bb(Some(bb))?;
                let carried = self.parts_seated_in(id);
                if let Some(rot) = rotation {
                    let target = rot % 360 / 90 * 90;
                    for _ in 0..4 {
                        if self.project.circuit.breadboards[&id].rotation == target {
                            break;
                        }
                        self.rotate_breadboard(id);
                    }
                }
                let b = self.project.circuit.breadboards.get_mut(&id).ok_or("breadboard vanished")?;
                let to = [x.unwrap_or(b.pos[0]), y.unwrap_or(b.pos[1])];
                let delta = [to[0] - b.pos[0], to[1] - b.pos[1]];
                b.pos = to;
                for cid in carried {
                    if let Some(c) = self.project.circuit.components.get_mut(&cid) {
                        c.pos = [c.pos[0] + delta[0], c.pos[1] + delta[1]];
                    }
                }
                self.topo_rev += 1;
                Ok(json!({ "bb": bb, "pos": to, "rotation": self.project.circuit.breadboards[&id].rotation }))
            }
            Cmd::RemoveBreadboard { bb } => {
                let id = self.resolve_bb(Some(bb))?;
                self.project.circuit.remove_breadboard(id);
                self.topo_rev += 1;
                Ok(json!({ "removed": bb }))
            }
            Cmd::SeatComponent { comp, terminal, hole, bb, rotation, span } => {
                let id = self.resolve_comp(&comp)?;
                let (bb_id, _, hole_name) = self.resolve_hole(bb, &hole)?;
                let target = wirelab_core::breadboard::hole_endpoint_world(&self.project.circuit, &self.lib, bb_id, &hole_name)
                    .ok_or("hole position unavailable")?;
                let c = self.project.circuit.components.get(&id).ok_or("component vanished")?;
                let def = self.lib.component(&c.def_id).ok_or("component definition missing")?.clone();
                if def.leads.is_empty() {
                    return Err(format!("{} has no through-hole leads; wire it to the holes instead", def.name));
                }
                let idx = def
                    .terminals
                    .iter()
                    .position(|t| t.id.eq_ignore_ascii_case(&terminal))
                    .ok_or_else(|| format!("'{terminal}' is not a terminal of {}", def.name))?;
                let c = self.project.circuit.components.get_mut(&id).ok_or("component vanished")?;
                if let Some(r) = rotation {
                    c.rotation = r % 360 / 90 * 90;
                }
                if let Some(s) = span {
                    if def.leads.len() != 2 {
                        return Err("span applies to two-lead parts only".into());
                    }
                    c.props.insert(wirelab_core::geometry::SPAN_PROP.into(), f64::from(s.round().max(1.0)));
                }
                let t = wirelab_core::geometry::terminal_world_pos(c, &def, idx);
                c.pos = [c.pos[0] + target[0] - t[0], c.pos[1] + target[1] - t[1]];
                self.topo_rev += 1;
                let (seats, misses) = self.seats_json(Some(id));
                Ok(json!({ "comp": id.0, "pos": self.project.circuit.components[&id].pos, "seated": seats, "missing_holes": misses }))
            }
            Cmd::InspectHole { hole, bb } => {
                let (bb_id, h, name) = self.resolve_hole(bb, &hole)?;
                let profile = self.lib.breadboard(&self.project.circuit.breadboards[&bb_id].profile).ok_or("profile missing")?.clone();
                let board = self.lib.board(&self.project.circuit.board_id).ok_or("no board")?;
                let nl = Netlist::build(&self.project.circuit, board, &self.lib);
                let names = wirelab_core::script::component_names(&self.project.circuit, &self.lib);
                let node = profile.node_of(h);
                let on_node = |e: &Endpoint| matches!(e, Endpoint::Hole { bb, hole } if *bb == bb_id && profile.parse_hole(hole).is_some_and(|x| profile.node_of(x) == node));
                let net = nl.nets.iter().find(|n| n.endpoints.iter().any(on_node));
                let members: Vec<Value> = net
                    .map(|n| n.endpoints.iter().filter(|e| !matches!(e, Endpoint::Hole { .. })).map(|e| self.describe_endpoint(e, &names)).collect())
                    .unwrap_or_default();
                let net_name = net.and_then(|n| {
                    self.project.circuit.net_names.iter().find(|(_, ep)| nl.net_of(ep) == Some(n.id)).map(|(k, _)| k.clone())
                });
                Ok(json!({
                    "hole": name,
                    "copper": profile.node_label(h),
                    "holes_on_copper": profile.bus_holes(h).into_iter().map(|o| profile.hole_name(o)).collect::<Vec<_>>(),
                    "net": net_name,
                    "connected": members,
                }))
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
                let diags: Vec<Value> = self
                    .docs
                    .diagnose_source(&source, &self.project.circuit, &self.lib, &self.live.scripts, Some(id))
                    .iter()
                    .map(diag_json)
                    .collect();
                let editor_in_sync = !self.docs.is_dirty(id, &self.project.circuit);
                if editor_in_sync {
                    self.docs.set_buffer(id, &self.project.circuit, source.clone());
                }
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
            Cmd::SetProp { comp, key, value } => {
                let id = self.resolve_comp(&comp)?;
                let def_id = self.project.circuit.components.get(&id).ok_or("no such component")?.def_id.clone();
                let def = self.lib.component(&def_id).ok_or("unknown part")?;
                let Some(spec) = def.props.iter().find(|p| p.key == key) else {
                    let keys: Vec<&str> = def.props.iter().map(|p| p.key.as_str()).collect();
                    return Err(format!("{def_id} has no prop '{key}'; it has {keys:?}"));
                };
                let (min, max, default) = (spec.min, spec.max, spec.default);
                let c = self.project.circuit.components.get_mut(&id).ok_or("no such component")?;
                let v = match value {
                    Some(v) => {
                        let v = v.clamp(min, max);
                        c.props.insert(key.clone(), v);
                        v
                    }
                    None => {
                        c.props.remove(&key);
                        default
                    }
                };
                self.topo_rev += 1;
                Ok(json!({ "comp": id.0, "key": key, "value": v, "range": [min, max] }))
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
            Cmd::GetProjectSnapshot => {
                self.project.sync_active();
                let mut profiles = serde_json::Map::new();
                let mut defs = serde_json::Map::new();
                for tab in &self.project.boards {
                    if let Some(b) = self.lib.board(&tab.circuit.board_id)
                        && !profiles.contains_key(&tab.circuit.board_id)
                        && let Ok(v) = serde_json::to_value(b)
                    {
                        profiles.insert(tab.circuit.board_id.clone(), v);
                    }
                }
                // The full library rides along: it's the iPad's palette.
                for (id, d) in &self.lib.components {
                    if let Ok(v) = serde_json::to_value(d) {
                        defs.insert(id.clone(), v);
                    }
                }
                let flow_bases: serde_json::Map<String, Value> = self
                    .project
                    .boards
                    .iter()
                    .map(|b| (b.id.to_string(), Value::from(flow_hash(&b.flow))))
                    .collect();
                Ok(serde_json::json!({
                    "name": self.project.name,
                    "active": self.project.active,
                    "boards": self.project.boards,
                    "profiles": profiles,
                    "defs": defs,
                    "flow_bases": flow_bases,
                }))
            }
            Cmd::SetBoardFlow { board_id, base, nodes, wires } => {
                self.project.sync_active();
                let Some(idx) = self.project.boards.iter().position(|b| b.id == board_id)
                else {
                    return Err("no such board".into());
                };
                if flow_hash(&self.project.boards[idx].flow) != base {
                    return Err("conflict: the flow changed on the desktop".into());
                }
                let graph: wirelab_core::flow::FlowGraph =
                    serde_json::from_value(serde_json::json!({ "nodes": nodes, "wires": wires }))
                        .map_err(|e| format!("bad flow: {e}"))?;
                let new_base = flow_hash(&graph);
                self.project.boards[idx].flow = graph.clone();
                if idx == self.project.active {
                    self.project.flow = graph;
                    self.flow_rev += 1;
                } else if let Some(bb) = self.background.get_mut(&board_id) {
                    // A live parked board picks the edit up immediately.
                    bb.set_flow(&graph);
                }
                self.console.push(format!(
                    "[{}] flow updated from a remote editor",
                    self.project.boards[idx].name
                ));
                Ok(serde_json::json!({ "ok": true, "base": new_base }))
            }
            Cmd::EditBoard { board_id, op } => {
                self.project.sync_active();
                let Some(idx) = self.project.boards.iter().position(|b| b.id == board_id)
                else {
                    return Err("no such board".into());
                };
                let name = self.project.boards[idx].name.clone();
                let result = if idx == self.project.active {
                    let outs: Vec<u8> =
                        self.cache.bindings.outputs.values().map(|o| o.gpio).collect();
                    // Working copy is authoritative for the active board; it
                    // folds into boards[active] at the next sync_active.
                    let lib = self.lib.clone();
                    let r = apply_edit(&mut self.project.circuit, &lib, &outs, op)?;
                    self.topo_rev += 1;
                    self.state_rev += 1;
                    r
                } else {
                    let outs: Vec<u8> = self
                        .background
                        .get(&board_id)
                        .map(|bb| bb.output_gpios())
                        .unwrap_or_default();
                    let lib = self.lib.clone();
                    let circuit = &mut self.project.boards[idx].circuit;
                    let r = apply_edit(circuit, &lib, &outs, op)?;
                    // A live parked board re-plans its pin setup immediately.
                    let circuit = self.project.boards[idx].circuit.clone();
                    if let Some(bb) = self.background.get_mut(&board_id) {
                        bb.set_topo(&circuit, &self.lib);
                    }
                    r
                };
                self.console.push(format!("[{name}] circuit edited from a remote editor"));
                Ok(result)
            }
            Cmd::SetBoardScript { board_id, comp, source } => {
                self.project.sync_active();
                let Some(idx) = self.project.boards.iter().position(|b| b.id == board_id)
                else {
                    return Err("no such board".into());
                };
                let is_active = idx == self.project.active;
                let id = CompId(comp);
                let circuit = if is_active {
                    &mut self.project.circuit
                } else {
                    &mut self.project.boards[idx].circuit
                };
                if !circuit.components.contains_key(&id) {
                    return Err("no such component".into());
                }
                let script = if source.trim().is_empty() { None } else { Some(source.clone()) };
                circuit.components.get_mut(&id).unwrap().script = script;
                if is_active {
                    self.script_rev += 1;
                } else if let Some(bb) = self.background.get_mut(&board_id) {
                    bb.bump_script();
                }
                let circuit = if is_active {
                    &self.project.circuit
                } else {
                    &self.project.boards[idx].circuit
                };
                let names = wirelab_core::script::component_names(circuit, &self.lib);
                let comp_name =
                    names.get(&id).cloned().unwrap_or_else(|| id.0.to_string());
                let circuit = circuit.clone();
                let diags: Vec<Value> = if source.trim().is_empty() {
                    Vec::new()
                } else {
                    self.docs
                        .diagnose_source(&source, &circuit, &self.lib, &self.live.scripts, Some(id))
                        .iter()
                        .map(diag_json)
                        .collect()
                };
                let compile_error = if source.trim().is_empty() {
                    None
                } else {
                    rhai::Engine::new().compile(&source).err().map(|e| e.to_string())
                };
                let board_name = self.project.boards[idx].name.clone();
                self.console
                    .push(format!("[{board_name}] script for {comp_name} edited remotely"));
                Ok(json!({ "ok": true, "diagnostics": diags, "compile_error": compile_error }))
            }
            Cmd::MoveComponents { board_id, moves } => {
                self.project.sync_active();
                let Some(idx) = self.project.boards.iter().position(|b| b.id == board_id)
                else {
                    return Err("no such board".into());
                };
                let mut n = 0;
                for (comp, pos) in moves {
                    let id = wirelab_core::circuit::CompId(comp);
                    if idx == self.project.active
                        && let Some(c) = self.project.circuit.components.get_mut(&id)
                    {
                        c.pos = pos;
                        n += 1;
                    }
                    if let Some(c) = self.project.boards[idx].circuit.components.get_mut(&id) {
                        c.pos = pos;
                        if idx != self.project.active {
                            n += 1;
                        }
                    }
                }
                Ok(serde_json::json!({ "ok": true, "moved": n }))
            }
            Cmd::ImportProject { project, from } => {
                let name = project.name.clone();
                let boards = project.boards.len();
                let who = from.clone().unwrap_or_else(|| "a remote editor".to_string());
                // Parked only — the confirm modal is what replaces the project.
                if let Err(e) = self.import.park(project, from) {
                    self.console.push(format!(
                        "refused '{name}' from {who} — an earlier import is still on screen"
                    ));
                    return Err(e);
                }
                self.console.push(format!(
                    "{who} sent the project '{name}' — accept or decline it in WireLab"
                ));
                Ok(serde_json::json!({
                    "ok": true, "pending": true, "name": name, "boards": boards,
                }))
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

/// A script diagnostic as MCP JSON.
fn diag_json(d: &crate::rhai_lint::LintDiag) -> Value {
    let severity = match d.severity {
        crate::rhai_lint::Severity::Error => "error",
        crate::rhai_lint::Severity::Warning => "warning",
    };
    json!({"line": d.line, "col": d.col, "severity": severity, "message": d.message})
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
    fn push_bodies_parse_and_hash_is_stable() {
        let cmd = parse_flow_push(
            r#"{"board_id":2,"base":7,"nodes":[{"kind":{"t":"Toggle"},"pos":[1.0,2.0]}],"wires":[]}"#,
        )
        .unwrap();
        assert!(matches!(cmd, Cmd::SetBoardFlow { board_id: 2, base: 7, .. }));
        let cmd = parse_move_push(r#"{"board_id":1,"moves":[[3,[10.0,20.0]]]}"#).unwrap();
        match cmd {
            Cmd::MoveComponents { board_id, moves } => {
                assert_eq!(board_id, 1);
                assert_eq!(moves, vec![(3, [10.0, 20.0])]);
            }
            _ => panic!(),
        }
        assert!(parse_flow_push("{}").is_err());

        let mut g = wirelab_core::flow::FlowGraph::default();
        let h0 = flow_hash(&g);
        assert_eq!(h0, flow_hash(&g), "hash is deterministic");
        g.nodes.push(wirelab_core::flow::FlowNode {
            kind: wirelab_core::flow::NodeKind::Toggle,
            pos: [0.0, 0.0],
        });
        assert_ne!(h0, flow_hash(&g), "hash tracks content");
    }

    #[test]
    fn edit_push_parses_and_applies_with_wire_safety() {
        use wirelab_core::circuit::Circuit;
        use wirelab_core::library::Library;

        // Parsing: each op shape round-trips into the right Cmd.
        let cmd = parse_edit_push(
            r#"{"board_id":3,"op":"add_comp","def_id":"led-red","pos":[40.0,20.0]}"#,
        )
        .unwrap();
        assert!(matches!(
            &cmd,
            Cmd::EditBoard { board_id: 3, op: EditOp::AddComp { def_id, .. } } if def_id == "led-red"
        ));
        assert!(parse_edit_push(r#"{"op":"add_comp","def_id":"x","pos":[0,0]}"#).is_err());

        // Applying against a real board, with the wire-safety verdict.
        let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        let lib = Library::load(&assets.join("boards"), &assets.join("components")).unwrap();
        let mut circuit = Circuit::new("esp32-c5-devkitc-1");

        let add = parse_edit_push(
            r#"{"board_id":1,"op":"add_comp","def_id":"led-red","pos":[40.0,20.0]}"#,
        )
        .unwrap();
        let Cmd::EditBoard { op, .. } = add else { panic!() };
        let r = apply_edit(&mut circuit, &lib, &[], op).unwrap();
        let comp = r["comp"].as_u64().unwrap() as u32;
        assert_eq!(circuit.components.len(), 1);

        // A legal wire (GPIO -> LED anode) is accepted.
        let anode = serde_json::json!({ "at": "terminal", "comp": comp, "terminal": "anode" });
        let gpio2 = serde_json::json!({ "at": "board_pin", "key": "GPIO2" });
        let body = serde_json::json!({ "board_id": 1, "op": "add_wire", "a": gpio2, "b": anode });
        let Cmd::EditBoard { op, .. } = parse_edit_push(&body.to_string()).unwrap() else {
            panic!()
        };
        assert!(apply_edit(&mut circuit, &lib, &[], op).unwrap()["ok"].as_bool().unwrap());

        // A rail short (5V -> GND) is refused by the same verdict the desktop uses.
        let short = serde_json::json!({
            "board_id": 1, "op": "add_wire",
            "a": { "at": "board_pin", "key": "5V" },
            "b": { "at": "board_pin", "key": "GND1" },
        });
        let Cmd::EditBoard { op, .. } = parse_edit_push(&short.to_string()).unwrap() else {
            panic!()
        };
        assert!(apply_edit(&mut circuit, &lib, &[], op).is_err());

        // A wire to a pin that doesn't exist is rejected, not silently added.
        let phantom = serde_json::json!({
            "board_id": 1, "op": "add_wire",
            "a": { "at": "board_pin", "key": "NO_SUCH_PIN" },
            "b": { "at": "board_pin", "key": "GND1" },
        });
        let Cmd::EditBoard { op, .. } = parse_edit_push(&phantom.to_string()).unwrap() else {
            panic!()
        };
        assert!(apply_edit(&mut circuit, &lib, &[], op).is_err());

        // Removing the component drops it.
        let rm = parse_edit_push(&format!(r#"{{"board_id":1,"op":"remove_comp","comp":{comp}}}"#))
            .unwrap();
        let Cmd::EditBoard { op, .. } = rm else { panic!() };
        apply_edit(&mut circuit, &lib, &[], op).unwrap();
        assert!(circuit.components.is_empty());
    }

    #[test]
    fn script_push_parses_and_rejects_missing_fields() {
        let cmd =
            parse_script_push(r#"{"board_id":1,"comp":2,"source":"fn on_start(){}"}"#).unwrap();
        match cmd {
            Cmd::SetBoardScript { board_id, comp, source } => {
                assert_eq!(board_id, 1);
                assert_eq!(comp, 2);
                assert_eq!(source, "fn on_start(){}");
            }
            _ => panic!(),
        }
        assert!(parse_script_push(r#"{"comp":2,"source":"x"}"#).is_err());
        assert!(parse_script_push(r#"{"board_id":1,"source":"x"}"#).is_err());
        assert!(parse_script_push(r#"{"board_id":1,"comp":2}"#).is_err());
    }

    #[test]
    fn import_push_needs_a_named_project_with_boards() {
        let board = serde_json::json!({
            "id": 4, "name": "Board 1",
            "circuit": { "board_id": "esp32-c5-devkitc-1", "board_pos": [0.0, 0.0],
                "components": {}, "wires": {}, "next_comp": 1, "next_wire": 1 },
        });
        let body =
            serde_json::json!({ "name": "phone lab", "active": 0, "boards": [board.clone()],
                "from": "Logan's phone" });
        match parse_import_push(&body.to_string()).unwrap() {
            Cmd::ImportProject { project, from } => {
                assert_eq!(project.name, "phone lab");
                assert_eq!(project.boards.len(), 1);
                assert_eq!(project.circuit.board_id, "esp32-c5-devkitc-1");
                assert_eq!(from.as_deref(), Some("Logan's phone"));
            }
            _ => panic!(),
        }
        // A blank or boardless body must never stand in for a project.
        assert!(parse_import_push(r#"{"name":"empty","boards":[]}"#).is_err());
        assert!(parse_import_push(r#"{"name":"empty"}"#).is_err());
        assert!(
            parse_import_push(&serde_json::json!({ "boards": [board] }).to_string()).is_err()
        );
        assert!(parse_import_push("not json").is_err());
    }

    #[test]
    fn host_beacon_format() {
        assert_eq!(host_beacon(4530, "desk"), "WIRELAB-HOST 4530 desk");
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
