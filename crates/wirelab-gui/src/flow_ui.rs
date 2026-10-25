//! The Flow tab: the shared `wirelab_flow_ui` node-graph view, kept in sync
//! with `project.flow` and compiled to Rhai by the app cache.

pub use wirelab_flow_ui::{FlowView, build_snarl, extract_graph};

use egui::{Frame, Panel, RichText};
use egui_snarl::NodeId;
use egui_theme_kit::controls::{self, BarKind};
use wirelab_flow_ui::FlowViewer;

use crate::app::WireLabApp;
use crate::dock::Tab;
use crate::ui_kit::{self, ph};

impl WireLabApp {
    /// The flow tab: status toolbar, the graph, and the generated script beside it when shown.
    pub fn show_flow_tab(&mut self, ui: &mut egui::Ui) {
        let m = egui_theme_kit::metrics(ui.ctx());
        let fill = ui.visuals().panel_fill;
        Panel::top("flow-toolbar").exact_size(m.bar_h).frame(Frame::NONE.fill(fill)).show_separator_line(false).show(ui, |ui| {
            controls::bar(ui, BarKind::Tool, |ui| {
                let errs = self.flow_cache.2.len();
                let runtime = self.live.scripts.errors.get(&wirelab_core::script::FLOW_ID).cloned();
                if errs > 0 {
                    let text = format!("{errs} problem{}", if errs == 1 { "" } else { "s" });
                    if ui_kit::status(ui, ph::X_CIRCLE, &text, ui.visuals().error_fg_color).on_hover_text("Open the Problems panel").clicked() {
                        self.open_tab(Tab::Problems);
                    }
                } else if let Some(err) = runtime {
                    ui_kit::status(ui, ph::WARNING_CIRCLE, "Runtime error", ui.visuals().error_fg_color).on_hover_text(err);
                } else if self.project.flow.nodes.is_empty() {
                    ui_kit::status(ui, ph::CURSOR_CLICK, "Right-click the graph to add nodes", ui.visuals().weak_text_color());
                } else if self.live.connected() {
                    ui_kit::status(ui, ph::PULSE, "Live", egui_theme_kit::success(ui));
                } else {
                    ui_kit::status(ui, ph::CHECK_CIRCLE, "Compiles", ui.visuals().weak_text_color());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui_kit::tool_toggle(ui, self.flow_code_open, ph::CODE, "Show the Rhai script the graph compiles to").clicked() {
                        self.flow_code_open = !self.flow_code_open;
                    }
                });
            });
        });
        if self.flow_code_open {
            Panel::right("flow-code").resizable(true).default_size(360.0).show(ui, |ui| {
                controls::subheading(ui, Some(ui_kit::kit(ph::CODE)), "Generated script");
                let code = self.flow_cache.1.clone().unwrap_or_else(|| "// the graph is empty or has problems".into());
                let theme = egui_extras::syntax_highlighting::CodeTheme::from_memory(ui.ctx(), ui.style());
                egui::ScrollArea::vertical().id_salt("flow-code-scroll").auto_shrink([false, false]).show(ui, |ui| {
                    egui_extras::syntax_highlighting::code_view_ui(ui, &theme, &code, "rs");
                });
                ui.label(RichText::new("Read-only: edit the graph to change it.").small().weak());
            });
        }
        egui::CentralPanel::default().frame(Frame::NONE).show(ui, |ui| self.show_flow_editor(ui));
    }

    /// The Flow tab body: sync project → snarl, draw, sync snarl → project.
    pub fn show_flow_editor(&mut self, ui: &mut egui::Ui) {
        if self.flow_view.built_rev != self.flow_rev {
            self.flow_view.snarl = build_snarl(&self.project.flow);
            self.flow_view.built_rev = self.flow_rev;
        }
        let comp_names: Vec<String> =
            wirelab_core::script::component_names(&self.project.circuit, &self.lib)
                .into_values()
                .collect();
        // Live values ride the wires while a device is connected.
        let values = self.live.connected().then(|| {
            self.live.scripts.flow_state().into_iter().collect::<std::collections::HashMap<_, _>>()
        });
        let index: std::collections::HashMap<NodeId, usize> = self
            .flow_view
            .snarl
            .nodes_pos_ids()
            .enumerate()
            .map(|(i, (id, _, _))| (id, i))
            .collect();
        let mut viewer = FlowViewer::new(comp_names, values, index);
        wirelab_flow_ui::show(&mut self.flow_view, &mut viewer, "wirelab-flow", ui);

        let graph = extract_graph(&self.flow_view.snarl);
        if graph != self.project.flow {
            self.project.flow = graph;
            self.flow_rev += 1;
            self.flow_view.built_rev = self.flow_rev;
        }
    }
}
