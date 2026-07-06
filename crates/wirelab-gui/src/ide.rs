//! The script IDE window: VSCode-shaped. Top toolbar (tabs + actions +
//! find), left hardware tree, right function list, collapsible bottom
//! (problems / console / guide) — and a central panel where ONLY the
//! text editor scrolls.

use egui::{Color32, RichText, ScrollArea};
use wirelab_core::circuit::CompId;

use crate::app::{FindKind, IdeBottomTab, IdeTab, Selection, WireLabApp};

impl WireLabApp {
    pub fn show_ide(&mut self, ui: &mut egui::Ui) {
        // Keep the active tab's buffer loaded before any panel touches it.
        if let Some(IdeTab::Comp(id)) = self.ide.tabs.get(self.ide.active).cloned() {
            self.ensure_buffer(id);
        }

        egui::Panel::top("ide-top").show(ui, |ui| {
            ui.add_space(2.0);
            self.ide_toolbar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::left("ide-tree")
            .resizable(true)
            .default_size(185.0)
            .show(ui, |ui| self.ide_tree(ui));
        egui::Panel::right("ide-side")
            .resizable(true)
            .default_size(185.0)
            .show(ui, |ui| self.ide_side(ui));
        egui::Panel::bottom("ide-status").show(ui, |ui| self.ide_status_bar(ui));
        if self.ide.bottom_open {
            egui::Panel::bottom("ide-bottom")
                .resizable(true)
                .default_size(150.0)
                .show(ui, |ui| self.ide_bottom(ui));
        }
        egui::CentralPanel::default().show(ui, |ui| self.ide_central(ui));
    }

    fn active_comp(&self) -> Option<CompId> {
        match self.ide.tabs.get(self.ide.active) {
            Some(IdeTab::Comp(id)) => Some(*id),
            _ => None,
        }
    }

    fn ensure_buffer(&mut self, id: CompId) {
        if self.script_ed.comp != Some(id) {
            self.stash_script_buffer();
            self.script_ed.comp = Some(id);
            let saved = self
                .project
                .circuit
                .components
                .get(&id)
                .and_then(|c| c.script.clone());
            self.script_ed.buffer =
                self.script_ed.stash.remove(&id).or(saved).unwrap_or_default();
        }
    }

    // ------------------------------------------------------------- top --

    fn ide_toolbar(&mut self, ui: &mut egui::Ui) {
        let names = wirelab_core::script::component_names(&self.project.circuit, &self.lib);
        let mut close: Option<usize> = None;
        ui.horizontal_wrapped(|ui| {
            let tabs = self.ide.tabs.clone();
            for (i, tab) in tabs.iter().enumerate() {
                let title = match tab {
                    IdeTab::Comp(id) => names.get(id).cloned().unwrap_or_else(|| "?".into()),
                    IdeTab::Info(k) => format!("ℹ {k}"),
                    IdeTab::Flow => format!("{} flow", egui_phosphor::regular::GRAPH),
                    IdeTab::Program => format!("{} rules", egui_phosphor::regular::LIST_CHECKS),
                };
                if ui.add(egui::Button::selectable(self.ide.active == i, title)).clicked() {
                    self.ide.active = i;
                }
                if ui.small_button("×").clicked() {
                    close = Some(i);
                }
            }
            if tabs.is_empty() {
                ui.label(RichText::new("open a component from the tree ⬅").weak());
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if matches!(self.ide.tabs.get(self.ide.active), Some(IdeTab::Flow)) {
                    if ui
                        .button("</> code")
                        .on_hover_text("the Rhai script generated from this graph")
                        .clicked()
                    {
                        self.flow_code_open = !self.flow_code_open;
                    }
                    let bottom_icon = if self.ide.bottom_open { "⬇" } else { "⬆" };
                    if ui.button(bottom_icon).clicked() {
                        self.ide.bottom_open = !self.ide.bottom_open;
                    }
                    let errs = self.flow_cache.2.len();
                    if errs > 0 {
                        ui.label(
                            RichText::new(format!("✖ {errs} problem{}", if errs == 1 { "" } else { "s" }))
                                .color(ui.visuals().error_fg_color),
                        );
                    } else if let Some(err) =
                        self.live.scripts.errors.get(&wirelab_core::script::FLOW_ID)
                    {
                        ui.label(RichText::new("✖ runtime error").color(ui.visuals().error_fg_color))
                            .on_hover_text(err.clone());
                    } else if self.project.flow.nodes.is_empty() {
                        ui.label(RichText::new("right-click the canvas to add nodes").weak());
                    } else {
                        ui.label(RichText::new("✔ live").color(Color32::from_rgb(90, 220, 120)));
                    }
                }
                if let Some(id) = self.active_comp() {
                    let script = self
                        .project
                        .circuit
                        .components
                        .get(&id)
                        .and_then(|c| c.script.clone());
                    let dirty = script.as_deref() != Some(self.script_ed.buffer.as_str());
                    let apply_hotkey = ui
                        .input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
                    if (ui
                        .add_enabled(dirty, egui::Button::new(RichText::new("▶ Apply").strong()))
                        .on_hover_text("compile & hot-swap (Ctrl+Enter)")
                        .clicked()
                        || (apply_hotkey && dirty))
                        && !self.script_ed.buffer.is_empty()
                    {
                        self.apply_active_script(id);
                    }
                    if ui.button("🔍").on_hover_text("find & replace (Ctrl+F)").clicked() {
                        self.ide.find_open = !self.ide.find_open;
                    }
                    if ui.button("📖").on_hover_text("scripting reference").clicked() {
                        self.docs_open = !self.docs_open;
                    }
                    let bottom_icon = if self.ide.bottom_open { "⬇" } else { "⬆" };
                    if ui
                        .button(bottom_icon)
                        .on_hover_text("toggle the problems / console strip")
                        .clicked()
                    {
                        self.ide.bottom_open = !self.ide.bottom_open;
                    }
                    if dirty {
                        ui.label(RichText::new("modified").color(ui.visuals().warn_fg_color));
                    } else if self.live.scripts.errors.contains_key(&id) {
                        ui.label(RichText::new("✖ error").color(ui.visuals().error_fg_color));
                    } else if self.live.scripts.has_script(id) {
                        ui.label(
                            RichText::new("✔ live").color(Color32::from_rgb(90, 220, 120)),
                        );
                    }
                }
            });
        });
        if self.ide.find_open {
            ui.horizontal(|ui| {
                ui.label("find");
                ui.add(egui::TextEdit::singleline(&mut self.ide.find).desired_width(140.0));
                let n = if self.ide.find.is_empty() {
                    0
                } else {
                    self.script_ed.buffer.matches(&self.ide.find).count()
                };
                ui.label(RichText::new(format!("{n}×")).weak());
                if ui.small_button("next").clicked() {
                    self.ide.pending_find = Some(FindKind::Next);
                }
                ui.label("replace");
                ui.add(egui::TextEdit::singleline(&mut self.ide.replace).desired_width(140.0));
                if ui.small_button("replace").clicked() {
                    self.ide.pending_find = Some(FindKind::Replace);
                }
                if ui.small_button("all").clicked() {
                    self.ide.pending_find = Some(FindKind::All);
                }
                if ui.small_button("✖").clicked() {
                    self.ide.find_open = false;
                }
            });
        }
    }

    fn apply_active_script(&mut self, id: CompId) {
        let names = wirelab_core::script::component_names(&self.project.circuit, &self.lib);
        let own = names.get(&id).cloned().unwrap_or_default();
        if let Some(c) = self.project.circuit.components.get_mut(&id) {
            c.script = Some(self.script_ed.buffer.clone());
            self.script_rev += 1;
            self.sync_scripts_offline();
            if let Some(err) = self.live.scripts.errors.get(&id) {
                self.console.push(format!("script error in `{own}`: {err}"));
            } else {
                self.console.push(format!("script for `{own}` applied"));
            }
        }
    }

    // ------------------------------------------------------------ left --

    fn ide_tree(&mut self, ui: &mut egui::Ui) {
        ScrollArea::vertical().id_salt("ide-tree-scroll").show(ui, |ui| {
            ui.label(RichText::new("BOARD").small().color(Color32::from_gray(120)));
            let active = self.project.active;
            let mut switch: Option<usize> = None;
            egui::ComboBox::from_id_salt("ide-board-pick")
                .width(ui.available_width() - 8.0)
                .selected_text(format!(
                    "{}{}",
                    if self.live.connected() { "● " } else { "" },
                    self.project.active_name()
                ))
                .show_ui(ui, |ui| {
                    for (i, tab) in self.project.boards.iter().enumerate() {
                        let is_live = if i == active {
                            self.live.connected()
                        } else {
                            self.background.get(&tab.id).is_some_and(|b| b.live.connected())
                        };
                        let label = format!(
                            "{}{}  ·  {}",
                            if is_live { "● " } else { "" },
                            tab.name,
                            tab.circuit.board_id
                        );
                        if ui.selectable_label(i == active, label).clicked() {
                            switch = Some(i);
                        }
                    }
                });
            if let Some(i) = switch {
                self.switch_board(i);
            }
            ui.add_space(6.0);
            ui.label(RichText::new("PROGRAM").small().color(Color32::from_gray(120)));
            let flow_n = self.project.flow.nodes.len();
            let flow_label = if flow_n == 0 {
                format!("{} flow graph", egui_phosphor::regular::GRAPH)
            } else {
                format!("{} flow graph ({flow_n})", egui_phosphor::regular::GRAPH)
            };
            if ui
                .add(egui::Button::selectable(
                    matches!(self.ide.tabs.get(self.ide.active), Some(IdeTab::Flow)),
                    flow_label,
                ))
                .on_hover_text("wire events to actions visually — compiles to a Rhai script")
                .clicked()
            {
                self.open_flow_tab();
            }
            let rule_n = self.project.program.rules.len();
            let rules_label = if rule_n == 0 {
                format!("{} rules program", egui_phosphor::regular::LIST_CHECKS)
            } else {
                format!("{} rules program ({rule_n})", egui_phosphor::regular::LIST_CHECKS)
            };
            if ui
                .add(egui::Button::selectable(
                    matches!(self.ide.tabs.get(self.ide.active), Some(IdeTab::Program)),
                    rules_label,
                ))
                .on_hover_text("trigger → action rules; runs with the Run button in the device bar")
                .clicked()
            {
                self.open_program_tab();
            }
            ui.add_space(6.0);
            ui.label(RichText::new("HARDWARE").small().color(Color32::from_gray(120)));
            let board_name = self
                .lib
                .board(&self.project.circuit.board_id)
                .map(|b| b.name.clone())
                .unwrap_or_default();
            if ui.small_button(format!("🕹 {board_name}")).clicked() {
                self.open_info_tab("board");
            }
            egui::CollapsingHeader::new(RichText::new("components").small())
                .default_open(true)
                .show(ui, |ui| {
                    let names = wirelab_core::script::component_names(
                        &self.project.circuit,
                        &self.lib,
                    );
                    let mut rows: Vec<(CompId, String, bool, bool)> = self
                        .project
                        .circuit
                        .components
                        .values()
                        .map(|c| {
                            (
                                c.id,
                                names.get(&c.id).cloned().unwrap_or_default(),
                                c.script.is_some(),
                                self.live.scripts.errors.contains_key(&c.id),
                            )
                        })
                        .collect();
                    rows.sort_by(|a, b| b.2.cmp(&a.2).then(a.1.cmp(&b.1)));
                    for (id, name, scripted, err) in rows {
                        let badge = if err {
                            " ✖"
                        } else if scripted {
                            " 📜"
                        } else {
                            ""
                        };
                        if ui.small_button(format!("{name}{badge}")).clicked() {
                            self.selection = Selection::Comp(id);
                            self.open_script_tab(id);
                        }
                    }
                });
            egui::CollapsingHeader::new(RichText::new("radios · planned").small())
                .default_open(false)
                .show(ui, |ui| {
                    for (label, key) in [
                        ("wifi 2.4 / 5 GHz", "wifi"),
                        ("bluetooth LE", "ble"),
                        ("802.15.4", "zigbee"),
                    ] {
                        if ui.small_button(label).clicked() {
                            self.open_info_tab(key);
                        }
                    }
                });
            egui::CollapsingHeader::new(RichText::new("buses").small())
                .default_open(false)
                .show(ui, |ui| {
                    for (label, key) in [
                        ("uart / serial", "uart"),
                        ("spi · planned", "spi"),
                        ("i2c · planned", "i2c"),
                    ] {
                        if ui.small_button(label).clicked() {
                            self.open_info_tab(key);
                        }
                    }
                });
        });
    }

    // ----------------------------------------------------------- right --

    fn ide_side(&mut self, ui: &mut egui::Ui) {
        ScrollArea::vertical().id_salt("ide-side-scroll").show(ui, |ui| {
            ui.label(RichText::new("OUTLINE").small().color(Color32::from_gray(120)));
            if self.active_comp().is_some() {
                let fns: Vec<(usize, String)> = self
                    .script_ed
                    .buffer
                    .lines()
                    .enumerate()
                    .filter_map(|(i, l)| {
                        l.trim_start().strip_prefix("fn ").map(|rest| {
                            let name: String = rest
                                .chars()
                                .take_while(|c| c.is_alphanumeric() || *c == '_')
                                .collect();
                            (i, name)
                        })
                    })
                    .collect();
                if fns.is_empty() {
                    ui.label(RichText::new("no functions yet").small().weak());
                }
                for (line, name) in fns {
                    if ui.small_button(format!("ƒ {name}")).clicked() {
                        self.ide.pending_jump = Some(line);
                    }
                }
            } else {
                ui.label(RichText::new("open a script to see its functions").small().weak());
            }

            ui.add_space(8.0);
            ui.separator();
            ui.label(RichText::new("SNIPPETS").small().color(Color32::from_gray(120)));
            let editing = self.active_comp().is_some();
            if !editing {
                ui.label(
                    RichText::new("everything this board can do —\nopen a script, click to insert")
                        .small()
                        .weak(),
                );
            }
            if let Some(board) = self.lib.board(&self.project.circuit.board_id) {
                for sn in crate::ide_snippets::snippets_for(board) {
                    let btn = ui.add_enabled(
                        editing,
                        egui::Button::new(RichText::new(sn.title)).small(),
                    );
                    if btn.clicked() {
                        self.ide.pending_snippet = Some(sn.code);
                    }
                    btn.on_hover_ui(|ui| {
                        ui.label(sn.blurb);
                        ui.add_space(4.0);
                        ui.label(RichText::new(sn.code).monospace().small().weak());
                    });
                }
            }
        });
    }

    // ---------------------------------------------------------- bottom --

    fn ide_bottom(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for (tab, label) in [
                (IdeBottomTab::Problems, format!("problems ({})", self.script_ed.lint.len())),
                (IdeBottomTab::Console, "console".to_string()),
                (IdeBottomTab::Guide, "guide".to_string()),
            ] {
                if ui
                    .add(egui::Button::selectable(self.ide.bottom_tab == tab, label))
                    .clicked()
                {
                    self.ide.bottom_tab = tab;
                }
            }
        });
        ui.separator();
        match self.ide.bottom_tab {
            IdeBottomTab::Problems if matches!(self.ide.tabs.get(self.ide.active), Some(IdeTab::Flow)) => {
                let error_color = ui.visuals().error_fg_color;
                ScrollArea::vertical().id_salt("ide-flow-problems").show(ui, |ui| {
                    let errs = self.flow_cache.2.clone();
                    let runtime = self.live.scripts.errors.get(&wirelab_core::script::FLOW_ID).cloned();
                    if errs.is_empty() && runtime.is_none() {
                        ui.label(
                            RichText::new("✔ no problems")
                                .small()
                                .color(Color32::from_rgb(90, 220, 120)),
                        );
                    }
                    for e in errs {
                        let title = e
                            .node
                            .and_then(|i| self.project.flow.nodes.get(i))
                            .map(|n| format!("[{}] ", n.kind.title()))
                            .unwrap_or_default();
                        ui.label(RichText::new(format!("{title}{}", e.msg)).small().color(error_color));
                    }
                    if let Some(err) = runtime {
                        ui.label(RichText::new(format!("runtime: {err}")).small().color(error_color));
                    }
                });
            }
            IdeBottomTab::Problems => {
                let error_color = ui.visuals().error_fg_color;
                ScrollArea::vertical().id_salt("ide-problems").show(ui, |ui| {
                    if self.script_ed.lint.is_empty() {
                        ui.label(
                            RichText::new("✔ no problems")
                                .small()
                                .color(Color32::from_rgb(90, 220, 120)),
                        );
                    }
                    let diags = self.script_ed.lint.clone();
                    for d in diags {
                        if ui
                            .link(
                                RichText::new(format!("{}:{}  {}", d.line, d.col, d.message))
                                    .small()
                                    .color(error_color),
                            )
                            .clicked()
                        {
                            self.ide.pending_jump = Some(d.line.saturating_sub(1));
                        }
                    }
                });
            }
            IdeBottomTab::Console => {
                self.show_console(ui);
            }
            IdeBottomTab::Guide => {
                ScrollArea::vertical().id_salt("ide-guide").show(ui, |ui| {
                    if let Some(id) = self.active_comp() {
                        let names = wirelab_core::script::component_names(
                            &self.project.circuit,
                            &self.lib,
                        );
                        let own = names.get(&id).cloned().unwrap_or_default();
                        if let Some(def) = self
                            .project
                            .circuit
                            .components
                            .get(&id)
                            .and_then(|c| self.lib.component(&c.def_id))
                            .cloned()
                        {
                            self.show_script_cheat_sheet(ui, &def.sim, &own, &names, id);
                        }
                    }
                });
            }
        }
    }

    /// Floating read-only view of the Rhai generated from the flow graph.
    pub(crate) fn show_flow_code_window(&mut self, ctx: &egui::Context) {
        if !self.flow_code_open {
            return;
        }
        let mut open = self.flow_code_open;
        egui::Window::new("generated flow script")
            .open(&mut open)
            .default_size([460.0, 420.0])
            .show(ctx, |ui| {
                let mut code = self
                    .flow_cache
                    .1
                    .clone()
                    .unwrap_or_else(|| "// the graph is empty or has problems".into());
                ScrollArea::vertical().id_salt("flow-code").show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut code)
                            .font(egui::TextStyle::Monospace)
                            .interactive(false)
                            .desired_width(f32::INFINITY),
                    );
                });
            });
        self.flow_code_open = open;
    }

    /// VSCode-style one-liner: board · link · script state · problems · caret.
    fn ide_status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let live = self.live.connected();
            let (dot, color) = if live {
                ("●", Color32::from_rgb(90, 220, 120))
            } else {
                ("○", Color32::from_gray(120))
            };
            ui.label(RichText::new(dot).color(color).small());
            ui.label(
                RichText::new(format!(
                    "{} · {}",
                    self.project.active_name(),
                    self.project.circuit.board_id
                ))
                .small(),
            );
            if live {
                let backend = match self.live.backend {
                    crate::live::Backend::Simulator => "sim",
                    crate::live::Backend::Serial => "usb",
                    crate::live::Backend::Tcp => "wi-fi",
                };
                ui.label(RichText::new(backend).small().weak());
            }
            ui.separator();
            let problems = self.script_ed.lint.len() + self.flow_cache.2.len();
            if problems > 0 {
                ui.label(
                    RichText::new(format!("✖ {problems}"))
                        .small()
                        .color(ui.visuals().error_fg_color),
                );
            } else {
                ui.label(RichText::new("✔").small().color(Color32::from_rgb(90, 220, 120)));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.active_comp().is_some() {
                    let (l, c) = self.ide.cursor;
                    ui.label(RichText::new(format!("Ln {l}, Col {c}")).small().weak());
                    let hints = if self.ide.hints_on { "hints: on" } else { "hints: off" };
                    if ui
                        .add(egui::Button::new(RichText::new(hints).small()).frame(false))
                        .on_hover_text("inline type hints")
                        .clicked()
                    {
                        self.ide.hints_on = !self.ide.hints_on;
                    }
                }
                ui.label(RichText::new("Rhai").small().weak());
            });
        });
    }

    // --------------------------------------------------------- central --

    fn ide_central(&mut self, ui: &mut egui::Ui) {
        match self.ide.tabs.get(self.ide.active).cloned() {
            Some(IdeTab::Flow) => {
                self.show_flow_editor(ui);
                self.show_flow_code_window(ui.ctx());
            }
            Some(IdeTab::Program) => {
                ScrollArea::vertical().id_salt("ide-program").show(ui, |ui| {
                    self.show_program(ui);
                });
            }
            Some(IdeTab::Info(key)) => {
                ScrollArea::vertical().id_salt("ide-info").show(ui, |ui| {
                    ui.label(crate::panels_info::ide_info(self, key));
                });
            }
            Some(IdeTab::Comp(id)) => {
                if !self.project.circuit.components.contains_key(&id) {
                    ui.label("component was removed");
                    return;
                }
                let has_script = self
                    .project
                    .circuit
                    .components
                    .get(&id)
                    .is_some_and(|c| c.script.is_some());
                if !has_script && self.script_ed.buffer.is_empty() {
                    ui.add_space(12.0);
                    if ui.button("✚ Attach script").clicked() {
                        self.attach_template_script(id);
                        self.ensure_buffer(id);
                    }
                    return;
                }
                let avail = ui.available_height();
                ScrollArea::vertical()
                    .id_salt(("ide-editor", id))
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.ide_editor_text(ui, id, avail));
            }
            None => {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new(
                            "WireLab Script IDE\n\nPick a component from the hardware tree, \
                             or click 📜 on any part on the canvas.",
                        )
                        .color(Color32::from_gray(140)),
                    );
                });
            }
        }
    }

    /// The text editor itself — the only thing that scrolls.
    fn ide_editor_text(&mut self, ui: &mut egui::Ui, _id: CompId, panel_height: f32) {
        // A snippet click appends its template and jumps to it.
        if let Some(code) = self.ide.pending_snippet.take() {
            if !self.script_ed.buffer.is_empty() {
                while !self.script_ed.buffer.ends_with("\n\n") {
                    self.script_ed.buffer.push('\n');
                }
            }
            let line = self.script_ed.buffer.lines().count();
            self.script_ed.buffer.push_str(code);
            self.ide.pending_jump = Some(line);
        }

        let names = wirelab_core::script::component_names(&self.project.circuit, &self.lib);

        // Live lint on change.
        {
            use std::hash::{Hash, Hasher};
            let mut sorted: Vec<String> = names.values().cloned().collect();
            sorted.sort();
            let mut h = std::hash::DefaultHasher::new();
            self.script_ed.buffer.hash(&mut h);
            sorted.hash(&mut h);
            let hash = h.finish();
            if self.script_ed.lint_hash != hash {
                self.script_ed.lint_hash = hash;
                self.linter.set_api(&sorted);
                self.script_ed.lint = self.linter.lint(&self.script_ed.buffer);
            }
        }

        // Ctrl+F here too (the toolbar row lives in the top panel).
        if ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
            self.ide.find_open = true;
        }

        // Enter keeps indentation; consumed pre-editor.
        let editor_id = egui::Id::new("wirelab-script-editor");
        let mut auto_indent = false;
        if self.script_ed.completion.is_none() && ui.ctx().memory(|m| m.has_focus(editor_id)) {
            ui.input_mut(|i| {
                auto_indent = i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
            });
        }

        let error_color = ui.visuals().error_fg_color;
        let mut accept: Option<usize> = None;
        if self.script_ed.completion.is_some() {
            use egui::{Key, Modifiers};
            let (mut up, mut down, mut esc, mut tab, mut enter) =
                (false, false, false, false, false);
            ui.input_mut(|i| {
                up = i.consume_key(Modifiers::NONE, Key::ArrowUp);
                down = i.consume_key(Modifiers::NONE, Key::ArrowDown);
                esc = i.consume_key(Modifiers::NONE, Key::Escape);
                tab = i.consume_key(Modifiers::NONE, Key::Tab);
                enter = i.consume_key(Modifiers::NONE, Key::Enter);
            });
            if esc {
                self.script_ed.completion = None;
            } else if let Some(c) = &mut self.script_ed.completion {
                if up {
                    c.selected = c.selected.saturating_sub(1);
                }
                if down {
                    c.selected = (c.selected + 1).min(c.items.len().saturating_sub(1));
                }
                if tab || enter {
                    accept = Some(c.selected);
                }
            }
        }

        let squiggle: Vec<(usize, usize)> =
            self.script_ed.lint.iter().map(|d| (d.start, d.end)).collect();
        let matches: Vec<(usize, usize)> = if self.ide.find_open && !self.ide.find.is_empty() {
            self.script_ed
                .buffer
                .match_indices(&self.ide.find)
                .map(|(i, m)| (i, i + m.len()))
                .collect()
        } else {
            Vec::new()
        };
        let match_bg = ui.visuals().selection.bg_fill;
        let theme =
            egui_extras::syntax_highlighting::CodeTheme::from_memory(ui.ctx(), ui.style());
        let mut layouter = |ui: &egui::Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
            let mut job = egui_extras::syntax_highlighting::highlight(
                ui.ctx(),
                ui.style(),
                &theme,
                buf.as_str(),
                "rs",
            );
            for section in &mut job.sections {
                let r = &section.byte_range;
                if squiggle.iter().any(|(s, e)| r.start.0 < *e && *s < r.end.0) {
                    section.format.underline = egui::Stroke::new(1.5, error_color);
                }
                if matches.iter().any(|(s, e)| r.start.0 < *e && *s < r.end.0) {
                    section.format.background = match_bg;
                }
            }
            job.wrap.max_width = wrap_width;
            ui.fonts_mut(|f| f.layout_job(job))
        };
        // Fill the whole panel even for short scripts.
        let row_h = ui.text_style_height(&egui::TextStyle::Monospace).max(1.0);
        let rows = ((panel_height / row_h).ceil() as usize).max(8);
        let out = egui::TextEdit::multiline(&mut self.script_ed.buffer)
            .id(editor_id)
            .code_editor()
            .desired_width(f32::INFINITY)
            .desired_rows(rows)
            .layouter(&mut layouter)
            .show(ui);

        // Caret position for the status bar.
        if let Some(cr) = out.cursor_range {
            let ci = cr.primary.index.0;
            let (mut l, mut c) = (1usize, 1usize);
            for (n, ch) in self.script_ed.buffer.chars().enumerate() {
                if n >= ci {
                    break;
                }
                if ch == '\n' {
                    l += 1;
                    c = 1;
                } else {
                    c += 1;
                }
            }
            self.ide.cursor = (l, c);
        }

        // Inline type hints, painted as ghost text past each line's end.
        if self.ide.hints_on {
            let painter = ui.painter().with_clip_rect(out.text_clip_rect);
            let font = egui::FontId::monospace(
                ui.text_style_height(&egui::TextStyle::Monospace) * 0.75,
            );
            let color = ui.visuals().weak_text_color().gamma_multiply(0.75);
            let lines: Vec<&str> = self.script_ed.buffer.lines().collect();
            let mut line = 0usize;
            let rows = &out.galley.rows;
            for (ri, placed) in rows.iter().enumerate() {
                let line_ends_here = placed.ends_with_newline || ri + 1 == rows.len();
                if line_ends_here {
                    if let Some(text) = lines.get(line)
                        && let Some(hint) = line_hint(text)
                    {
                        let pos = out.galley_pos
                            + placed.pos.to_vec2()
                            + egui::vec2(placed.row.size.x + 14.0, 1.0);
                        painter.text(pos, egui::Align2::LEFT_TOP, hint, font.clone(), color);
                    }
                    line += 1;
                }
            }
        }

        // Requests from the side panels.
        let pending_jump = self.ide.pending_jump.take();
        let pending_find = self.ide.pending_find.take();
        if pending_jump.is_some() || pending_find.is_some() {
            let caret_to = |ui: &egui::Ui, state: &egui::text_edit::TextEditState, ci: usize| {
                let mut state = state.clone();
                state.cursor.set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(ci),
                )));
                state.store(ui.ctx(), editor_id);
                ui.ctx().memory_mut(|m| m.request_focus(editor_id));
            };
            if let Some(line) = pending_jump {
                let ci = self
                    .script_ed
                    .buffer
                    .split('\n')
                    .take(line)
                    .map(|l| l.chars().count() + 1)
                    .sum::<usize>();
                caret_to(ui, &out.state, ci);
            } else if let Some(kind) = pending_find
                && !self.ide.find.is_empty()
            {
                let find = self.ide.find.clone();
                let cur = out
                    .cursor_range
                    .map(|c| c.primary.index.0)
                    .unwrap_or(0)
                    .min(self.script_ed.buffer.chars().count());
                let cur_byte = self
                    .script_ed
                    .buffer
                    .char_indices()
                    .nth(cur)
                    .map(|(b, _)| b)
                    .unwrap_or(self.script_ed.buffer.len());
                match kind {
                    FindKind::Next => {
                        let hit = self.script_ed.buffer[cur_byte..]
                            .find(&find)
                            .map(|o| cur_byte + o)
                            .or_else(|| self.script_ed.buffer.find(&find));
                        if let Some(b) = hit {
                            let ci = self.script_ed.buffer[..b].chars().count()
                                + find.chars().count();
                            caret_to(ui, &out.state, ci);
                        }
                    }
                    FindKind::Replace => {
                        let hit = self.script_ed.buffer[cur_byte..]
                            .find(&find)
                            .map(|o| cur_byte + o)
                            .or_else(|| self.script_ed.buffer.find(&find));
                        if let Some(b) = hit {
                            let repl = self.ide.replace.clone();
                            self.script_ed.buffer.replace_range(b..b + find.len(), &repl);
                            let ci = self.script_ed.buffer[..b].chars().count()
                                + repl.chars().count();
                            caret_to(ui, &out.state, ci);
                        }
                    }
                    FindKind::All => {
                        let repl = self.ide.replace.clone();
                        self.script_ed.buffer = self.script_ed.buffer.replace(&find, &repl);
                    }
                }
            }
            ui.ctx().request_repaint();
            return;
        }

        // Auto-indent.
        if auto_indent
            && let Some(cr) = out.cursor_range
        {
            let chars: Vec<char> = self.script_ed.buffer.chars().collect();
            let (lo, hi) = {
                let (p, q) = (cr.primary.index.0, cr.secondary.index.0);
                (p.min(q).min(chars.len()), p.max(q).min(chars.len()))
            };
            let mut ls = lo;
            while ls > 0 && chars[ls - 1] != '\n' {
                ls -= 1;
            }
            let indent: String =
                chars[ls..lo].iter().take_while(|c| **c == ' ' || **c == '\t').collect();
            let line: String = chars[ls..lo].iter().collect();
            let extra = if line.trim_end().ends_with('{') { "    " } else { "" };
            let insert = format!("\n{indent}{extra}");
            let byte = |ci: usize| {
                self.script_ed
                    .buffer
                    .char_indices()
                    .nth(ci)
                    .map(|(b, _)| b)
                    .unwrap_or(self.script_ed.buffer.len())
            };
            let (blo, bhi) = (byte(lo), byte(hi));
            self.script_ed.buffer.replace_range(blo..bhi, &insert);
            let caret = lo + insert.chars().count();
            let mut state = out.state.clone();
            state.cursor.set_char_range(Some(egui::text::CCursorRange::one(
                egui::text::CCursor::new(caret),
            )));
            state.store(ui.ctx(), editor_id);
            ui.ctx().request_repaint();
            return;
        }

        self.script_completion_and_hover(ui, &names, editor_id, accept, out);
    }
}

