//! The Flow tab: a node-graph editor (egui-snarl) over `wirelab_core::flow`,
//! kept in sync with `project.flow` and compiled to Rhai by the app cache.

use egui::{Color32, RichText};
use egui_snarl::ui::{PinInfo, SnarlStyle, SnarlViewer};
use egui_snarl::{InPin, NodeId, OutPin, Snarl};
use wirelab_core::flow::{CmpOp, FlowGraph, FlowNode, FlowWire, NodeKind, PinType, flow_categories};

use crate::app::WireLabApp;

#[derive(Default)]
pub struct FlowView {
    pub snarl: Snarl<NodeKind>,
    /// The flow_rev the snarl was last built from; a mismatch means the
    /// project changed underneath (load/new) and the view must rebuild.
    pub built_rev: u64,
}

/// A drag-value with a fixed, readable width so node fields never collapse.
fn num(ui: &mut egui::Ui, dv: egui::DragValue<'_>) -> egui::Response {
    ui.add_sized([62.0, 22.0], dv)
}

fn pin_color(ty: PinType) -> Color32 {
    match ty {
        PinType::Pulse => Color32::from_rgb(240, 160, 60),
        PinType::Bool => Color32::from_rgb(90, 220, 120),
        PinType::Num => Color32::from_rgb(90, 170, 255),
        PinType::Text => Color32::from_rgb(200, 120, 240),
        PinType::Any => Color32::from_gray(160),
    }
}

fn pin_info(ty: PinType) -> PinInfo {
    let info = match ty {
        PinType::Pulse => PinInfo::triangle(),
        PinType::Any => PinInfo::square(),
        _ => PinInfo::circle(),
    };
    info.with_fill(pin_color(ty))
}

pub fn build_snarl(graph: &FlowGraph) -> Snarl<NodeKind> {
    let mut snarl = Snarl::new();
    let ids: Vec<NodeId> = graph
        .nodes
        .iter()
        .map(|n| snarl.insert_node(egui::pos2(n.pos[0], n.pos[1]), n.kind.clone()))
        .collect();
    for w in &graph.wires {
        if let (Some(&from), Some(&to)) = (ids.get(w.from.0), ids.get(w.to.0)) {
            snarl.connect(
                egui_snarl::OutPinId { node: from, output: w.from.1 },
                egui_snarl::InPinId { node: to, input: w.to.1 },
            );
        }
    }
    snarl
}

pub fn extract_graph(snarl: &Snarl<NodeKind>) -> FlowGraph {
    let mut index = std::collections::HashMap::new();
    let mut nodes = Vec::new();
    for (id, pos, kind) in snarl.nodes_pos_ids() {
        index.insert(id, nodes.len());
        nodes.push(FlowNode { kind: kind.clone(), pos: [pos.x, pos.y] });
    }
    let mut wires: Vec<FlowWire> = snarl
        .wires()
        .filter_map(|(out, inp)| {
            Some(FlowWire {
                from: (*index.get(&out.node)?, out.output),
                to: (*index.get(&inp.node)?, inp.input),
            })
        })
        .collect();
    wires.sort_by_key(|w| (w.from, w.to));
    FlowGraph { nodes, wires }
}

struct FlowViewer {
    comp_names: Vec<String>,
    /// Live node-output values keyed `n<node>_<output>`; present while a
    /// device is connected and the flow script is installed.
    values: Option<std::collections::HashMap<String, String>>,
    /// Snarl id → flow-graph node index (extraction order, = compile order).
    index: std::collections::HashMap<NodeId, usize>,
}

impl FlowViewer {
    fn comp_picker(&self, ui: &mut egui::Ui, id: NodeId, comp: &mut String) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("on").small());
            egui::ComboBox::from_id_salt(("flow-comp", id.0))
                .selected_text(if comp.is_empty() { "pick…" } else { comp.as_str() })
                .width(140.0)
                .show_ui(ui, |ui| {
                    if self.comp_names.is_empty() {
                        ui.label(RichText::new("wire up components first").weak().small());
                    }
                    for name in &self.comp_names {
                        ui.selectable_value(comp, name.clone(), name);
                    }
                });
        });
    }
}

impl SnarlViewer<NodeKind> for FlowViewer {
    fn title(&mut self, node: &NodeKind) -> String {
        node.title()
    }

