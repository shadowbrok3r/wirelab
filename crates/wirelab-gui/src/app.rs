//! Top-level application state and frame orchestration.

use std::path::PathBuf;

use egui::{Color32, RichText};
use wirelab_core::circuit::{CompId, WireId};
use wirelab_core::engine::{Bindings, plan_setup};
use wirelab_core::library::Library;
use wirelab_core::netlist::Netlist;
use wirelab_core::project::Project;
use wirelab_core::validate::{Lint, validate};

use crate::ai_import::{AiImportState, save_profile};
use crate::canvas::CanvasState;
use crate::live::LiveState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    None,
    Comp(CompId),
    /// Rubber-band multi-selection.
    Comps(Vec<CompId>),
    Wire(WireId),
    Pin(String),
}

impl Selection {
    pub fn comp_ids(&self) -> Vec<CompId> {
        match self {
            Selection::Comp(id) => vec![*id],
            Selection::Comps(v) => v.clone(),
            _ => Vec::new(),
        }
    }

    pub fn contains_comp(&self, id: CompId) -> bool {
        match self {
            Selection::Comp(c) => *c == id,
            Selection::Comps(v) => v.contains(&id),
            _ => false,
        }
    }
}

/// Per-component script editor state; unsaved buffers survive selection moves.
#[derive(Default)]
pub struct ScriptEditor {
    pub comp: Option<CompId>,
    pub buffer: String,
    pub stash: std::collections::HashMap<CompId, String>,
    /// Live diagnostics for the current buffer.
    pub lint: Vec<crate::rhai_lint::LintDiag>,
    pub lint_hash: u64,
    pub completion: Option<Completion>,
    /// (cursor char index, buffer hash) the popup was built for; rebuilding
    /// only on change keeps the arrow-key selection stable.
    pub completion_key: (usize, u64),
}