/// Ghost-text hint for one source line: callback parameter types on `fn`
/// lines, inferred value types on `let` bindings.
fn line_hint(line: &str) -> Option<String> {
    let t = line.trim();
    const CALLBACKS: &[(&str, &str)] = &[
        ("fn on_change(", "on: bool"),
        ("fn on_reading(", "mv: int (millivolts)"),
        ("fn on_tick(", "dt: int (ms)"),
        ("fn on_pin(", "gpio: int, high: bool"),
        ("fn on_uart(", "line: string"),
        ("fn on_board_msg(", "from: string, text: string"),
        ("fn on_i2c(", "addr: int, data: [int]"),
        ("fn on_spi(", "data: [int]"),
    ];
    for (prefix, hint) in CALLBACKS {
        if t.starts_with(prefix) {
            return Some(hint.to_string());
        }
    }
    let rest = t.strip_prefix("let ").or_else(|| t.strip_prefix("const "))?;
    let (_, expr) = rest.split_once('=')?;
    infer_type(expr).map(|ty| format!(": {ty}"))
}

/// Cheap, literal-and-known-call type inference; None when unsure.
fn infer_type(expr: &str) -> Option<&'static str> {
    let e = expr.trim().trim_end_matches(';').trim();
    if e.is_empty() {
        return None;
    }
    const CALLS: &[(&str, &'static str)] = &[
        ("pin(", "Pin"),
        ("comp(", "Component"),
        ("millis()", "int (ms)"),
        ("chip()", "string"),
        ("board_has(", "bool"),
    ];
    for (call, ty) in CALLS {
        if e.starts_with(call) && !e.contains('.') {
            return Some(ty);
        }
    }
    if e.ends_with(".millivolts()") {
        return Some("int (mV)");
    }
    if e.ends_with(".is_on()") || e.ends_with(".is_pressed()") || e.ends_with(".is_high()") {
        return Some("bool");
    }
    if e.starts_with('"') || e.starts_with('`') {
        return Some("string");
    }
    if e == "true" || e == "false" || e.starts_with('!') {
        return Some("bool");
    }
    if e.starts_with('[') {
        return Some("array");
    }
    if e.starts_with("#{") {
        return Some("map");
    }
    if e.starts_with("||") || (e.starts_with('|') && e.contains("| ")) {
        return Some("closure");
    }
    // Numeric literals, incl. simple arithmetic over them.
    let numeric = e
        .chars()
        .all(|c| c.is_ascii_digit() || " .+-*/%()_".contains(c));
    if numeric && e.chars().any(|c| c.is_ascii_digit()) {
        return Some(if e.contains('.') { "float" } else { "int" });
    }
    None
}

