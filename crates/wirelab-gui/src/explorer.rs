//! The explorer tab: the program, every part with what its script does, and the board's sheets.

use egui::{ScrollArea, Ui};
use egui_theme_kit::controls::{ListItem, Section};
use wirelab_core::circuit::CompId;
use wirelab_core::program::{Action, Trigger};

use crate::app::{Selection, WireLabApp};
use crate::dock::Tab;
use crate::rhai_lint::Severity;
use crate::ui_kit::{self, ph};

/// A handler counts as active for this long after it ran, in ms.
const ACTIVE_MS: u64 = 1500;

impl WireLabApp {
    pub fn show_explorer(&mut self, ui: &mut Ui) {
        ScrollArea::vertical()
            .id_salt("explorer")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                self.explorer_program(ui);
                self.explorer_scripts(ui);
                self.explorer_parts(ui);
                self.explorer_hardware(ui);
            });
    }

    fn explorer_program(&mut self, ui: &mut Ui) {
        if !Section::new("explorer-program", "Program")
            .icon(ui_kit::kit(ph::FLOW_ARROW))
            .default_open(true)
            .header(ui)
        {
            return;
        }
        let nodes = self.project.flow.nodes.len();
        let flow_problems = self.flow_cache.2.len();
        let flow_detail = match (nodes, flow_problems) {
            (0, _) => "empty".to_string(),
            (n, 0) => format!("{n} nodes"),
            (n, e) => format!("{n} nodes · {e} problems"),
        };
        let mut flow = ListItem::new("Flow graph")
            .icon(ui_kit::kit(ph::FLOW_ARROW))
            .detail(&flow_detail)
            .selected(self.is_tab_open(&Tab::Flow));
        if flow_problems > 0 {
            flow = flow.trailing(ui_kit::kit(ph::WARNING_CIRCLE), ui.visuals().error_fg_color);
        }
        if flow.show(ui).clicked() {
            self.open_tab(Tab::Flow);
        }
        let rules = self.project.program.rules.len();
        let rules_detail = match (rules, self.live.program_running) {
            (0, _) => "none yet".to_string(),
            (n, true) => format!("{n} rules · running"),
            (n, false) => format!("{n} rules"),
        };
        let mut row = ListItem::new("Rules")
            .icon(ui_kit::kit(ph::LIST_CHECKS))
            .detail(&rules_detail)
            .selected(self.is_tab_open(&Tab::Rules));
        if self.live.program_running {
            row = row.trailing(ui_kit::kit(ph::PULSE), egui_theme_kit::success(ui));
        }
        if row.show(ui).clicked() {
            self.open_tab(Tab::Rules);
        }
    }

    /// Rules whose trigger or actions name `id`.
    fn rules_for(&self, id: CompId) -> usize {
        self.project
            .program
            .rules
            .iter()
            .filter(|r| {
                matches!(&r.trigger, Trigger::CompEvent { comp, .. } if *comp == id)
                    || r.actions
                        .iter()
                        .any(|a| matches!(a, Action::CompAction { comp, .. } if *comp == id))
            })
            .count()
    }

    fn explorer_scripts(&mut self, ui: &mut Ui) {
        let names = wirelab_core::script::component_names(&self.project.circuit, &self.lib);
        let mut rows: Vec<(CompId, String)> = self
            .project
            .circuit
            .components
            .values()
            .filter(|c| c.script.is_some() || self.docs.is_dirty(c.id, &self.project.circuit))
            .map(|c| (c.id, names.get(&c.id).cloned().unwrap_or_default()))
            .collect();
        rows.sort_by(|a, b| a.1.cmp(&b.1));
        let count = rows.len().to_string();
        let open = Section::new("explorer-scripts", "Scripts")
            .icon(ui_kit::kit(ph::FILE_CODE))
            .suffix(&count, None)
            .default_open(true)
            .header(ui);
        if !open {
            return;
        }
        if rows.is_empty() {
            ui.label(
                egui::RichText::new("No part has a script yet. Select a part and attach one.")
                    .weak(),
            );
            return;
        }
        let now = self.live.now_ms();
        for (id, name) in rows {
            let Some(def) = self
                .project
                .circuit
                .components
                .get(&id)
                .and_then(|c| self.lib.component(&c.def_id))
                .cloned()
            else {
                continue;
            };
            let handlers = match self.live.scripts.handlers(id) {
                h if !h.is_empty() => h,
                _ => buffer_functions(self.docs.get(id).map_or_else(
                    || {
                        self.project
                            .circuit
                            .components
                            .get(&id)
                            .and_then(|c| c.script.clone())
                            .unwrap_or_default()
                    },
                    |d| d.buffer.clone(),
                )),
            };
            let fired = self.live.scripts.last_fired.get(&id).cloned();
            let active = fired
                .as_ref()
                .is_some_and(|(_, at)| now.saturating_sub(*at) < ACTIVE_MS);
            let detail = match &fired {
                Some((f, at)) if self.live.connected() => {
                    format!("{f} · {:.1} s ago", now.saturating_sub(*at) as f32 / 1000.0)
                }
                _ => handlers
                    .iter()
                    .filter(|h| h.starts_with("on_"))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" · "),
            };
            let errors = self
                .docs
                .diags(id)
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .count();
            let (state_icon, _, mut tone) = self.script_state(ui, id);
            let state_icon = if errors > 0 { ph::X_CIRCLE } else { state_icon };
            if errors > 0 {
                tone = ui.visuals().error_fg_color;
            }
            let selected = self.selection.contains_comp(id)
                || self.is_tab_open(&Tab::Script(id)) && self.selection == Selection::None;
            let tint = crate::icons::part_tint(ui, &def);
            let item = ListItem::new(&name)
                .icon(ui_kit::kit(crate::icons::part(&def)))
                .icon_tone(tint)
                .detail(&detail)
                .selected(selected)
                .unread(active)
                .trailing(ui_kit::kit(state_icon), tone);
            let mut jump: Option<String> = None;
            let (row, _) = item.show_node(ui, ("explorer-script", id), false, |ui| {
                for h in &handlers {
                    let callback = h.starts_with("on_");
                    let ran = fired.as_ref().filter(|(f, _)| f == h).map(|(_, at)| {
                        format!("{:.1} s ago", now.saturating_sub(*at) as f32 / 1000.0)
                    });
                    let child = ListItem::new(h)
                        .icon(ui_kit::kit(if callback {
                            ph::LIGHTNING
                        } else {
                            ph::BRACKETS_CURLY
                        }))
                        .detail(ran.as_deref().unwrap_or(""));
                    if child.show(ui).clicked() {
                        jump = Some(h.clone());
                    }
                }
            });
            let rules = self.rules_for(id);
            let row = if rules > 0 {
                row.on_hover_text(format!("also used by {rules} rule(s)"))
            } else {
                row
            };
            if row.clicked() {
                self.selection = Selection::Comp(id);
                self.open_tab(Tab::Script(id));
            }
            if let Some(h) = jump {
                self.open_tab(Tab::Script(id));
                let doc = self.docs.doc(id, &self.project.circuit);
                doc.pending_jump = doc.buffer.lines().position(|l| {
                    l.trim_start().strip_prefix("fn ").is_some_and(|r| {
                        r.starts_with(h.as_str()) && r[h.len()..].starts_with(['(', ' '])
                    })
                });
            }
            if active {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(250));
            }
        }
    }

    fn explorer_parts(&mut self, ui: &mut Ui) {
        let names = wirelab_core::script::component_names(&self.project.circuit, &self.lib);
        let mut rows: Vec<(CompId, String)> = self
            .project
            .circuit
            .components
            .values()
            .filter(|c| c.script.is_none() && !self.docs.is_dirty(c.id, &self.project.circuit))
            .filter(|c| {
                self.lib
                    .component(&c.def_id)
                    .is_some_and(|d| d.visual.shape != wirelab_core::component::Shape::Dot)
            })
            .map(|c| (c.id, names.get(&c.id).cloned().unwrap_or_default()))
            .collect();
        rows.sort_by(|a, b| a.1.cmp(&b.1));
        let count = rows.len().to_string();
        let open = Section::new("explorer-parts", "Other parts")
            .icon(ui_kit::kit(ph::PUZZLE_PIECE))
            .suffix(&count, None)
            .default_open(true)
            .header(ui);
        if !open {
            return;
        }
        for (id, name) in rows {
            let Some(def) = self
                .project
                .circuit
                .components
                .get(&id)
                .and_then(|c| self.lib.component(&c.def_id))
                .cloned()
            else {
                continue;
            };
            let rules = self.rules_for(id);
            let detail = match (self.cache.bindings.gpio_of(id), rules) {
                (Some(g), 0) => format!("GPIO{g}"),
                (Some(g), n) => format!("GPIO{g} · {n} rules"),
                (None, 0) => String::new(),
                (None, n) => format!("{n} rules"),
            };
            let tint = crate::icons::part_tint(ui, &def);
            let (row, attach) = ListItem::new(&name)
                .icon(ui_kit::kit(crate::icons::part(&def)))
                .icon_tone(tint)
                .detail(&detail)
                .selected(self.selection.contains_comp(id))
                .action(ui_kit::kit(ph::FILE_PLUS), "Attach a script")
                .show_with_action(ui);
            let row = row.on_hover_text(&def.name);
            if row.clicked() {
                self.selection = Selection::Comp(id);
            }
            if attach.is_some_and(|a| a.clicked()) {
                self.selection = Selection::Comp(id);
                self.attach_template_script(id);
            }
        }
    }

    fn explorer_hardware(&mut self, ui: &mut Ui) {
        if !Section::new("explorer-hardware", "Hardware")
            .icon(ui_kit::kit(ph::CPU))
            .default_open(false)
            .header(ui)
        {
            return;
        }
        let board = self
            .lib
            .board(&self.project.circuit.board_id)
            .map(|b| (b.name.clone(), b.chip.name().to_string()));
        if let Some((name, chip)) = board
            && ListItem::new(&name)
                .icon(ui_kit::kit(ph::CIRCUITRY))
                .detail(&chip)
                .show(ui)
                .clicked()
        {
            self.open_tab(Tab::Info("board".into()));
        }
        for (key, icon, detail) in [
            ("uart", ph::USB, "serial · scriptable"),
            ("spi", ph::ARROWS_LEFT_RIGHT, "displays · scriptable"),
            ("i2c", ph::GIT_BRANCH, "sensors · scriptable"),
            ("wifi", ph::WIFI_HIGH, "link + http_get"),
            ("ble", ph::BLUETOOTH, "planned"),
            ("zigbee", ph::BROADCAST, "planned"),
        ] {
            if ListItem::new(crate::panels_info::title(key))
                .icon(ui_kit::kit(icon))
                .detail(detail)
                .show(ui)
                .clicked()
            {
                self.open_tab(Tab::Info(key.into()));
            }
        }
    }
}

/// Function names declared in a script's source, in order.
fn buffer_functions(src: String) -> Vec<String> {
    src.lines()
        .filter_map(|l| {
            let rest = l.trim_start().strip_prefix("fn ")?;
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}
