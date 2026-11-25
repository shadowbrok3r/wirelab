//! Breadboard panels: the parts-tab list and the breadboard inspector.

use egui::{DragValue, RichText};
use egui_theme_kit::controls::{self, Section};
use wirelab_core::circuit::{BbId, CompId, Endpoint};
use wirelab_core::geometry::SPAN_PROP;

use crate::app::{Selection, WireLabApp};
use crate::ui_kit::{self, ph};

impl WireLabApp {
    /// Placed breadboards and buttons to add each library profile.
    pub(crate) fn parts_breadboards(&mut self, ui: &mut egui::Ui) {
        if self.lib.breadboards.is_empty() {
            return;
        }
        let count = self.project.circuit.breadboards.len().to_string();
        if !Section::new("parts-breadboards", "Breadboards")
            .icon(ui_kit::kit(ph::GRID_FOUR))
            .suffix(&count, None)
            .default_open(true)
            .header(ui)
        {
            return;
        }
        let placed: Vec<(BbId, String)> = self
            .project
            .circuit
            .breadboards
            .values()
            .map(|b| {
                let name = self.lib.breadboard(&b.profile).map_or(b.profile.as_str(), |p| p.name.as_str());
                let label = if b.label.is_empty() { format!("BB{}", b.id.0) } else { b.label.clone() };
                (b.id, format!("{label} · {name} · {}°", b.rotation))
            })
            .collect();
        for (id, text) in placed {
            let selected = self.selection == Selection::Breadboard(id);
            if ui.selectable_label(selected, text).clicked() {
                self.selection = Selection::Breadboard(id);
            }
        }
        let profiles: Vec<(String, String)> =
            self.lib.breadboards.values().map(|p| (p.id.clone(), p.name.clone())).collect();
        for (id, name) in profiles {
            if ui_kit::button(ui, ph::PLUS, &format!("Add {name}")).clicked() {
                let at = self.free_spot_for(&id);
                if let Some(bb) = self.add_breadboard_at(&id, at) {
                    self.selection = Selection::Breadboard(bb);
                    self.canvas.fit_pending = true;
                }
            }
        }
    }

    /// Canvas point left of everything placed, with room for a breadboard.
    fn free_spot_for(&self, profile: &str) -> [f32; 2] {
        let (w, h) = self.lib.breadboard(profile).map_or((60.0, 40.0), |p| (p.width_mm, p.height_mm));
        let c = &self.project.circuit;
        let mut min_x = c.board_pos[0];
        for comp in c.components.values() {
            min_x = min_x.min(comp.pos[0] - 15.0);
        }
        for bb in c.breadboards.values() {
            if let Some(p) = self.lib.breadboard(&bb.profile) {
                min_x = min_x.min(bb.pos[0] - p.width_mm.max(p.height_mm) / 2.0);
            }
        }
        [min_x - w / 2.0 - 12.0, c.board_pos[1] + h / 2.0]
    }

