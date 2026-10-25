//! Top-level application state and frame orchestration.

use std::path::PathBuf;

use egui::RichText;
use egui_theme_kit::chrome::{self, Chrome};
use egui_theme_kit::controls::{self, BarKind, MenuButton};
use egui_theme_kit::title_bar::{Side, TitleBar, window_edges};
use wirelab_core::circuit::{CompId, WireId};
use wirelab_core::engine::{Bindings, plan_setup};
use wirelab_core::library::Library;
use wirelab_core::netlist::Netlist;
use wirelab_core::project::Project;
use wirelab_core::validate::{Lint, validate};

use crate::ai_import::{AiImportState, save_profile};
use crate::canvas::CanvasState;
use crate::dock::{DockRequest, Tab};
use crate::live::LiveState;
use crate::script_docs::ScriptDocs;
use crate::theme::ThemeState;
use crate::ui_kit::{self, ph};

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

impl BackgroundBoard {
    /// GPIOs currently driven as outputs, for wire-safety verdicts.
    pub fn output_gpios(&self) -> Vec<u8> {
        self.bindings.outputs.values().map(|o| o.gpio).collect()
    }

    /// Re-plan after a remote structural edit: fresh netlist + bindings, and
    /// a bumped topo revision so the live session reapplies its pin setup.
    pub fn set_topo(&mut self, circuit: &wirelab_core::circuit::Circuit, lib: &Library) {
        if let Some(board) = lib.board(&circuit.board_id) {
            let netlist = Netlist::build(circuit, board, lib);
            let (_msgs, bindings) = plan_setup(circuit, board, lib, &netlist);
            self.netlist = netlist;
            self.bindings = bindings;
            self.revs.0 += 1;
            self.revs.1 += 1;
        }
    }

    /// Bump the frozen script revision so the next tick re-syncs scripts.
    pub fn bump_script(&mut self) {
        self.revs.2 += 1;
    }

    /// Hot-swap this parked board's flow (e.g. a remote edit): recompile and
    /// bump the frozen flow revision so its next tick installs the script.
    pub fn set_flow(&mut self, graph: &wirelab_core::flow::FlowGraph) {
        self.flow_code = if graph.nodes.is_empty() {
            None
        } else {
            wirelab_core::flow::compile(graph).ok()
        };
        self.revs.3 += 1;
    }
}

/// A project pushed over the LAN, held until the user accepts or declines it.
/// The listener is unauthenticated, so nothing here reaches `project` without
/// a click.
pub struct PendingImport {
    project: Project,
    /// What the sender called itself, if it said.
    from: Option<String>,
    /// Identifies this payload against the summary the modal drew from it.
    id: u64,
    /// Set once the modal has drawn this payload.
    shown: bool,
}

impl PendingImport {
    pub fn project(&self) -> &Project {
        &self.project
    }
    pub fn from(&self) -> Option<&str> {
        self.from.as_deref()
    }
    pub fn into_project(self) -> Project {
        self.project
    }
}

/// Holds at most one pushed project. Once the modal draws a payload it is
/// locked: further pushes are refused rather than swapping out the project the
/// user is being asked about, and only the drawn payload can be adopted.
#[derive(Default)]
pub struct ImportGate {
    pending: Option<PendingImport>,
    next_id: u64,
}

impl ImportGate {
    /// Parks a pushed project; errs while a drawn payload awaits a decision.
    pub fn park(&mut self, project: Project, from: Option<String>) -> Result<(), String> {
        if let Some(p) = &self.pending
            && p.shown
        {
            return Err(format!(
                "'{}' is already awaiting a decision in WireLab — retry once it is accepted or declined",
                p.project.name
            ));
        }
        self.next_id += 1;
        self.pending = Some(PendingImport { project, from, id: self.next_id, shown: false });
        Ok(())
    }

    /// Marks the pending payload as drawn and returns its id.
    pub fn lock_shown(&mut self) -> Option<u64> {
        let p = self.pending.as_mut()?;
        p.shown = true;
        Some(p.id)
    }

    pub fn pending(&self) -> Option<&PendingImport> {
        self.pending.as_ref()
    }