/// The script IDE: its own native window, VSCode-shaped.
#[derive(Default)]
pub struct IdeState {
    /// The IDE viewport is shown.
    pub open: bool,
    pub tabs: Vec<IdeTab>,
    pub active: usize,
    pub find_open: bool,
    pub find: String,
    pub replace: String,
    /// Bottom strip (problems / console / guide), collapsible.
    pub bottom_open: bool,
    pub bottom_tab: IdeBottomTab,
    /// One-shot requests from side panels, consumed by the editor.
    pub pending_jump: Option<usize>,
    pub pending_find: Option<FindKind>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FindKind {
    Next,
    Replace,
    All,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum IdeBottomTab {
    #[default]
    Problems,
    Console,
    Guide,
}

#[derive(Clone, PartialEq)]
pub enum IdeTab {
    Comp(CompId),
    /// Static capability / roadmap sheets, keyed.
    Info(&'static str),
    /// The node-graph editor.
    Flow,
}

/// An open completion popup in the script editor.
pub struct Completion {
    /// (label, insert text, cursor chars back from end, detail).
    pub items: Vec<(String, String, usize, String)>,
    pub selected: usize,
    /// Char index where the word being completed starts.
    pub word_start: usize,
}

#[derive(Default)]
pub struct ModelCache {
    pub rev: u64,
    pub netlist: Netlist,
    pub bindings: Bindings,
    pub lints: Vec<Lint>,
}

/// A parked (non-active) board's live session plus everything its
/// background tick needs, frozen at stash time.
pub struct BackgroundBoard {
    pub live: LiveState,
    netlist: Netlist,
    bindings: Bindings,
    flow_code: Option<String>,
    /// (topo, state, script, flow) revisions the session was synced at.
    revs: (u64, u64, u64, u64),
}

pub struct WireLabApp {
    pub lib: Library,
    pub boards_dir: PathBuf,
    pub project: Project,
    pub project_path: Option<PathBuf>,
    pub selection: Selection,
    pub canvas: CanvasState,
    pub live: LiveState,
    /// Sessions of non-active board tabs, still ticking, keyed by board id.
    pub background: std::collections::HashMap<u64, BackgroundBoard>,
    pub ai: AiImportState,
    pub console: Vec<String>,
    /// Bumped on structural edits (components/wires/board).
    pub topo_rev: u64,
    /// Bumped on runtime state edits (button pressed, pot moved...).
    pub state_rev: u64,
    /// Bumped when any component script changes.
    pub script_rev: u64,
    /// Bumped when the flow graph changes.
    pub flow_rev: u64,
    pub flow_view: crate::flow_ui::FlowView,
    /// Compiled flow: (rev it was built from, Rhai source, compile errors).
    pub flow_cache: (u64, Option<String>, Vec<wirelab_core::flow::FlowError>),
    /// Floating read-only window with the generated flow script.
    pub flow_code_open: bool,
    /// Inline board-tab rename in progress: (tab index, edit buffer).
    pub board_rename: Option<(usize, String)>,
    pub script_ed: ScriptEditor,
    pub linter: crate::rhai_lint::Linter,
    pub ide: IdeState,
    pub docs_open: bool,
    pub docs_filter: String,
    /// Shipped example projects: (display name, path).
    pub examples: Vec<(String, PathBuf)>,
    pub flash: crate::flash::FlashState,
    /// Embedded MCP server for AI circuit assistance.
    pub mcp: Option<crate::mcp::McpServer>,
    /// Components to spotlight this frame (hovered warning in Checks).
    pub hover_highlight: Vec<CompId>,
    pub wiring_open: bool,
    pub wiring_filter: String,
    pub cache: ModelCache,
}

fn find_assets_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("WIRELAB_ASSETS") {
        return PathBuf::from(dir);
    }
    let candidates = [
        PathBuf::from("assets"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets"),
    ];
    for c in candidates {
        if c.join("boards").exists() {
            return c;
        }
    }
    PathBuf::from("assets")
}

/// Load a serialized `egui::Style` (e.g. an exported color scheme).
///
/// The file is deep-merged over `Style::default()` so exports from slightly
/// older or newer egui versions still apply.
fn load_theme(path: &std::path::Path) -> Result<egui::Style, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let user: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mut base = serde_json::to_value(egui::Style::default()).map_err(|e| e.to_string())?;
    merge_json(&mut base, user);
    serde_json::from_value(base).map_err(|e| e.to_string())
}

fn merge_json(base: &mut serde_json::Value, over: serde_json::Value) {
    match (base, over) {
        (serde_json::Value::Object(b), serde_json::Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(slot) => merge_json(slot, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, v) => *b = v,
    }
}

impl WireLabApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        cc.egui_ctx.set_fonts(fonts);
        let assets = find_assets_dir();
        let mut theme_note = match load_theme(&assets.join("theme.json")) {
            Ok(style) => {
                cc.egui_ctx.set_theme(egui::Theme::Dark);
                cc.egui_ctx.set_style_of(egui::Theme::Dark, style);
                "theme loaded: assets/theme.json".to_string()
            }
            Err(e) => {
                cc.egui_ctx.set_visuals(egui::Visuals::dark());
                format!("theme.json not applied ({e}); using default dark")
            }
        };
        theme_note.truncate(120);
        let boards_dir = assets.join("boards");
        let (lib, mut console) =
            match Library::load(&boards_dir, &assets.join("components")) {
                Ok(lib) => {
                    let msg = format!(
                        "library loaded: {} boards, {} components",
                        lib.boards.len(),
                        lib.components.len()
                    );
                    (lib, vec![msg])
                }
                Err(e) => (Library::default(), vec![format!("library load failed: {e}")]),
            };
        let default_board = lib
            .boards
            .keys()
            .find(|k| k.contains("c3"))
            .or_else(|| lib.boards.keys().next())
            .cloned()
            .unwrap_or_else(|| "unknown".into());
        console.push(theme_note);
        console.push("welcome to WireLab — place components, wire them to pins, hit Connect".into());
        let mut examples: Vec<(String, PathBuf)> = std::fs::read_dir(assets.join("examples"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .filter_map(|p| {
                let name = Project::load(&p).ok()?.name;
                Some((name, p))
            })
            .collect();
        examples.sort_by(|a, b| a.1.cmp(&b.1));
        // The MCP AI-assistance server starts by default; WIRELAB_MCP=0 opts out.
        let mcp_enabled = std::env::var("WIRELAB_MCP").map(|v| v != "0").unwrap_or(true);
        let mcp = mcp_enabled.then(|| {
            let port = std::env::var("WIRELAB_MCP_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(crate::mcp::DEFAULT_PORT);
            crate::mcp::McpServer::start(port, cc.egui_ctx.clone())
        });
        let mcp = match mcp {
            Some(Ok(server)) => {
                console.push(format!("MCP server at {}", server.addr));
                Some(server)
            }
            Some(Err(e)) => {
                console.push(format!("MCP start failed: {e}"));
                None
            }
            None => None,
        };
        WireLabApp {
            lib,
            boards_dir,
            project: Project::new("untitled", &default_board),
            project_path: None,
            selection: Selection::None,
            canvas: CanvasState::default(),
            live: LiveState::default(),
            background: std::collections::HashMap::new(),
            ai: AiImportState::default(),
            console,
            topo_rev: 1,
            state_rev: 1,
            script_rev: 1,
            flow_rev: 1,
            flow_view: Default::default(),
            flow_cache: (0, None, Vec::new()),
            flow_code_open: false,
            board_rename: None,
            script_ed: ScriptEditor::default(),
            linter: crate::rhai_lint::Linter::new(),
            ide: IdeState { bottom_open: true, ..Default::default() },
            docs_open: false,
            docs_filter: String::new(),
            examples,
            flash: crate::flash::FlashState::default(),
            mcp,
            hover_highlight: Vec::new(),
            wiring_open: false,
            wiring_filter: String::new(),
            cache: ModelCache::default(),
        }
    }

    /// Focus (or open) a component's editor tab in the IDE window.
    pub fn open_script_tab(&mut self, id: CompId) {
        self.ide.open = true;
        let tab = IdeTab::Comp(id);
        if let Some(i) = self.ide.tabs.iter().position(|t| *t == tab) {
            self.ide.active = i;
        } else {
            self.ide.tabs.push(tab);
            self.ide.active = self.ide.tabs.len() - 1;
        }
    }

    pub fn open_info_tab(&mut self, key: &'static str) {
        self.ide.open = true;
        let tab = IdeTab::Info(key);
        if let Some(i) = self.ide.tabs.iter().position(|t| *t == tab) {
            self.ide.active = i;
        } else {
            self.ide.tabs.push(tab);
            self.ide.active = self.ide.tabs.len() - 1;
        }
    }

    pub fn open_flow_tab(&mut self) {
        self.ide.open = true;
        if let Some(i) = self.ide.tabs.iter().position(|t| *t == IdeTab::Flow) {
            self.ide.active = i;
        } else {
            self.ide.tabs.push(IdeTab::Flow);
            self.ide.active = self.ide.tabs.len() - 1;
        }
    }

    /// Recompile the active flow graph when its revision moved.
    fn ensure_flow_cache(&mut self) {
        if self.flow_cache.0 != self.flow_rev {
            let (code, errors) = if self.project.flow.nodes.is_empty() {
                (None, Vec::new())
            } else {
                match wirelab_core::flow::compile(&self.project.flow) {
                    Ok(code) => (Some(code), Vec::new()),
                    Err(errs) => (None, errs),
                }
            };
            self.flow_cache = (self.flow_rev, code, errors);
        }
    }

    fn rebuild_cache(&mut self) {
        if self.cache.rev == self.topo_rev {
            return;
        }
        let Some(board) = self.lib.board(&self.project.circuit.board_id) else { return };
        let netlist = Netlist::build(&self.project.circuit, board, &self.lib);
        let (_msgs, bindings) = plan_setup(&self.project.circuit, board, &self.lib, &netlist);
        let lints = validate(&self.project.circuit, board, &self.lib, &netlist);
        self.cache = ModelCache { rev: self.topo_rev, netlist, bindings, lints };
    }

    fn log(&mut self, line: impl Into<String>) {
        self.console.push(line.into());
        if self.console.len() > 500 {
            self.console.drain(..250);
        }
    }

    fn save_project(&mut self, pick_path: bool) {
        let path = if pick_path || self.project_path.is_none() {
            rfd::FileDialog::new()
                .add_filter("WireLab project", &["json"])
                .set_file_name(format!("{}.wirelab.json", self.project.name))
                .save_file()
        } else {
            self.project_path.clone()
        };
        let Some(path) = path else { return };
        match self.project.save(&path) {
            Ok(()) => {
                self.log(format!("saved {}", path.display()));
                self.project_path = Some(path);
            }
            Err(e) => self.log(format!("save failed: {e}")),
        }
    }

    fn open_project(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("WireLab project", &["json"])
            .pick_file()
        else {
            return;
        };
        self.load_project_path(&path, true);
    }

    pub(crate) fn load_project_path(&mut self, path: &std::path::Path, remember_path: bool) {
        match Project::load(path) {
            Ok(project) => {
                self.live.disconnect(&mut self.console);
                for (_, mut bb) in self.background.drain() {
                    bb.live.disconnect(&mut self.console);
                }
                self.project = project;
                self.project_path = remember_path.then(|| path.to_path_buf());
                self.selection = Selection::None;
                self.script_ed = crate::app::ScriptEditor::default();
                self.topo_rev += 1;
                self.state_rev += 1;
                self.script_rev += 1;
                self.flow_rev += 1;
                self.log(format!("opened {}", path.display()));
            }
            Err(e) => self.log(format!("open failed: {e}")),
        }
    }

    /// Park the active board's live session (still ticking) with everything a
    /// background tick needs: its netlist/bindings, compiled flow, and the rev
    /// numbers it was synced at.
    fn stash_active_live(&mut self) {
        let id = self.project.boards[self.project.active].id;
        let live = std::mem::take(&mut self.live);
        if !live.connected() {
            return;
        }
        self.rebuild_cache();
        self.ensure_flow_cache();
        self.background.insert(
            id,
            BackgroundBoard {
                live,
                netlist: self.cache.netlist.clone(),
                bindings: self.cache.bindings.clone(),
                flow_code: self.flow_cache.1.clone(),
                revs: (self.topo_rev, self.state_rev, self.script_rev, self.flow_rev),
            },
        );
    }

    /// Switch to board tab `i`: park the current session (it keeps running),
    /// restore the target's, and reload everything derived.
    fn switch_board(&mut self, i: usize) {
        if i == self.project.active {
            return;
        }
        self.stash_active_live();
        self.project.switch_to(i);
        let id = self.project.boards[i].id;
        self.live = self.background.remove(&id).map(|b| b.live).unwrap_or_default();
        self.selection = Selection::None;
        self.script_ed = crate::app::ScriptEditor::default();
        self.topo_rev += 1;
        self.state_rev += 1;
        self.script_rev += 1;
        self.flow_rev += 1;
    }

    fn add_board(&mut self, board_id: &str) {
        self.stash_active_live();
        self.project.add_board(board_id);
        self.selection = Selection::None;
        self.script_ed = crate::app::ScriptEditor::default();
        self.topo_rev += 1;
        self.state_rev += 1;
        self.script_rev += 1;
        self.flow_rev += 1;
        self.log(format!("added {} ({})", self.project.active_name(), board_id));
    }

    /// The board-tab strip: a chip per board, plus add / rename / remove.
    fn show_board_tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let active = self.project.active;
            let count = self.project.boards.len();
            let mut switch_to: Option<usize> = None;
            let mut remove: Option<usize> = None;
            let mut commit_rename = false;

            for i in 0..count {
                if matches!(&self.board_rename, Some((idx, _)) if *idx == i) {
                    // Inline rename field for this tab.
                    if let Some((_, buf)) = &mut self.board_rename {
                        let r = ui.add(
                            egui::TextEdit::singleline(buf).desired_width(90.0),
                        );
                        r.request_focus();
                        if r.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            commit_rename = true;
                        }
                    }
                    continue;
                }

                let tab = &self.project.boards[i];
                let is_live = if i == active {
                    self.live.connected()
                } else {
                    self.background.get(&tab.id).is_some_and(|b| b.live.connected())
                };
                let chip = format!(
                    "{}{}  ·  {}",
                    if is_live { "● " } else { "" },
                    tab.name,
                    tab.circuit.board_id
                );
                let mut text = RichText::new(chip).strong();
                if is_live {
                    text = text.color(egui::Color32::from_rgb(90, 220, 120));
                }
                let resp = ui
                    .selectable_label(i == active, text)
                    .on_hover_text(if is_live { "session live" } else { "not connected" });
                if resp.clicked() {
                    switch_to = Some(i);
                }
                if resp.double_clicked() {
                    self.board_rename = Some((i, self.project.boards[i].name.clone()));
                }
                resp.context_menu(|ui| {
                    if ui.button("Rename").clicked() {
                        self.board_rename = Some((i, self.project.boards[i].name.clone()));
                        ui.close();
                    }
                    if count > 1 && ui.button("Remove board").clicked() {
                        remove = Some(i);
                        ui.close();
                    }
                });
            }

            if commit_rename
                && let Some((idx, buf)) = self.board_rename.take()
            {
                let name = buf.trim();
                if !name.is_empty() {
                    self.project.boards[idx].name = name.to_string();
                }
            }

            ui.separator();
            let boards = self.lib.boards.keys().cloned().collect::<Vec<_>>();
            ui.menu_button("➕ board", |ui| {
                for id in &boards {
                    let name = self.lib.board(id).map(|b| b.name.clone()).unwrap_or_else(|| id.clone());
                    if ui.button(name).clicked() {
                        self.add_board(id);
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text("add another ESP32 board to this project");

            if let Some(i) = switch_to {
                self.switch_board(i);
            }
            if let Some(i) = remove {
                let removed_id = self.project.boards[i].id;
                if let Some(mut bb) = self.background.remove(&removed_id) {
                    bb.live.disconnect(&mut self.console);
                }
                self.project.remove_board(i);
                self.selection = Selection::None;
                self.script_ed = crate::app::ScriptEditor::default();
                self.topo_rev += 1;
                self.state_rev += 1;
                self.script_rev += 1;
                self.flow_rev += 1;
                if self.live.connected() {
                    self.live.disconnect(&mut self.console);
                }
            }
        });
    }

    fn show_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("WireLab").strong().size(17.0));
            ui.separator();
            ui.menu_button("File", |ui| {
                if ui.button("New").clicked() {
                    self.live.disconnect(&mut self.console);
                    for (_, mut bb) in self.background.drain() {
                        bb.live.disconnect(&mut self.console);
                    }
                    let board = self.project.circuit.board_id.clone();
                    self.project = Project::new("untitled", &board);
                    self.project_path = None;
                    self.selection = Selection::None;
                    self.topo_rev += 1;
                    self.flow_rev += 1;
                    ui.close();
                }
                if ui.button("Open…").clicked() {
                    self.open_project();
                    ui.close();
                }
                if ui.button("Save").clicked() {
                    self.save_project(false);
                    ui.close();
                }
                if ui.button("Save as…").clicked() {
                    self.save_project(true);
                    ui.close();
                }
            });
            ui.menu_button("Tools", |ui| {
                if ui
                    .button(if self.ide.open { "📜 Hide IDE" } else { "📜 Open IDE" })
                    .on_hover_text("the script IDE window (editors, hardware tree, problems)")
                    .clicked()
                {
                    self.ide.open = !self.ide.open;
                    ui.close();
                }
                if !self.examples.is_empty() {
                    ui.menu_button("Examples", |ui| {
                        let examples = self.examples.clone();
                        for (name, path) in &examples {
                            if ui.button(name).clicked() {
                                // Examples never save back over the shipped file.
                                self.load_project_path(path, false);
                                ui.close();
                            }
                        }
                    });
                }
                if ui
                    .button("✨ AI board import")
                    .on_hover_text("turn a manufacturer pinout/spec into a board profile")
                    .clicked()
                {
                    self.ai.open = true;
                    ui.close();
                }
                ui.separator();
                self.mcp_submenu(ui);
            });
            ui.separator();
            ui.label("project");
            ui.add(egui::TextEdit::singleline(&mut self.project.name).desired_width(140.0));
            if let Selection::Comps(v) = &self.selection
                && v.len() >= 2 {
                    let n = v.len();
                    if ui
                        .button(RichText::new(format!("⚡ Auto wire ({n})")).strong())
                        .on_hover_text("wire the selected components to free board pins")
                        .clicked()
                    {
                        self.apply_auto_wire();
                    }
                }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.show_device_bar(ui);
            });
        });
    }

    /// The MCP-server submenu: status, connect string, and a start/stop toggle.
    fn mcp_submenu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("🤖 MCP server", |ui| {
            let running = self.mcp.is_some();
            if let Some(server) = &self.mcp {
                ui.label(
                    RichText::new("● running").color(egui::Color32::from_rgb(90, 220, 120)),
                );
                ui.label(RichText::new(server.addr.to_string()).monospace().small());
                ui.label(
                    RichText::new(format!("claude mcp add -t http wirelab {}", server.addr))
                        .monospace()
                        .small()
                        .color(egui::Color32::from_gray(150)),
                );
            } else {
                ui.label(RichText::new("○ stopped").weak());
            }
            ui.separator();
            if ui.button(if running { "Stop server" } else { "Start server" }).clicked() {
                if running {
                    self.mcp = None;
                    self.console.push("MCP server stopped".into());
                } else {
                    let port = std::env::var("WIRELAB_MCP_PORT")
                        .ok()
                        .and_then(|p| p.parse().ok())
                        .unwrap_or(crate::mcp::DEFAULT_PORT);
                    match crate::mcp::McpServer::start(port, ui.ctx().clone()) {
                        Ok(server) => {
                            self.console.push(format!("MCP server at {}", server.addr));
                            self.console.push(format!(
                                "  connect: claude mcp add -t http wirelab {}",
                                server.addr
                            ));
                            self.mcp = Some(server);
                        }
                        Err(e) => self.console.push(format!("MCP start failed: {e}")),
                    }
                }
                ui.close();
            }
        })
        .response
        .on_hover_text("AI assistance over MCP: build, validate, script, read live telemetry");
    }

    fn show_ai_window(&mut self, ctx: &egui::Context) {
        if !self.ai.open {
            return;
        }
        let mut open = self.ai.open;
        let mut adopt: Option<wirelab_core::board::BoardProfile> = None;
        egui::Window::new("AI board import")
            .open(&mut open)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.label("Paste the manufacturer pinout table, datasheet excerpt or product page text; Claude turns it into a board profile.");
                ui.horizontal(|ui| {
                    ui.label("board name");
                    ui.text_edit_singleline(&mut self.ai.board_name);
                });
                if std::env::var("ANTHROPIC_API_KEY").is_err() {
                    ui.horizontal(|ui| {
                        ui.label("API key");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.ai.api_key)
                                .password(true)
                                .desired_width(260.0),
                        );
                    });
                }
                egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.ai.spec_text)
                            .desired_width(f32::INFINITY)
                            .desired_rows(10)
                            .hint_text("GPIO table, header layout, strapping pin notes..."),
                    );
                });
                ui.horizontal(|ui| {
                    let can_go = !self.ai.busy
                        && !self.ai.spec_text.trim().is_empty()
                        && !self.ai.board_name.trim().is_empty();
                    if ui.add_enabled(can_go, egui::Button::new("Generate profile")).clicked() {
                        self.ai.start();
                    }
                    if self.ai.busy {
                        ui.spinner();
                        ui.label("asking Claude…");
                    }
                });
                if let Some(err) = &self.ai.error {
                    let error_color = ui.visuals().error_fg_color;
                    ui.colored_label(error_color, err);
                }
                if let Some(profile) = &self.ai.preview {
                    ui.separator();
                    ui.label(
                        RichText::new(format!(
                            "{} — {} pins ({})",
                            profile.name,
                            profile.pins.len(),
                            profile.chip.name()
                        ))
                        .strong(),
                    );
                    let warn_color = ui.visuals().warn_fg_color;
                    for p in &self.ai.problems {
                        ui.colored_label(warn_color, format!("⚠ {p}"));
                    }
                    if self.ai.problems.is_empty() {
                        ui.colored_label(Color32::from_rgb(90, 220, 120), "profile passes all checks");
                    }
                    if ui.button("Add to library & save").clicked() {
                        adopt = Some(profile.clone());
                    }
                }
            });
        self.ai.open = open;
        if let Some(profile) = adopt {
            match save_profile(&self.boards_dir, &profile) {
                Ok(path) => self.log(format!("board profile saved to {}", path.display())),
                Err(e) => self.log(format!("could not save profile: {e}")),
            }
            self.lib.add_board(profile);
            self.ai.preview = None;
            self.ai.open = false;
        }
    }
}

