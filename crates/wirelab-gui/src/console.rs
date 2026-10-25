//! The console tab (app and script log) and the problems tab (diagnostics across every script).

use egui::text::LayoutJob;
use egui::{Color32, FontId, Frame, Panel, ScrollArea, TextFormat, Ui};
use egui_theme_kit::controls::{self, BarKind, ListItem, Section};

use crate::app::WireLabApp;
use crate::dock::Tab;
use crate::rhai_lint::Severity;
use crate::ui_kit::{self, ph};

/// How a console line reads: errors, warnings, script output, or plain.
fn line_tone(line: &str, ui: &Ui) -> Option<Color32> {
    let lower = line.to_ascii_lowercase();
    if lower.contains("error") || lower.contains("failed") || lower.contains("refused") {
        Some(ui.visuals().error_fg_color)
    } else if lower.contains("warn") || lower.contains("dropped") {
        Some(ui.visuals().warn_fg_color)
    } else {
        None
    }
}

impl WireLabApp {
    pub fn show_console_tab(&mut self, ui: &mut Ui) {
        let m = egui_theme_kit::metrics(ui.ctx());
        Panel::top("console-toolbar")
            .exact_size(m.bar_h)
            .frame(Frame::NONE.fill(ui.visuals().panel_fill))
            .show_separator_line(false)
            .show(ui, |ui| {
                controls::bar(ui, BarKind::Tool, |ui| {
                    if ui_kit::tool(ui, ph::ERASER, "Clear").clicked() {
                        self.console.clear();
                    }
                    if ui_kit::tool(ui, ph::COPY, "Copy the console").clicked() {
                        ui.ctx().copy_text(self.console.join("\n"));
                    }
                    if ui_kit::tool_toggle(
                        ui,
                        self.console_follow,
                        ph::ARROW_LINE_DOWN,
                        "Follow new output",
                    )
                    .clicked()
                    {
                        self.console_follow = !self.console_follow;
                    }
                    controls::search_field(ui, &mut self.console_filter, "Filter", 200.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!("{} lines", self.console.len())).weak(),
                        );
                    });
                });
            });
        let mono = egui::TextStyle::Monospace.resolve(ui.style());
        let filter = self.console_filter.to_lowercase();
        let primary = egui_theme_kit::primary(ui);
        let text = ui.visuals().text_color();
        ScrollArea::vertical()
            .id_salt("console-scroll")
            .auto_shrink([false, false])
            .stick_to_bottom(self.console_follow)
            .show(ui, |ui| {
                for line in self
                    .console
                    .iter()
                    .filter(|l| filter.is_empty() || l.to_lowercase().contains(&filter))
                {
                    let tone = line_tone(line, ui).unwrap_or(text);
                    let mut job = LayoutJob::default();
                    let fmt = |color| TextFormat {
                        font_id: FontId::monospace(mono.size),
                        color,
                        ..Default::default()
                    };
                    match line.strip_prefix('[').and_then(|r| r.split_once("] ")) {
                        Some((who, rest)) => {
                            job.append(&format!("[{who}] "), 0.0, fmt(primary));
                            job.append(rest, 0.0, fmt(tone));
                        }
                        None => job.append(line, 0.0, fmt(tone)),
                    }
                    ui.label(job);
                }
            });
    }

    pub fn show_problems(&mut self, ui: &mut Ui) {
        let (errors, warnings) = self.docs.counts();
        let flow_errors = self.flow_cache.2.len();
        let flow_runtime = self
            .live
            .scripts
            .errors
            .get(&wirelab_core::script::FLOW_ID)
            .cloned();
        if errors + warnings + flow_errors == 0 && flow_runtime.is_none() {
            controls::empty_state(
                ui,
                ui_kit::kit(ph::SEAL_CHECK),
                "No problems",
                "Every script and the flow graph compile cleanly.",
                &[],
            );
            return;
        }
        let names = wirelab_core::script::component_names(&self.project.circuit, &self.lib);
        let (error_tone, warn_tone) = (ui.visuals().error_fg_color, ui.visuals().warn_fg_color);
        ScrollArea::vertical()
            .id_salt("problems")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for id in self.docs.with_problems() {
                    let diags = self.docs.diags(id).to_vec();
                    let name = names.get(&id).cloned().unwrap_or_default();
                    let count = diags.len().to_string();
                    let mut section = Section::new(("problems", id), &name)
                        .suffix(&count, None)
                        .default_open(true);
                    if let Some(def) = self
                        .project
                        .circuit
                        .components
                        .get(&id)
                        .and_then(|c| self.lib.component(&c.def_id))
                    {
                        section = section
                            .icon(ui_kit::kit(crate::icons::part(def)))
                            .icon_tone(crate::icons::part_tint(ui, def));
                    }
                    if !section.header(ui) {
                        continue;
                    }
                    for d in diags {
                        let (icon, tone) = match d.severity {
                            Severity::Error => (ph::X_CIRCLE, error_tone),
                            Severity::Warning => (ph::WARNING, warn_tone),
                        };
                        let place = format!("line {}, col {}", d.line, d.col);
                        let row = ListItem::new(&d.message)
                            .icon(ui_kit::kit(icon))
                            .icon_tone(tone)
                            .detail(&place)
                            .show(ui);
                        if row.clicked() {
                            self.open_tab(Tab::Script(id));
                            self.docs.doc(id, &self.project.circuit).pending_jump =
                                Some(d.line.saturating_sub(1));
                        }
                    }
                }
                if flow_errors > 0 || flow_runtime.is_some() {
                    let count = (flow_errors + usize::from(flow_runtime.is_some())).to_string();
                    if Section::new("problems-flow", "Flow graph")
                        .icon(ui_kit::kit(ph::FLOW_ARROW))
                        .suffix(&count, None)
                        .default_open(true)
                        .header(ui)
                    {
                        let errs = self.flow_cache.2.clone();
                        for e in errs {
                            let node = e
                                .node
                                .and_then(|i| self.project.flow.nodes.get(i))
                                .map(|n| n.kind.title().to_string())
                                .unwrap_or_default();
                            if ListItem::new(&e.msg)
                                .icon(ui_kit::kit(ph::X_CIRCLE))
                                .icon_tone(error_tone)
                                .detail(&node)
                                .show(ui)
                                .clicked()
                            {
                                self.open_tab(Tab::Flow);
                            }
                        }
                        if let Some(err) = flow_runtime
                            && ListItem::new(&err)
                                .icon(ui_kit::kit(ph::WARNING_CIRCLE))
                                .icon_tone(error_tone)
                                .detail("runtime")
                                .show(ui)
                                .clicked()
                        {
                            self.open_tab(Tab::Flow);
                        }
                    }
                }
            });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn script_log_prefix_splits() {
        assert_eq!(
            "[led] on"
                .strip_prefix('[')
                .and_then(|r| r.split_once("] ")),
            Some(("led", "on"))
        );
        assert_eq!(
            "plain".strip_prefix('[').and_then(|r| r.split_once("] ")),
            None
        );
    }
}