    /// Removes the pending payload only when it is the one identified by `id`.
    pub fn take(&mut self, id: u64) -> Option<PendingImport> {
        match &self.pending {
            Some(p) if p.id == id => self.pending.take(),
            _ => None,
        }
    }
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
    /// The flow tab shows the generated script beside the graph.
    pub flow_code_open: bool,
    /// Inline board-tab rename in progress: (tab index, edit buffer).
    pub board_rename: Option<(usize, String)>,
    /// Script buffers and diagnostics.
    pub docs: ScriptDocs,
    pub dock: egui_dock::DockState<Tab>,
    /// Tab changes queued during the frame, applied around the dock pass.
    pub dock_requests: Vec<DockRequest>,
    /// Tabs open at the start of the dock pass.
    pub dock_open: std::collections::HashSet<Tab>,
    pub console_filter: String,
    pub console_follow: bool,
    /// Search text of the parts library.
    pub parts_filter: String,
    pub docs_filter: String,
    /// Shipped example projects: (display name, path).
    pub examples: Vec<(String, PathBuf)>,
    pub flash: crate::flash::FlashState,
    /// Embedded MCP server for AI circuit assistance.
    pub mcp: Option<crate::mcp::McpServer>,
    /// Project pushed by a remote editor, awaiting the confirm modal.
    pub import: ImportGate,
    /// Components to spotlight this frame (hovered warning in Checks).
    pub hover_highlight: Vec<CompId>,
    pub wiring_filter: String,
    pub cache: ModelCache,
    /// Custom title bar (frameless) or the system one.
    pub chrome: Chrome,
    pub theme: ThemeState,
    /// Uploaded image of the simulated / mirrored display.
    pub lcd_texture: crate::lcd::LcdTexture,
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

impl WireLabApp {
    pub fn new(cc: &eframe::CreationContext<'_>, chrome: Chrome) -> Self {
        let assets = find_assets_dir();
        let (theme, mut theme_note) = ThemeState::new(cc, &assets.join("theme.json"));
        egui_icon_pack::install(&cc.egui_ctx);
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
        let mut app = WireLabApp {
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
            docs: ScriptDocs::default(),
            dock: crate::dock::load_layout(cc.storage),
            dock_requests: Vec::new(),
            dock_open: Default::default(),
            console_filter: String::new(),
            console_follow: true,
            parts_filter: String::new(),
            docs_filter: String::new(),
            examples,
            flash: crate::flash::FlashState::default(),
            mcp,
            import: ImportGate::default(),
            hover_highlight: Vec::new(),
            wiring_filter: String::new(),
            cache: ModelCache::default(),
            chrome,
            theme,
            lcd_texture: Default::default(),
        };
        // WIRELAB_OPEN=<project.json> opens a project at startup.
        if let Ok(path) = std::env::var("WIRELAB_OPEN") {
            app.load_project_path(std::path::Path::new(&path), true);
        }
        app
    }

    /// Focus (or open) a component's script tab.
    pub fn open_script_tab(&mut self, id: CompId) {
        self.open_tab(Tab::Script(id));
    }

    /// The script name of a component, as scripts address it.
    pub fn comp_name(&self, id: CompId) -> String {
        wirelab_core::script::component_names(&self.project.circuit, &self.lib)
            .remove(&id)
            .unwrap_or_else(|| format!("#{}", id.0))
    }

    /// Forget script buffers and their tabs; component ids now mean another circuit.
    fn forget_scripts(&mut self) {
        self.docs.clear();
        self.dock_requests.push(DockRequest::CloseScripts);
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

    pub(crate) fn log(&mut self, line: impl Into<String>) {
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
                self.adopt_project(project, remember_path.then(|| path.to_path_buf()));
                self.log(format!("opened {}", path.display()));
            }
            Err(e) => self.log(format!("open failed: {e}")),
        }
    }