    fn inputs(&mut self, node: &NodeKind) -> usize {
        node.inputs().len()
    }

    fn outputs(&mut self, node: &NodeKind) -> usize {
        node.outputs().len()
    }

    fn show_input(
        &mut self,
        pin: &InPin,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeKind>,
    ) -> impl egui_snarl::ui::SnarlPin + 'static {
        let (name, ty) = snarl[pin.id.node].inputs()[pin.id.input];
        ui.label(RichText::new(name).small().color(pin_color(ty)));
        pin_info(ty)
    }

    fn show_output(
        &mut self,
        pin: &OutPin,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeKind>,
    ) -> impl egui_snarl::ui::SnarlPin + 'static {
        let (name, ty) = snarl[pin.id.node].outputs()[pin.id.output];
        ui.label(RichText::new(name).small().color(pin_color(ty)));
        pin_info(ty)
    }

    fn has_body(&mut self, node: &NodeKind) -> bool {
        !matches!(
            node,
            NodeKind::OnUart
                | NodeKind::Not
                | NodeKind::And
                | NodeKind::Or
                | NodeKind::Edge
                | NodeKind::Toggle
                | NodeKind::Gate
                | NodeKind::Rgb
                | NodeKind::UartSend
        )
    }

    fn show_body(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeKind>,
    ) {
        // Give every node body room so labels and inputs aren't squished.
        ui.set_min_width(190.0);
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        ui.spacing_mut().interact_size.y = 22.0;
        let kind = &mut snarl[node];
        match kind {
            NodeKind::OnPress { comp }
            | NodeKind::OnRelease { comp }
            | NodeKind::OnChange { comp }
            | NodeKind::OnReading { comp }
            | NodeKind::SetComp { comp }
            | NodeKind::ToggleComp { comp } => self.comp_picker(ui, node, comp),
            NodeKind::OnPin { gpio } | NodeKind::SetPin { gpio } => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("GPIO").small());
                    ui.add(egui::DragValue::new(gpio).range(0..=48));
                });
            }
            NodeKind::Every { ms } | NodeKind::Delay { ms } => {
                ui.horizontal(|ui| {
                    num(ui, egui::DragValue::new(ms).range(10.0..=600_000.0).speed(10));
                    ui.label(RichText::new("ms").small());
                });
            }
            NodeKind::Compare { op, value } => {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt((node, "op"))
                        .selected_text(op.symbol())
                        .width(52.0)
                        .show_ui(ui, |ui| {
                            for o in [CmpOp::Gt, CmpOp::Lt, CmpOp::Ge, CmpOp::Le, CmpOp::Eq, CmpOp::Ne]
                            {
                                ui.selectable_value(op, o, o.symbol());
                            }
                        });
                    num(ui, egui::DragValue::new(value).speed(10));
                });
            }
            NodeKind::Threshold { high, low } => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("on ≥").small());
                    num(ui, egui::DragValue::new(high).speed(10));
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("off ≤").small());
                    num(ui, egui::DragValue::new(low).speed(10));
                });
            }
            NodeKind::Counter { modulo } => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("mod").small());
                    num(ui, egui::DragValue::new(modulo).range(1..=1_000_000));
                });
            }
            NodeKind::MapRange { in_min, in_max, out_min, out_max } => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("in").small());
                    num(ui, egui::DragValue::new(in_min).speed(10));
                    ui.label(RichText::new("→").small());
                    num(ui, egui::DragValue::new(in_max).speed(10));
                });
                ui.horizontal(|ui| {
                    ui.label(RichText::new("out").small());
                    num(ui, egui::DragValue::new(out_min).speed(10));
                    ui.label(RichText::new("→").small());
                    num(ui, egui::DragValue::new(out_max).speed(10));
                });
            }
            NodeKind::Script { code, inputs } => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("inputs").small());
                    num(ui, egui::DragValue::new(inputs).range(0..=3));
                });
                ui.add(
                    egui::TextEdit::multiline(code)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(2)
                        .desired_width(174.0),
                );
            }
            NodeKind::Pwm { gpio, freq_hz } => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("GPIO").small());
                    num(ui, egui::DragValue::new(gpio).range(0..=48));
                });
                ui.horizontal(|ui| {
                    num(ui, egui::DragValue::new(freq_hz).range(1..=40_000));
                    ui.label(RichText::new("Hz").small());
                });
            }
            NodeKind::SendText { text } => {
                ui.add(egui::TextEdit::singleline(text).desired_width(174.0));
            }
            NodeKind::LcdText { x, y } => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("x").small());
                    num(ui, egui::DragValue::new(x).range(0..=127));
                    ui.label(RichText::new("y").small());
                    num(ui, egui::DragValue::new(y).range(0..=127));
                });
            }
            NodeKind::Log { label } => {
                ui.add(
                    egui::TextEdit::singleline(label)
                        .hint_text("label")
                        .desired_width(174.0),
                );
            }
            _ => {}
        }
    }

    fn has_wire_widget(
        &mut self,
        from: &egui_snarl::OutPinId,
        _to: &egui_snarl::InPinId,
        snarl: &Snarl<NodeKind>,
    ) -> bool {
        // Pulses are momentary — only level outputs have a value to show.
        self.values.is_some()
            && snarl
                .get_node(from.node)
                .and_then(|k| k.outputs().get(from.output).copied())
                .is_some_and(|(_, ty)| ty != PinType::Pulse)
    }

    fn show_wire_widget(
        &mut self,
        from: &OutPin,
        _to: &InPin,
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeKind>,
    ) {
        let Some(values) = &self.values else { return };
        let Some(&idx) = self.index.get(&from.id.node) else { return };
        let Some(val) = values.get(&format!("n{idx}_{}", from.id.output)) else { return };
        let ty = snarl[from.id.node].outputs()[from.id.output].1;
        egui::Frame::new()
            .fill(egui::Color32::from_black_alpha(200))
            .corner_radius(4.0)
            .inner_margin(egui::Margin::symmetric(5, 2))
            .show(ui, |ui| {
                ui.label(RichText::new(val).small().monospace().color(pin_color(ty)));
            });
    }

    fn connect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<NodeKind>) {
        let from_ty = snarl[from.id.node].outputs()[from.id.output].1;
        let to_ty = snarl[to.id.node].inputs()[to.id.input].1;
        if !to_ty.accepts(from_ty) {
            return;
        }
        // One wire per input keeps the compiled dataflow unambiguous.
        snarl.drop_inputs(to.id);
        snarl.connect(from.id, to.id);
    }

    fn has_graph_menu(&mut self, _pos: egui::Pos2, _snarl: &mut Snarl<NodeKind>) -> bool {
        true
    }

    fn show_graph_menu(&mut self, pos: egui::Pos2, ui: &mut egui::Ui, snarl: &mut Snarl<NodeKind>) {
        ui.label(RichText::new("add node").small().weak());
        for (cat, kinds) in flow_categories() {
            ui.menu_button(cat, |ui| {
                for kind in kinds {
                    let label = generic_label(&kind);
                    if ui.button(label).clicked() {
                        snarl.insert_node(pos, kind.clone());
                        ui.close();
                    }
                }
            });
        }
    }

    fn has_node_menu(&mut self, _node: &NodeKind) -> bool {
        true
    }

    fn show_node_menu(
        &mut self,
        node: NodeId,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut egui::Ui,
        snarl: &mut Snarl<NodeKind>,
    ) {
        if ui.button("delete node").clicked() {
            snarl.remove_node(node);
            ui.close();
        }
    }
}

/// Menu label for a palette entry (component params still unpicked).
fn generic_label(kind: &NodeKind) -> String {
    match kind {
        NodeKind::OnPress { .. } => "on press".into(),
        NodeKind::OnRelease { .. } => "on release".into(),
        NodeKind::OnChange { .. } => "on level change".into(),
        NodeKind::OnReading { .. } => "on analog reading".into(),
        NodeKind::SetComp { .. } => "set component".into(),
        NodeKind::ToggleComp { .. } => "toggle component".into(),
        NodeKind::SendText { .. } => "send text".into(),
        NodeKind::Log { .. } => "log".into(),
        other => other.title(),
    }
}

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
        let mut viewer = FlowViewer { comp_names, values, index };
        let style = SnarlStyle::new();
        self.flow_view.snarl.show(&mut viewer, &style, "wirelab-flow", ui);

        let graph = extract_graph(&self.flow_view.snarl);
        if graph != self.project.flow {
            self.project.flow = graph;
            self.flow_rev += 1;
            self.flow_view.built_rev = self.flow_rev;
        }
    }
}
