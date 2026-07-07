//! The Flow tab: the shared `wirelab_flow_ui` node-graph view, kept in sync
//! with `project.flow` and compiled to Rhai by the app cache.

pub use wirelab_flow_ui::{FlowView, build_snarl, extract_graph};

use egui_snarl::NodeId;
use wirelab_flow_ui::FlowViewer;

use crate::app::WireLabApp;

impl WireLabApp {
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