    /// Replace the open project: drop every live session and reset the derived
    /// state. `path` is the file Save writes back to; `None` leaves it unsaved.
    pub(crate) fn adopt_project(&mut self, project: Project, path: Option<PathBuf>) {
        self.live.disconnect(&mut self.console);
        for (_, mut bb) in self.background.drain() {
            bb.live.disconnect(&mut self.console);
        }
        self.project = project;
        self.project_path = path;
        self.selection = Selection::None;
        self.forget_scripts();
        self.canvas.fit_pending = true;
        self.topo_rev += 1;
        self.state_rev += 1;
        self.script_rev += 1;
        self.flow_rev += 1;
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
    pub(crate) fn switch_board(&mut self, i: usize) {
        if i == self.project.active {
            return;
        }
        self.stash_active_live();
        self.project.switch_to(i);
        let id = self.project.boards[i].id;
        self.live = self.background.remove(&id).map(|b| b.live).unwrap_or_default();
        self.selection = Selection::None;
        self.forget_scripts();
        self.topo_rev += 1;
        self.state_rev += 1;
        self.script_rev += 1;
        self.flow_rev += 1;
    }

    fn add_board(&mut self, board_id: &str) {
        self.stash_active_live();
        self.project.add_board(board_id);
        self.selection = Selection::None;
        self.forget_scripts();
        self.topo_rev += 1;
        self.state_rev += 1;
        self.script_rev += 1;
        self.flow_rev += 1;
        self.log(format!("added {} ({})", self.project.active_name(), board_id));
    }

    /// The board strip: a pill per board (double-click renames, right-click removes) and Add board.
    fn show_board_tabs(&mut self, ui: &mut egui::Ui) {
        let active = self.project.active;
        let count = self.project.boards.len();
        let mut switch_to: Option<usize> = None;
        let mut remove: Option<usize> = None;
        let mut commit_rename = false;

        for i in 0..count {
            if let Some((idx, buf)) = &mut self.board_rename
                && *idx == i
            {
                let r = controls::text_field(ui, buf, "Board name", 150.0);
                r.request_focus();
                if r.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    commit_rename = true;
                }
                continue;
            }
            let tab = &self.project.boards[i];
            let is_live = if i == active {
                self.live.connected()
            } else {
                self.background.get(&tab.id).is_some_and(|b| b.live.connected())
            };
            let label = format!("{}  ·  {}", tab.name, tab.circuit.board_id);
            let icon = self.lib.board(&tab.circuit.board_id).map_or(crate::icons::BOARD_DEVKIT, crate::icons::board);
            let mut pill = controls::Pill::new(&label).icon(ui_kit::kit(icon)).selected(i == active);
            if is_live {
                pill = pill.tone(egui_theme_kit::success(ui));
            }
            let tip = if is_live { "Session live · double-click to rename" } else { "Not connected · double-click to rename" };
            let resp = pill.show(ui).on_hover_text(tip);
            if resp.clicked() {
                switch_to = Some(i);
            }
            if resp.double_clicked() {
                self.board_rename = Some((i, self.project.boards[i].name.clone()));
            }
            resp.context_menu(|ui| {
                if ui_kit::menu_item(ui, ph::PENCIL_SIMPLE, "Rename").clicked() {
                    self.board_rename = Some((i, self.project.boards[i].name.clone()));
                    ui.close();
                }
                if count > 1 && controls::menu_danger_item(ui, ui_kit::kit(ph::TRASH), "Remove board").clicked() {
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

        controls::vline(ui, egui_theme_kit::metrics(ui.ctx()).control_h);
        let boards: Vec<(String, String)> = self.lib.boards.iter().map(|(id, b)| (id.clone(), b.name.clone())).collect();
        let _ = MenuButton::new("Add board").icon(ui_kit::kit(ph::PLUS)).tip("Add another ESP32 board to this project").show(ui, |ui| {
            for (id, name) in &boards {
                let icon = self.lib.board(id).map_or(crate::icons::BOARD_DEVKIT, crate::icons::board);
                if controls::menu_item(ui, ui_kit::kit(icon), name).clicked() {
                    self.add_board(id);
                    ui.close();
                }
            }
        });

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
            self.forget_scripts();
            self.topo_rev += 1;
            self.state_rev += 1;
            self.script_rev += 1;
            self.flow_rev += 1;
            if self.live.connected() {
                self.live.disconnect(&mut self.console);
            }
        }
    }

    /// Window top strip: menus and project name on the left, device controls on the right.
    fn show_title_bar(&mut self, ui: &mut egui::Ui) {
        let frameless = self.chrome.frameless;
        let _ = TitleBar::new("wirelab.title_bar", if frameless { "WireLab" } else { "" })
            .frameless(frameless)
            .mark(ui_kit::kit(ph::CIRCUITRY))
            .show(ui, |ui, side| match side {
                Side::Left => self.show_menus(ui),
                Side::Right => self.show_device_bar(ui),
            });
    }

    fn new_project(&mut self) {
        let board = self.project.circuit.board_id.clone();
        self.adopt_project(Project::new("untitled", &board), None);
    }

    fn show_menus(&mut self, ui: &mut egui::Ui) {
        let _ = MenuButton::new("File").show(ui, |ui| {
            if ui_kit::menu_item(ui, ph::FILE_PLUS, "New project").clicked() {
                self.new_project();
                ui.close();
            }
            if ui_kit::menu_item(ui, ph::FOLDER_OPEN, "Open…").clicked() {
                self.open_project();
                ui.close();
            }
            if ui_kit::menu_item(ui, ph::FLOPPY_DISK, "Save").clicked() {
                self.save_project(false);
                ui.close();
            }
            if ui_kit::menu_item(ui, ph::EXPORT, "Save as…").clicked() {
                self.save_project(true);
                ui.close();
            }
            if !self.examples.is_empty() {
                controls::menu_separator(ui);
                let examples = self.examples.clone();
                let _ = controls::submenu(ui, ui_kit::kit(ph::BOOK_OPEN), "Examples", |ui| {
                    for (name, path) in &examples {
                        // Examples never save back over the shipped file.
                        if ui_kit::menu_item(ui, ph::FILE, name).clicked() {
                            self.load_project_path(path, false);
                            ui.close();
                        }
                    }
                });
            }
        });
        let _ = MenuButton::new("View").keep_open(true).show(ui, |ui| {
            for tab in Tab::PANELS {
                let open = self.dock.find_tab(&tab).is_some();
                if controls::menu_check_item(ui, ui_kit::kit(tab.icon()), tab.label(), open).clicked() {
                    let request = if open && tab != Tab::Schematic { DockRequest::Close(tab) } else { DockRequest::Open(tab) };
                    self.dock_requests.push(request);
                }
            }
            controls::menu_separator(ui);
            if ui_kit::menu_item(ui, ph::LAYOUT, "Reset layout").clicked() {
                self.dock_requests.push(DockRequest::Reset);
                ui.close();
            }
            controls::menu_separator(ui);
            let _ = chrome::toggle_row(ui, &mut self.chrome, "Custom title bar");
            if ui_kit::menu_item(ui, ph::PALETTE, "Theme settings…").clicked() {
                self.theme.picker.open = true;
                ui.close();
            }
            if ui_kit::menu_item(ui, ph::SQUARES_FOUR, "Icon gallery").clicked() {
                egui_icon_pack::gallery::toggle(ui.ctx());
                ui.close();
            }
        });
        let _ = MenuButton::new("Tools").show(ui, |ui| {
            if ui_kit::menu_item(ui, ph::SPARKLE, "AI board import…")
                .on_hover_text("Turn a manufacturer pinout or spec into a board profile")
                .clicked()
            {
                self.ai.open = true;
                ui.close();
            }
            self.mcp_submenu(ui);
        });
        controls::vline(ui, egui_theme_kit::metrics(ui.ctx()).control_h);
        controls::TextField::new(&mut self.project.name).hint("Project name").icon(ui_kit::kit(ph::CIRCUITRY)).width(170.0).show(ui);
        if let Selection::Comps(v) = &self.selection
            && v.len() >= 2
        {
            let label = format!("Auto wire ({})", v.len());
            if ui_kit::button(ui, ph::MAGIC_WAND, &label)
                .on_hover_text("Wire the selected parts to free board pins")
                .clicked()
            {
                self.apply_auto_wire();
            }
        }
    }

    /// The MCP server: status, connect command, start / stop.
    fn mcp_submenu(&mut self, ui: &mut egui::Ui) {
        let running = self.mcp.as_ref().map(|s| s.addr.clone());
        let _ = controls::submenu(ui, ui_kit::kit(ph::ROBOT), "MCP server", |ui| {
            match running {
                Some(addr) => {
                    controls::menu_inset_row(ui, |ui| {
                        ui_kit::status(ui, ph::PULSE, &format!("Running at {addr}"), egui_theme_kit::success(ui))
                    });
                    let cmd = format!("claude mcp add -t http wirelab {addr}");
                    if ui_kit::menu_item(ui, ph::COPY, "Copy connect command").on_hover_text(&cmd).clicked() {
                        ui.ctx().copy_text(cmd);
                        ui.close();
                    }
                    if ui_kit::menu_item(ui, ph::STOP, "Stop server").clicked() {
                        self.mcp = None;
                        self.log("MCP server stopped");
                        ui.close();
                    }
                }
                None => {
                    let weak = ui.visuals().weak_text_color();
                    controls::menu_inset_row(ui, |ui| ui_kit::status(ui, ph::CIRCLE_DASHED, "Stopped", weak));
                    if ui_kit::menu_item(ui, ph::PLAY, "Start server").clicked() {
                        let port = std::env::var("WIRELAB_MCP_PORT")
                            .ok()
                            .and_then(|p| p.parse().ok())
                            .unwrap_or(crate::mcp::DEFAULT_PORT);
                        match crate::mcp::McpServer::start(port, ui.ctx().clone()) {
                            Ok(server) => {
                                self.log(format!("MCP server at {}", server.addr));
                                self.log(format!("  connect: claude mcp add -t http wirelab {}", server.addr));
                                self.mcp = Some(server);
                            }
                            Err(e) => self.log(format!("MCP start failed: {e}")),
                        }
                        ui.close();
                    }
                }
            }
        });
    }

    /// Bottom status line: link, board, scripts, problems, program and MCP state.
    fn show_status_bar(&mut self, ui: &mut egui::Ui) {
        use wirelab_link::SessionPhase;
        let weak = ui.visuals().weak_text_color();
        let control_h = egui_theme_kit::metrics(ui.ctx()).control_h;
        let backend = match self.live.backend {
            crate::live::Backend::Simulator => "simulator",
            crate::live::Backend::Serial => "USB serial",
            crate::live::Backend::Tcp => "Wi-Fi",
        };
        let (icon, text, tone) = match self.live.session.as_ref().map(|s| s.phase) {
            Some(SessionPhase::Ready) => (ph::PLUGS_CONNECTED, format!("Live · {backend}"), egui_theme_kit::success(ui)),
            Some(SessionPhase::AwaitingHello) => (ph::HOURGLASS, format!("Connecting · {backend}"), ui.visuals().warn_fg_color),
            Some(SessionPhase::Dead) => (ph::PLUG, "Link lost".to_string(), ui.visuals().error_fg_color),
            None => (ph::PLUG, "Offline".to_string(), weak),
        };
        ui_kit::status(ui, icon, &text, tone);
        controls::vline(ui, control_h);
        let board = self.lib.board(&self.project.circuit.board_id).map(|b| b.name.clone()).unwrap_or_default();
        ui_kit::status(ui, ph::CPU, &format!("{} · {board}", self.project.active_name()), weak);
        controls::vline(ui, control_h);
        let scripted = self.project.circuit.components.values().filter(|c| c.script.is_some()).count();
        let scripts = if scripted == 1 { "1 script".to_string() } else { format!("{scripted} scripts") };
        if ui_kit::status(ui, ph::FILE_CODE, &scripts, weak).on_hover_text("Open the Explorer").clicked() {
            self.open_tab(Tab::Explorer);
        }
        let (errors, warnings) = self.docs.counts();
        let checks = self.cache.lints.len() + self.cache.bindings.warnings.len();
        let problems = if errors + warnings == 0 {
            ui_kit::status(ui, ph::CHECK_CIRCLE, "No script problems", weak)
        } else {
            let tone = if errors > 0 { ui.visuals().error_fg_color } else { ui.visuals().warn_fg_color };
            ui_kit::status(ui, ph::BUG, &format!("{errors} errors · {warnings} warnings"), tone)
        };
        if problems.on_hover_text("Open the Problems panel").clicked() {
            self.open_tab(Tab::Problems);
        }
        if checks > 0
            && ui_kit::status(ui, ph::SHIELD_WARNING, &format!("{checks} circuit checks"), ui.visuals().warn_fg_color)
                .on_hover_text("Open the Checks panel")
                .clicked()
        {
            self.open_tab(Tab::Checks);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(server) = &self.mcp {
                ui_kit::status(ui, ph::ROBOT, &format!("MCP {}", server.addr), weak);
            }
            if self.live.program_running {
                ui_kit::status(ui, ph::LIST_CHECKS, "Rules running", egui_theme_kit::success(ui));
            }
        });
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
                    if ui_kit::primary(ui, can_go, ph::SPARKLE, "Generate").on_hover_text("Generate a board profile").clicked() {
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
                        ui_kit::status(ui, ph::SEAL_CHECK, "Profile passes all checks", egui_theme_kit::success(ui));
                    }
                    if ui_kit::primary(ui, true, ph::FLOPPY_DISK, "Add to library").on_hover_text("Add the profile to the board library and save it").clicked() {
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

    /// Confirm-before-replace gate for a project pushed over the LAN.
    fn show_import_modal(&mut self, ctx: &egui::Context) {
        let Some(shown_id) = self.import.lock_shown() else { return };
        let pending = self.import.pending().expect("locked payload is present");
        let who = pending.from().unwrap_or("a device on your network").to_string();
        let incoming = pending.project().name.clone();
        let boards: Vec<String> = pending
            .project()
            .boards
            .iter()
            .map(|b| {
                format!(
                    "• {} — {} parts, {} wires",
                    b.name,
                    b.circuit.components.len(),
                    b.circuit.wires.len()
                )
            })
            .collect();
        let current = self.project.name.clone();
        let current_path = self.project_path.clone();
        let mut accept = false;
        let mut decline = false;
        egui::Modal::new(egui::Id::unique("incoming-project")).show(ctx, |ui| {
            ui.set_width(430.0);
            ui.label(RichText::new("Incoming project").strong().size(16.0));
            ui.label(format!("'{incoming}' was sent by {who}."));
            ui.add_space(4.0);
            for line in &boards {
                ui.label(RichText::new(line).small());
            }
            ui.separator();
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("⚠ This replaces '{current}'. Unsaved work in it is lost."),
            );
            if let Some(path) = &current_path {
                ui.label(RichText::new(format!("current file: {}", path.display())).small().weak());
            }
            ui.label(
                RichText::new("The imported project arrives unsaved — use Save as… to write it to disk.")
                    .small()
                    .weak(),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui_kit::button(ui, ph::X, "Decline").clicked() {
                    decline = true;
                }
                if ui_kit::primary(ui, true, ph::DOWNLOAD_SIMPLE, "Replace project").clicked() {
                    accept = true;
                }
            });
        });
        // Keyed on shown_id: only the payload this summary was drawn from lands.
        if accept && let Some(pending) = self.import.take(shown_id) {
            let project = pending.into_project();
            let name = project.name.clone();
            // No path: the phone can never overwrite a file on disk.
            self.adopt_project(project, None);
            self.log(format!("imported '{name}' from {who} — unsaved, use Save as…"));
        } else if decline {
            self.import.take(shown_id);
            self.log(format!("declined the project '{incoming}' from {who}"));
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
            let mut log = Vec::new();
            // "*" broadcasts to every other live board.
            if to.trim() == "*" {
                if !self.project.active_name().eq_ignore_ascii_case(&from) {
                    self.live.deliver_board_msg(&from, &text, &mut log);
                }
                for (id, bb) in self.background.iter_mut() {
                    let sender = self
                        .project
                        .boards
                        .iter()
                        .find(|b| b.id == *id)
                        .is_some_and(|b| b.name.eq_ignore_ascii_case(&from));
                    if !sender {
                        bb.live.deliver_board_msg(&from, &text, &mut log);
                    }
                }
                self.console.extend(log);
                continue;
            }
            let target = self
                .project
                .boards
                .iter()
                .position(|b| b.name.eq_ignore_ascii_case(to.trim()));
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
        self.docs.refresh(&self.project.circuit, &self.lib, &self.live.scripts);
        if ticked || bg_ticked || self.ai.busy || flashing {
            ctx.request_repaint_after(std::time::Duration::from_millis(30));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let m = egui_theme_kit::metrics(&ctx);
        let fill = ui.visuals().panel_fill;

        self.show_title_bar(ui);
        egui::Panel::top("board-strip")
            .exact_size(m.bar_h)
            .frame(egui::Frame::NONE.fill(fill))
            .show_separator_line(false)
            .show(ui, |ui| controls::bar(ui, BarKind::Tool, |ui| self.show_board_tabs(ui)));
        egui::Panel::bottom("status-bar")
            .exact_size(m.header_h)
            .frame(egui::Frame::NONE.fill(fill))
            .show_separator_line(false)
            .show(ui, |ui| controls::bar(ui, BarKind::Status, |ui| self.show_status_bar(ui)));
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui_theme_kit::tokens(&ctx).base))
            .show(ui, |ui| self.show_dock(ui));

        self.show_ai_window(&ctx);
        self.show_import_modal(&ctx);
        self.theme.show(&ctx);
        egui_icon_pack::gallery::show_window(&ctx, &crate::icons::PACK);
        if let Some(e) = chrome::take_save_error(&ctx) {
            self.log(format!("couldn't save the title bar setting: {e}"));
        }
        if self.chrome.frameless {
            window_edges(ui);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        crate::dock::save_layout(&self.dock, storage);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.theme.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::ImportGate;
    use wirelab_core::project::Project;

    /// The modal's exact sequence: draw the pending payload, then adopt by id.
    #[test]
    fn a_push_cannot_swap_the_payload_under_a_modal_on_screen() {
        let mut gate = ImportGate::default();
        gate.park(Project::new("A", "esp32-c3-devkitm-1"), Some("phone".into())).unwrap();

        // The modal draws A's summary.
        let shown = gate.lock_shown().unwrap();
        assert_eq!(gate.pending().unwrap().project().name, "A");

        // B arrives while the user is reading A.
        let err = gate
            .park(Project::new("B", "esp32-c3-devkitm-1"), Some("phone".into()))
            .unwrap_err();
        assert!(err.contains("'A'"), "the refusal names the held project: {err}");
        assert_eq!(gate.pending().unwrap().project().name, "A", "B did not displace A");

        // The click adopts the project whose summary was shown.
        let adopted = gate.take(shown).unwrap();
        assert_eq!(adopted.into_project().name, "A");
        assert!(gate.pending().is_none());

        // A stale id adopts nothing.
        gate.park(Project::new("C", "esp32-c3-devkitm-1"), None).unwrap();
        assert!(gate.take(shown).is_none());
        assert_eq!(gate.pending().unwrap().project().name, "C");
    }

    /// A payload not yet drawn is replaceable; the user has seen no summary.
    #[test]
    fn an_undrawn_push_is_replaced_not_refused() {
        let mut gate = ImportGate::default();
        gate.park(Project::new("A", "esp32-c3-devkitm-1"), None).unwrap();
        gate.park(Project::new("B", "esp32-c3-devkitm-1"), None).unwrap();
        assert_eq!(gate.pending().unwrap().project().name, "B");
    }

    #[test]
    fn shipped_theme_deserializes() {
        let path = super::find_assets_dir().join("theme.json");
        let theme = egui_theme_kit::persist::load_file(&path).expect("theme.json loads as a kit theme");
        assert!(theme.style.visuals.dark_mode);
        assert_eq!(theme.style.visuals.panel_fill, egui::Color32::from_rgb(0, 0, 0));
    }
}