    pub(crate) fn inspect_breadboard(&mut self, ui: &mut egui::Ui, id: BbId) {
        let Some(placed) = self.project.circuit.breadboards.get(&id).cloned() else { return };
        let Some(profile) = self.lib.breadboard(&placed.profile).cloned() else {
            ui.label(format!("unknown breadboard profile '{}'", placed.profile));
            return;
        };
        let m = egui_theme_kit::metrics(ui.ctx());
        let weak = ui.visuals().weak_text_color();
        let text = ui.visuals().text_color();
        controls::heading(ui, Some(ui_kit::kit(ph::GRID_FOUR)), &profile.name);
        let holes = profile.holes().count();
        ui_kit::status(
            ui,
            ph::SQUARES_FOUR,
            &format!(
                "{:.1} x {:.1} mm · {holes} holes · {:.2} mm pitch · {}°",
                profile.width_mm, profile.height_mm, profile.pitch_mm, placed.rotation
            ),
            weak,
        );
        if let Some(b) = self.project.circuit.breadboards.get_mut(&id) {
            controls::TextField::new(&mut b.label)
                .hint("Label, e.g. main board")
                .icon(ui_kit::kit(ph::TAG))
                .width(ui.available_width())
                .show(ui);
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("Centre").weak());
            if let Some(b) = self.project.circuit.breadboards.get_mut(&id) {
                let x = ui.add(DragValue::new(&mut b.pos[0]).speed(0.1).suffix(" mm")).changed();
                let y = ui.add(DragValue::new(&mut b.pos[1]).speed(0.1).suffix(" mm")).changed();
                if x || y {
                    self.topo_rev += 1;
                }
            }
        });

        let (mut rotate, mut delete) = (false, false);
        ui.horizontal_wrapped(|ui| {
            rotate = ui_kit::button(ui, ph::ARROW_CLOCKWISE, "Rotate").clicked();
            delete = ui_kit::danger(ui, ph::TRASH, "Delete").clicked();
        });
        if rotate {
            self.rotate_breadboard(id);
        }
        if delete {
            self.project.circuit.remove_breadboard(id);
            self.selection = Selection::None;
            self.topo_rev += 1;
            return;
        }
        ui.add_space(m.gap.y);

        let seats: Vec<(String, String, String)> = self
            .cache
            .seating
            .seats
            .iter()
            .filter(|s| s.bb == id)
            .map(|s| (self.endpoint_label(&s.ep), s.hole_name.clone(), profile.node_label(s.hole)))
            .collect();
        let misses: Vec<String> = self
            .cache
            .seating
            .misses
            .iter()
            .filter(|s| s.bb == id)
            .map(|s| self.endpoint_label(&s.ep))
            .collect();
        let suffix = seats.len().to_string();
        if Section::new(("bb-seats", id.0), "Inserted leads").icon(ui_kit::kit(ph::PUSH_PIN)).suffix(&suffix, None).default_open(true).header(ui) {
            if seats.is_empty() && misses.is_empty() {
                ui.label(RichText::new("Drag a part with leads over the board; its leads snap into the nearest holes.").small().weak());
            }
            for (lead, hole, node) in &seats {
                ui.label(RichText::new(format!("{lead} -> {hole}  ({node})")).monospace().small());
            }
            for lead in &misses {
                controls::note(ui, ui_kit::kit(ph::WARNING_CIRCLE), &format!("{lead} sits over the board but misses every hole"), ui.visuals().error_fg_color);
            }
        }

        if Section::new(("bb-lookup", id.0), "Hole lookup").icon(ui_kit::kit(ph::MAGNIFYING_GLASS)).default_open(true).header(ui) {
            let key = ui.make_persistent_id(("bb-hole-query", id.0));
            let mut query: String = ui.data_mut(|d| d.get_temp(key).unwrap_or_default());
            controls::TextField::new(&mut query).hint("Hole, e.g. E12 or +A3").icon(ui_kit::kit(ph::CROSSHAIR)).width(ui.available_width()).show(ui);
            ui.data_mut(|d| d.insert_temp(key, query.clone()));
            if !query.trim().is_empty() {
                match profile.parse_hole(&query) {
                    Some(h) => {
                        let bus = profile.bus_holes(h);
                        ui_kit::status(ui, ph::LINK, &format!("{} · {} ({} holes)", profile.hole_name(h), profile.node_label(h), bus.len()), text);
                        let names: Vec<String> = bus.iter().map(|&o| profile.hole_name(o)).collect();
                        ui.label(RichText::new(names.join(" ")).monospace().small().weak());
                        let ep = Endpoint::Hole { bb: id, hole: profile.hole_name(h) };
                        let members = self.net_members_of_node(id, &profile, h, &ep);
                        if members.is_empty() {
                            ui.label(RichText::new("nothing connected to this copper").small().weak());
                        } else {
                            ui.label(RichText::new(format!("connected: {}", members.join(", "))).small());
                        }
                    }
                    None => {
                        ui.label(RichText::new("no such hole on this board").small().color(ui.visuals().error_fg_color));
                    }
                }
            }
        }

        if !profile.notes.is_empty()
            && Section::new(("bb-notes", id.0), "Board notes").icon(ui_kit::kit(ph::NOTE)).default_open(false).header(ui)
        {
            for n in &profile.notes {
                controls::note(ui, ui_kit::kit(ph::DOT), n, weak);
            }
        }
    }

    /// Part leads, board pins and wires reaching the copper under a hole.
    fn net_members_of_node(
        &self,
        id: BbId,
        profile: &wirelab_core::breadboard::BreadboardProfile,
        h: wirelab_core::breadboard::HoleRef,
        ep: &Endpoint,
    ) -> Vec<String> {
        let node = profile.node_of(h);
        let same_node = |o: &Endpoint| match o {
            Endpoint::Hole { bb, hole } => *bb == id && profile.parse_hole(hole).is_some_and(|x| profile.node_of(x) == node),
            _ => false,
        };
        let net = self.cache.netlist.nets.iter().find(|n| n.endpoints.iter().any(|e| e == ep || same_node(e)));
        let Some(net) = net else { return Vec::new() };
        let mut out: Vec<String> = net
            .endpoints
            .iter()
            .filter(|e| !matches!(e, Endpoint::Hole { .. }))
            .map(|e| self.endpoint_label(e))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Lead-span control for bendable two-lead parts.
    pub(crate) fn inspect_lead_span(&mut self, ui: &mut egui::Ui, id: CompId, leads: &[[f32; 2]]) {
        if leads.len() != 2 {
            return;
        }
        let default = (leads[1][0] - leads[0][0]).hypot(leads[1][1] - leads[0][1]) as f64;
        let Some(comp) = self.project.circuit.components.get_mut(&id) else { return };
        let mut span = comp.props.get(SPAN_PROP).copied().unwrap_or(default);
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Lead span").weak());
            changed |= ui.add(DragValue::new(&mut span).range(1.0..=20.0).speed(0.05).suffix(" holes")).changed();
            if comp.props.contains_key(SPAN_PROP) && ui.small_button("default").clicked() {
                comp.props.remove(SPAN_PROP);
                self.topo_rev += 1;
            }
        });
        if changed {
            comp.props.insert(SPAN_PROP.to_string(), span.round().max(1.0));
            self.topo_rev += 1;
        }
    }
}