impl eframe::App for WireLabApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.ai.poll() {
            ctx.request_repaint();
        }
        // Refreshed by the Checks panel while a warning row is hovered.
        self.hover_highlight.clear();
        self.rebuild_cache();
        if self.pump_mcp() {
            ctx.request_repaint();
        }

        self.ensure_flow_cache();
        let mut log = std::mem::take(&mut self.console);
        let ticked = self.live.tick(
            &self.lib,
            &self.project.circuit,
            &self.cache.netlist,
            &self.cache.bindings,
            self.topo_rev,
            self.state_rev,
            self.script_rev,
            self.flow_rev,
            self.flow_cache.1.as_deref(),
            &mut log,
        );
        self.console = log;

        // Parked boards keep running: tick each with its frozen caches.
        let mut bg_ticked = false;
        for (id, bb) in self.background.iter_mut() {
            let Some(idx) = self.project.boards.iter().position(|b| b.id == *id) else {
                continue;
            };
            let mut log = Vec::new();
            bg_ticked |= bb.live.tick(
                &self.lib,
                &self.project.boards[idx].circuit,
                &bb.netlist,
                &bb.bindings,
                bb.revs.0,
                bb.revs.1,
                bb.revs.2,
                bb.revs.3,
                bb.flow_code.as_deref(),
                &mut log,
            );
            let name = &self.project.boards[idx].name;
            self.console.extend(log.into_iter().map(|l| format!("[{name}] {l}")));
        }

        // Route send_board() mail between every live board, active included.
        let mut mail: Vec<(String, String, String)> = Vec::new();
        let active_name = self.project.active_name().to_string();
        mail.extend(self.live.outbox.drain(..).map(|(to, text)| (active_name.clone(), to, text)));
        for (id, bb) in self.background.iter_mut() {
            let Some(from) =
                self.project.boards.iter().find(|b| b.id == *id).map(|b| b.name.clone())
            else {
                continue;
            };
            mail.extend(bb.live.outbox.drain(..).map(|(to, text)| (from.clone(), to, text)));
        }
        for (from, to, text) in mail {
            let target = self
                .project
                .boards
                .iter()
                .position(|b| b.name.eq_ignore_ascii_case(to.trim()));
            let mut log = Vec::new();
            match target {
                Some(i) if i == self.project.active => {
                    self.live.deliver_board_msg(&from, &text, &mut log);
                }
                Some(i) => {
                    let id = self.project.boards[i].id;
                    match self.background.get_mut(&id) {
                        Some(bb) => bb.live.deliver_board_msg(&from, &text, &mut log),
                        None => log.push(format!("'{to}' is not connected — message dropped")),
                    }
                }
                None => log.push(format!("send_board: no board named '{to}'")),
            }
            self.console.extend(log);
        }

        let mut log = std::mem::take(&mut self.console);
        let flashing = self.flash.poll(&mut log);
        self.console = log;
        if ticked || bg_ticked || self.ai.busy || flashing {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(4.0);
            self.show_toolbar(ui);
            ui.add_space(4.0);
        });
        egui::Panel::top("board-tabs").show(ui, |ui| {
            self.show_board_tabs(ui);
        });
        egui::Panel::left("palette")
            .resizable(true)
            .default_size(210.0)
            .show(ui, |ui| self.show_palette(ui));
        egui::Panel::right("inspector")
            .resizable(true)
            .default_size(300.0)
            .show(ui, |ui| self.show_right_panel(ui));
        egui::Panel::bottom("bottom")
            .resizable(true)
            .default_size(280.0)
            .show(ui, |ui| self.show_bottom(ui));
        egui::CentralPanel::no_frame().show(ui, |ui| self.show_canvas(ui));

        self.show_ai_window(&ctx);
        crate::rhai_docs::show_docs_window(&ctx, &mut self.docs_open, &mut self.docs_filter);
        crate::wiring_guide::show_wiring_window(&ctx, &mut self.wiring_open, &mut self.wiring_filter);

        if self.ide.open {
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("wirelab-ide"),
                egui::ViewportBuilder::default()
                    .with_title("WireLab — Script IDE")
                    .with_inner_size([1060.0, 640.0])
                    .with_min_inner_size([620.0, 360.0]),
                |vui, _class| {
                    self.show_ide(vui);
                    if vui.ctx().input(|i| i.viewport().close_requested()) {
                        self.ide.open = false;
                    }
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn shipped_theme_deserializes() {
        let path = super::find_assets_dir().join("theme.json");
        let style = super::load_theme(&path).expect("theme.json parses as egui::Style");
        assert!(style.visuals.dark_mode);
        assert_eq!(style.visuals.panel_fill, egui::Color32::from_rgb(0, 0, 0));
    }
}