#[cfg(test)]
mod hint_tests {
    use super::*;

    #[test]
    fn infers_literals_calls_and_callbacks() {
        assert_eq!(line_hint("let x = 42;").as_deref(), Some(": int"));
        assert_eq!(line_hint("let y = 1.5;").as_deref(), Some(": float"));
        assert_eq!(line_hint("let s = \"hi\";").as_deref(), Some(": string"));
        assert_eq!(line_hint("let t = `n=${n}`;").as_deref(), Some(": string"));
        assert_eq!(line_hint("let ok = !btn.is_on();").as_deref(), Some(": bool"));
        assert_eq!(line_hint("let p = pin(4);").as_deref(), Some(": Pin"));
        assert_eq!(line_hint("let mv = pot.millivolts();").as_deref(), Some(": int (mV)"));
        assert_eq!(line_hint("let a = [1, 2];").as_deref(), Some(": array"));
        assert_eq!(line_hint("let m = #{ a: 1 };").as_deref(), Some(": map"));
        assert_eq!(line_hint("let t = millis() + 500;").as_deref(), Some(": int (ms)"));
        assert_eq!(
            line_hint("fn on_reading(mv) {").as_deref(),
            Some("mv: int (millivolts)")
        );
        assert_eq!(
            line_hint("fn on_board_msg(from, text) {").as_deref(),
            Some("from: string, text: string")
        );
        assert_eq!(line_hint("red_led.toggle();"), None);
        assert_eq!(line_hint("let z = mystery();"), None);
    }
}
