//! Dock panels: parts, inspector, checks, rules, and the title bar's device controls.

use egui::{ComboBox, DragValue, IdSalt, RichText, ScrollArea, Sense, Vec2};
use egui_theme_kit::controls::{self, BarKind, ListItem, MenuButton, Section};
use wirelab_core::circuit::CompId;
use wirelab_core::component::{CompState, SimModel};
use wirelab_core::program::{Action, Rule, Trigger};
use wirelab_core::validate::Severity;
use wirelab_link::SessionPhase;
use wirelab_link::discovery::Discovery;
use wirelab_proto::WifiState;

use crate::app::{Selection, WireLabApp};
use crate::dock::Tab;
use crate::live::Backend;
use crate::ui_kit::{self, ph};

/// Side of a part icon in the components list.
const PART_ICON: f32 = 20.0;

impl WireLabApp {
    /// The parts tab: the board picker and the searchable component library.
    pub fn show_parts(&mut self, ui: &mut egui::Ui) {
        ScrollArea::vertical().id_salt("parts").auto_shrink([false, false]).show(ui, |ui| {
            self.parts_board(ui);
            self.parts_breadboards(ui);
            let m = egui_theme_kit::metrics(ui.ctx());
            ui.add_space(m.gap.y);
            ui.horizontal(|ui| {
                controls::search_field(ui, &mut self.parts_filter, "Search parts", (ui.available_width() - m.control_h - m.gap.x).max(80.0));
                if ui_kit::tool(ui, ph::PLUGS_CONNECTED, "Wiring guide: LEDs and resistors, pull-ups, dividers").clicked() {
                    self.open_tab(Tab::WiringGuide);
                }
            });
            ui.label(RichText::new("Click a part, then click the schematic to place it.").weak().small());
            ui.add_space(m.gap.y);
            self.parts_list(ui);
        });
    }

    fn parts_board(&mut self, ui: &mut egui::Ui) {
        let icon = self.lib.board(&self.project.circuit.board_id).map_or(crate::icons::BOARD_DEVKIT, crate::icons::board);
        if !Section::new("parts-board", "Board").icon(ui_kit::kit(icon)).default_open(true).header(ui) {
            return;
        }
        let options: Vec<(String, String)> = self.lib.boards.iter().map(|(id, b)| (id.clone(), b.name.clone())).collect();
        let options: Vec<(String, &str)> = options.iter().map(|(id, name)| (id.clone(), name.as_str())).collect();
        let mut chosen = self.project.circuit.board_id.clone();
        controls::Dropdown::new("board-pick", &mut chosen, &options).width(ui.available_width()).show(ui);
        if chosen != self.project.circuit.board_id {
            if self.live.connected() {
                self.live.disconnect(&mut self.console);
                self.log("disconnected: board changed");
            }
            self.project.circuit.board_id = chosen;
            self.topo_rev += 1;
        }
        let Some(board) = self.lib.board(&self.project.circuit.board_id) else { return };
        let weak = ui.visuals().weak_text_color();
        ui_kit::status(ui, ph::CPU, &format!("{} · {} pins", board.chip.name(), board.pins.len()), weak);
        let f = &board.features;
        let mut extras = Vec::new();
        if let Some(g) = f.rgb_led_gpio {
            extras.push(format!("RGB LED (GPIO{g})"));
        }
        if f.reset_button {
            extras.push("RESET".into());
        }
        if f.boot_button_gpio.is_some() {
            extras.push("BOOT".into());
        }
        if !extras.is_empty() {
            ui_kit::status(ui, ph::CURSOR_CLICK, &format!("On board: {}", extras.join(" · ")), weak);
        }
        if !board.specs.is_empty() {
            let specs = board.specs.clone();
            if Section::new("parts-capabilities", "Capabilities").icon(ui_kit::kit(ph::LIST_BULLETS)).default_open(false).header(ui) {
                for s in &specs {
                    controls::note(ui, ui_kit::kit(ph::DOT), s, weak);
                }
                ui.label(RichText::new("Scripts can ask: board_has(\"wifi\"), chip()").monospace().small().weak());
            }
        }
    }

    fn parts_list(&mut self, ui: &mut egui::Ui) {
        let filter = self.parts_filter.trim().to_lowercase();
        let matches = |def: &wirelab_core::component::ComponentDef| {
            filter.is_empty()
                || def.name.to_lowercase().contains(&filter)
                || def.category.to_lowercase().contains(&filter)
                || def.id.contains(&filter)
        };
        let mut categories: Vec<String> = self.lib.components.values().filter(|d| matches(d)).map(|c| c.category.clone()).collect();
        categories.sort();
        categories.dedup();
        if categories.is_empty() {
            ui.label(RichText::new("No part matches the search.").weak());
            return;
        }
        for cat in categories {
            let defs: Vec<wirelab_core::component::ComponentDef> =
                self.lib.components.values().filter(|c| c.category == cat && matches(c)).cloned().collect();
            let count = defs.len().to_string();
            let open = Section::new(("parts-category", &cat), &cat)
                .icon(ui_kit::kit(crate::icons::category(&cat)))
                .suffix(&count, None)
                .default_open(true)
                .open((!filter.is_empty()).then_some(true))
                .header(ui);
            if !open {
                continue;
            }
            for def in &defs {
                let active = self.canvas.placing.as_deref() == Some(def.id.as_str());
                let icon = IdSalt::new(&def.id);
                // Trailing grow-atom pushes the label to the left edge.
                let button = egui::Button::selectable(
                    active,
                    (egui::Atom::custom(icon, Vec2::splat(PART_ICON)), RichText::new(&def.name), egui::Atom::grow()),
                );
                let layout = egui::Layout::centered_and_justified(ui.layout().main_dir());
                let size = egui::vec2(ui.available_width(), PART_ICON + 6.0);
                let atoms = ui.allocate_ui_with_layout(size, layout, |ui| button.atom_ui(ui)).inner;
                if let Some(rect) = atoms.rect(icon) {
                    crate::icons::paint_part(ui, rect, def);
                }
                let btn = atoms.response;
                if btn.clicked() {
                    self.canvas.placing = if active { None } else { Some(def.id.clone()) };
                    self.canvas.wire_from = None;
                }
                if btn.hovered() && !def.description.is_empty() {
                    btn.on_hover_text(&def.description);
                }
            }
        }
    }

    /// The inspector tab: properties and actions of the current selection.
    pub fn show_inspector_tab(&mut self, ui: &mut egui::Ui) {
        ScrollArea::vertical().id_salt("inspector").auto_shrink([false, false]).show(ui, |ui| match self.selection.clone() {
            Selection::Comp(id) => self.inspect_component(ui, id),
            Selection::Comps(ids) => self.inspect_many(ui, &ids),
            Selection::Wire(id) => self.inspect_wire(ui, id),
            Selection::Breadboard(id) => self.inspect_breadboard(ui, id),
            Selection::Pin(key) => self.inspect_pin(ui, &key),
            Selection::None => {
                controls::empty_state(ui, ui_kit::kit(ph::CURSOR_CLICK), "Nothing selected", "Click a part, a wire or a board pin on the schematic.", &[]);
            }
        });
    }

    fn inspect_many(&mut self, ui: &mut egui::Ui, ids: &[CompId]) {
        controls::heading(ui, Some(ui_kit::kit(ph::STACK)), &format!("{} parts selected", ids.len()));
        for id in ids.iter().take(12) {
            let Some(def) = self.project.circuit.components.get(id).and_then(|c| self.lib.component(&c.def_id)).cloned() else { continue };
            let name = self.comp_name(*id);
            ListItem::new(&name).icon(ui_kit::kit(crate::icons::part(&def))).icon_tone(crate::icons::part_tint(ui, &def)).detail(&def.name).show(ui);
        }
        if ids.len() > 12 {
            ui.label(RichText::new(format!("… and {} more", ids.len() - 12)).weak());
        }
        ui.add_space(egui_theme_kit::metrics(ui.ctx()).gap.y);
        ui.horizontal_wrapped(|ui| {
            if ui_kit::button(ui, ph::MAGIC_WAND, "Auto wire").on_hover_text("Wire the selection to free board pins").clicked() {
                self.apply_auto_wire();
            }
            if ui_kit::button(ui, ph::X, "Deselect").clicked() {
                self.selection = Selection::None;
            }
        });
    }

    fn inspect_wire(&mut self, ui: &mut egui::Ui, id: wirelab_core::circuit::WireId) {
        controls::heading(ui, Some(ui_kit::kit(ph::LINK)), &format!("Wire #{}", id.0));
        if let Some(w) = self.project.circuit.wires.get(&id) {
            let describe = |ep: &wirelab_core::circuit::Endpoint| self.endpoint_label(ep);
            let text = format!("{}  ->  {}", describe(&w.a), describe(&w.b));
            ui_kit::status(ui, ph::ARROWS_LEFT_RIGHT, &text, ui.visuals().text_color());
            if !w.label.is_empty() || !w.spec.is_empty() {
                ui.label(RichText::new(format!("{} {}", w.label, w.spec).trim().to_string()).small().weak());
            }
        }
        ui.add_space(egui_theme_kit::metrics(ui.ctx()).gap.y);
        let routed = self.project.circuit.wires.get(&id).map_or(0, |w| w.waypoints.len());
        if routed > 0 {
            ui.label(RichText::new(format!("Manual route through {routed} points")).small().weak());
            if ui_kit::button(ui, ph::ERASER, "Clear manual route").clicked()
                && let Some(w) = self.project.circuit.wires.get_mut(&id)
            {
                w.waypoints.clear();
            }
        }
        if ui_kit::danger(ui, ph::TRASH, "Delete wire").clicked() {
            self.project.circuit.remove_wire(id);
            self.selection = Selection::None;
            self.topo_rev += 1;
        }
    }

    fn inspect_pin(&mut self, ui: &mut egui::Ui, key: &str) {
        let Some(board) = self.lib.board(&self.project.circuit.board_id) else { return };
        let Some(pin) = board.pin(key).cloned() else { return };
        controls::heading(ui, Some(ui_kit::kit(ph::DOT)), &pin.key);
        let text = ui.visuals().text_color();
        ui_kit::status(ui, ph::TAG, &format!("{:?}", pin.kind), text);
        if !pin.caps.is_empty() {
            ui_kit::status(ui, ph::LIST_CHECKS, &format!("{:?}", pin.caps), text);
        }
        if let Some((unit, ch)) = pin.adc {
            ui_kit::status(ui, ph::WAVEFORM, &format!("ADC{unit}_CH{ch}"), text);
        }
        if let Some(w) = &pin.warning {
            controls::note(ui, ui_kit::kit(ph::WARNING), w, ui.visuals().warn_fg_color);
        }
    }

    fn inspect_component(&mut self, ui: &mut egui::Ui, id: CompId) {
        let Some(def) = self.project.circuit.components.get(&id).and_then(|c| self.lib.component(&c.def_id)).cloned() else {
            return;
        };
        let m = egui_theme_kit::metrics(ui.ctx());
        let name = self.comp_name(id);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(32.0), Sense::hover());
            crate::icons::paint_part(ui, rect, &def);
            ui.vertical(|ui| {
                ui.label(RichText::new(&name).strong().size(15.0));
                ui.label(RichText::new(&def.name).weak());
            });
        });
        if !def.description.is_empty() {
            ui.label(RichText::new(&def.description).small().weak());
        }
        ui.add_space(m.gap.y);
        let (icon, text, tone) = match self.cache.bindings.gpio_of(id) {
            Some(g) => (ph::PLUGS_CONNECTED, format!("Bound to GPIO{g}"), egui_theme_kit::success(ui)),
            None => (ph::LINK_BREAK, "Not bound to a GPIO".to_string(), ui.visuals().weak_text_color()),
        };
        ui_kit::status(ui, icon, &text, tone);

        if let Some(comp) = self.project.circuit.components.get_mut(&id) {
            controls::TextField::new(&mut comp.label).hint("Label (becomes the script name)").icon(ui_kit::kit(ph::TAG)).width(ui.available_width()).show(ui);
        }
        if !def.leads.is_empty() {
            self.inspect_lead_span(ui, id, &def.leads);
            let seats: Vec<String> = def
                .terminals
                .iter()
                .filter_map(|t| {
                    let ep = wirelab_core::circuit::Endpoint::Terminal { comp: id, terminal: t.id.clone() };
                    self.cache.seating.seat_of(&ep).map(|s| format!("{} -> {}", t.id, s.hole_name))
                })
                .collect();
            if !seats.is_empty() {
                ui_kit::status(ui, ph::PUSH_PIN, &seats.join(" · "), ui.visuals().text_color());
            }
        }

        if Section::new(("inspect-script", id), "Script").icon(ui_kit::kit(ph::FILE_CODE)).default_open(true).header(ui) {
            self.inspect_script(ui, id);
        }

        let rules = self.rules_for_comp(id);
        if rules > 0 {
            let label = format!("Used by {rules} rule(s)");
            if ui_kit::ghost(ui, ph::LIST_CHECKS, &label).clicked() {
                self.open_tab(Tab::Rules);
            }
        }

        let has_values = !def.props.is_empty()
            || matches!(
                def.sim,
                SimModel::Potentiometer { .. }
                    | SimModel::Photoresistor { .. }
                    | SimModel::AnalogSensor { .. }
                    | SimModel::ToggleSwitch
                    | SimModel::SlideSwitchSpdt
                    | SimModel::DigitalSensor
                    | SimModel::PushButton
            );
        if has_values && Section::new(("inspect-live", id), "Values").icon(ui_kit::kit(ph::SLIDERS)).default_open(true).header(ui) {
            self.inspect_values(ui, id, &def);
        }

        ui.add_space(m.gap.y);
        let (mut rotate, mut delete) = (false, false);
        ui.horizontal_wrapped(|ui| {
            rotate = ui_kit::button(ui, ph::ARROW_CLOCKWISE, "Rotate").clicked();
            delete = ui_kit::danger(ui, ph::TRASH, "Delete").clicked();
        });
        if rotate && let Some(comp) = self.project.circuit.components.get_mut(&id) {
            comp.rotation = (comp.rotation + 90) % 360;
            self.topo_rev += 1;
        }
        if delete {
            self.project.circuit.remove_component(id);
            self.selection = Selection::None;
            self.topo_rev += 1;
        }
    }

    /// Script state, handlers with their last run, and open / attach.
    fn inspect_script(&mut self, ui: &mut egui::Ui, id: CompId) {
        let has_script = self.project.circuit.components.get(&id).is_some_and(|c| c.script.is_some());
        let (icon, text, tone) = self.script_state(ui, id);
        ui_kit::status(ui, icon, text, tone);
        if let Some(err) = self.live.scripts.errors.get(&id).cloned() {
            controls::note(ui, ui_kit::kit(ph::WARNING_CIRCLE), &err, ui.visuals().error_fg_color);
        }
        let (handlers, helpers): (Vec<String>, Vec<String>) =
            self.live.scripts.handlers(id).into_iter().partition(|h| h.starts_with("on_"));
        if !handlers.is_empty() {
            let now = self.live.now_ms();
            let fired = self.live.scripts.last_fired.get(&id).cloned();
            for h in handlers {
                let detail = fired
                    .as_ref()
                    .filter(|(f, _)| *f == h)
                    .map(|(_, at)| format!("ran {:.1} s ago", now.saturating_sub(*at) as f32 / 1000.0))
                    .unwrap_or_default();
                let icon = if h.starts_with("on_") { ph::LIGHTNING } else { ph::BRACKETS_CURLY };
                if ListItem::new(&h).icon(ui_kit::kit(icon)).detail(&detail).show(ui).clicked() {
                    self.open_tab(Tab::Script(id));
                    let doc = self.docs.doc(id, &self.project.circuit);
                    doc.pending_jump = doc.buffer.lines().position(|l| {
                        l.trim_start().strip_prefix("fn ").is_some_and(|r| r.starts_with(h.as_str()) && r[h.len()..].starts_with(['(', ' ']))
                    });
                }
            }
        }
        if !helpers.is_empty() {
            let text = format!("{} helper function{}", helpers.len(), if helpers.len() == 1 { "" } else { "s" });
            ui_kit::status(ui, ph::BRACKETS_CURLY, &text, ui.visuals().weak_text_color()).on_hover_text(helpers.join(", "));
        }
        ui.horizontal_wrapped(|ui| {
            if has_script || self.docs.is_dirty(id, &self.project.circuit) {
                if ui_kit::primary(ui, true, ph::FILE_CODE, "Edit script").clicked() {
                    self.open_tab(Tab::Script(id));
                }
            } else if ui_kit::primary(ui, true, ph::FILE_PLUS, "Attach script").clicked() {
                self.attach_template_script(id);
            }
        });
    }

    /// Rules whose trigger or actions name `id`.
    fn rules_for_comp(&self, id: CompId) -> usize {
        self.project
            .program
            .rules
            .iter()
            .filter(|r| {
                matches!(&r.trigger, Trigger::CompEvent { comp, .. } if *comp == id)
                    || r.actions.iter().any(|a| matches!(a, Action::CompAction { comp, .. } if *comp == id))
            })
            .count()
    }

    /// Per-instance properties and live pokes.
    fn inspect_values(&mut self, ui: &mut egui::Ui, id: CompId, def: &wirelab_core::component::ComponentDef) {
        let Some(comp) = self.project.circuit.components.get_mut(&id) else { return };
        for prop in &def.props {
            let mut v = comp.props.get(&prop.key).copied().unwrap_or(prop.default);
            let changed = ui
                .horizontal(|ui| {
                    controls::form_row(ui, &prop.name, |ui| {
                        ui.add(DragValue::new(&mut v).range(prop.min..=prop.max).speed((prop.max - prop.min) / 200.0)).changed()
                    })
                })
                .inner;
            if changed {
                comp.props.insert(prop.key.clone(), v);
                self.state_rev += 1;
            }
        }
        let label = match def.sim {
            SimModel::Potentiometer { .. } => "Wiper",
            SimModel::Photoresistor { .. } => "Light level",
            _ => "Sensor level",
        };
        match (&def.sim, &mut comp.state) {
            (SimModel::Potentiometer { .. } | SimModel::Photoresistor { .. } | SimModel::AnalogSensor { .. }, CompState::Fraction { value }) => {
                let changed = ui.horizontal(|ui| controls::form_row(ui, label, |ui| ui.add(egui::Slider::new(value, 0.0..=1.0)).changed())).inner;
                if changed {
                    self.state_rev += 1;
                }
            }
            (SimModel::ToggleSwitch | SimModel::SlideSwitchSpdt | SimModel::DigitalSensor, CompState::Toggle { on }) => {
                if controls::toggle(ui, on, "On").changed() {
                    self.state_rev += 1;
                }
            }
            (SimModel::PushButton, CompState::Button { pressed }) => {
                let text = if *pressed { "Pressed (hold it on the schematic)" } else { "Released (hold it on the schematic)" };
                ui_kit::status(ui, ph::HAND_POINTING, text, ui.visuals().weak_text_color());
            }
            _ => {}
        }
    }

    /// The checks tab: electrical validation with one-click fixes.
    pub fn show_checks(&mut self, ui: &mut egui::Ui) {
        let lints = self.cache.lints.clone();
        let warnings = self.cache.bindings.warnings.clone();
        let live_warnings: Vec<String> = self.live.live_output.as_ref().map(|o| o.warnings.clone()).unwrap_or_default();
        if lints.is_empty() && warnings.is_empty() && live_warnings.is_empty() {
            controls::empty_state(ui, ui_kit::kit(ph::SHIELD_CHECK), "All clear", "No shorts, floating parts or missing resistors.", &[]);
            return;
        }
        let (error_tone, warn_tone, info_tone) = (ui.visuals().error_fg_color, ui.visuals().warn_fg_color, ui.visuals().weak_text_color());
        let mut apply_fix: Option<wirelab_core::validate::LintFix> = None;
        ScrollArea::vertical().id_salt("checks").auto_shrink([false, false]).show(ui, |ui| {
            for w in &warnings {
                ListItem::new(w).icon(ui_kit::kit(ph::WARNING)).icon_tone(warn_tone).show(ui);
            }
            for lint in &lints {
                let (icon, tone) = match lint.severity {
                    Severity::Error => (ph::X_CIRCLE, error_tone),
                    Severity::Warning => (ph::WARNING, warn_tone),
                    Severity::Info => (ph::INFO, info_tone),
                };
                let mut row = ListItem::new(&lint.message).icon(ui_kit::kit(icon)).icon_tone(tone);
                if lint.fix.is_some() {
                    row = row.action(ui_kit::kit(ph::WRENCH), "Fix: place the nearest stock resistor and splice it into the wire");
                }
                let (row, fix) = row.show_with_action(ui);
                if (row.hovered() || fix.as_ref().is_some_and(|f| f.hovered())) && !lint.comps.is_empty() {
                    self.hover_highlight = lint.comps.clone();
                }
                if fix.is_some_and(|f| f.clicked())
                    && let Some(f) = &lint.fix
                {
                    apply_fix = Some(f.clone());
                }
            }
            for w in &live_warnings {
                ListItem::new(w).icon(ui_kit::kit(ph::LIGHTNING)).icon_tone(error_tone).detail("live").show(ui);
            }
        });
        if let Some(fix) = apply_fix {
            self.apply_lint_fix(&fix);
        }
    }

    /// Device controls in the title bar's right slot; items are added right to left.
    pub fn show_device_bar(&mut self, ui: &mut egui::Ui) {
        let running = self.live.program_running;
        let can_run = self.live.connected() && !self.project.program.rules.is_empty();
        if running {
            if ui_kit::button(ui, ph::STOP, "Stop rules").clicked() {
                self.live.stop_program(&mut self.console);
            }
        } else if ui_kit::button_enabled(ui, can_run, ph::PLAY, "Run rules")
            .on_hover_text("Run the rules program on the connected board")
            .clicked()
        {
            let program = self.project.program.clone();
            let bindings = self.cache.bindings.clone();
            self.live.start_program(&program, &bindings, &mut self.console);
        }
        controls::vline(ui, egui_theme_kit::metrics(ui.ctx()).control_h);

        if self.live.connected() {
            if ui_kit::button(ui, ph::PLUG, "Disconnect").clicked() {
                self.live.disconnect(&mut self.console);
                return;
            }
            self.wifi_menu(ui);
            let (phase, desc, chip) = {
                let s = self.live.session.as_ref().unwrap();
                (s.phase, s.device.description(), s.info.map(|i| i.chip))
            };
            if let Some(chip) = chip.filter(|c| *c != wirelab_proto::ChipKind::Simulated) {
                ui.label(RichText::new(chip.name()).weak());
            }
            ui.label(RichText::new(desc).weak());
            let (icon, text, tone) = match phase {
                SessionPhase::Ready => (ph::PLUGS_CONNECTED, "Live", egui_theme_kit::success(ui)),
                SessionPhase::AwaitingHello => (ph::HOURGLASS, "Waiting", ui.visuals().warn_fg_color),
                SessionPhase::Dead => (ph::PLUG, "Lost", ui.visuals().error_fg_color),
            };
            ui_kit::status(ui, icon, text, tone);
            return;
        }

        if ui_kit::primary(ui, true, ph::PLUGS_CONNECTED, "Connect").clicked() {
            match self.live.backend {
                Backend::Simulator => {
                    let lib = self.lib.clone();
                    let circuit = self.project.circuit.clone();
                    self.live.connect_sim(&lib, &circuit, &mut self.console);
                }
                Backend::Serial => self.live.connect_serial(&mut self.console),
                Backend::Tcp => self.live.connect_tcp(&mut self.console),
            }
        }
        match self.live.backend {
            Backend::Serial => self.serial_controls(ui),
            Backend::Tcp => self.tcp_controls(ui),
            Backend::Simulator => {
                if self.live.discovery.is_some() {
                    self.live.discovery = None;
                }
            }
        }
        let backends = [(Backend::Simulator, "Simulator"), (Backend::Serial, "USB serial"), (Backend::Tcp, "Wi-Fi (TCP)")];
        controls::Dropdown::new("backend", &mut self.live.backend, &backends).width(132.0).show(ui);
    }

    fn serial_controls(&mut self, ui: &mut egui::Ui) {
        let flashing = self.flash.running();
        if flashing {
            controls::spinner(ui, controls::SpinnerSize::Sm);
        }
        let label = if flashing { "Flashing…" } else { "Flash firmware" };
        if ui_kit::button_enabled(ui, !flashing, ph::LIGHTNING, label)
            .on_hover_text("Build the WireLab firmware for this board's chip and flash it onto the selected port")
            .clicked()
        {
            let chip = self.lib.board(&self.project.circuit.board_id).map(|b| b.chip);
            let port = self.live.ports.get(self.live.selected_port).cloned();
            match (chip, port) {
                (Some(chip), Some(port)) => self.flash.start(chip, &port, &mut self.console),
                _ => self.log("pick a board and a port first"),
            }
        }
        if ui_kit::tool(ui, ph::ARROWS_CLOCKWISE, "Rescan serial ports").clicked() {
            self.live.refresh_ports();
        }
        let ports: Vec<(usize, String)> = self.live.ports.iter().cloned().enumerate().collect();
        let ports: Vec<(usize, &str)> = ports.iter().map(|(i, p)| (*i, p.as_str())).collect();
        controls::Dropdown::new("port", &mut self.live.selected_port, &ports).placeholder("No ports").width(170.0).show(ui);
    }

    fn tcp_controls(&mut self, ui: &mut egui::Ui) {
        // The beacon listener runs while the TCP backend is selected.
        let disco = self.live.discovery.get_or_insert_with(Discovery::listen);
        let boards = disco.boards();
        if let Some(err) = disco.error() {
            ui_kit::row_action(ui, ph::WARNING, &format!("Discovery listener: {err}"), ui.visuals().error_fg_color);
        }
        controls::TextField::new(&mut self.live.tcp_addr).hint("192.168.1.x:4518").width(150.0).show(ui);
        let found: Vec<(String, String)> = boards.iter().map(|b| (b.addr.clone(), format!("{} ({})", b.addr, b.chip))).collect();
        let _ = MenuButton::new("Boards")
            .icon(ui_kit::kit(ph::WIFI_HIGH))
            .tip("Boards announce themselves over UDP once their Wi-Fi is set up")
            .show(ui, |ui| {
                if found.is_empty() {
                    controls::menu_inset_row(ui, |ui| ui.label(RichText::new("Listening for beacons…").weak()));
                }
                for (addr, label) in &found {
                    if ui_kit::menu_item(ui, ph::CPU, label).clicked() {
                        self.live.tcp_addr = addr.clone();
                        ui.close();
                    }
                }
            });
    }

    /// Wi-Fi status and provisioning for the live board: join a network over the current link,
    /// then optionally move the session onto TCP.
    fn wifi_menu(&mut self, ui: &mut egui::Ui) {
        let wifi = self.live.session.as_ref().and_then(|s| s.wifi);
        let (tone, tip) = match wifi {
            Some((WifiState::Connected, ip)) => {
                (egui_theme_kit::success(ui), format!("Wi-Fi: connected, {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]))
            }
            Some((WifiState::Connecting, _)) => (ui.visuals().warn_fg_color, "Wi-Fi: connecting…".into()),
            Some((WifiState::Failed, _)) => (ui.visuals().error_fg_color, "Wi-Fi: join failed".into()),
            Some((WifiState::Off, _)) | None => (ui.visuals().weak_text_color(), "Wi-Fi: off, click to set up".into()),
        };
        let btn = controls::IconButton::new(ui_kit::kit(ph::WIFI_HIGH), controls::IconBox::Md).tint(tone).tip(&tip).show(ui);
        // A popup, not a menu, so typing and the buttons don't close it; a click outside does.
        egui::Popup::from_toggle_button_response(&btn)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_min_width(260.0);
                controls::subheading(ui, Some(ui_kit::kit(ph::WIFI_HIGH)), "Board Wi-Fi");
                controls::TextField::new(&mut self.live.wifi_ssid).hint("Network name (SSID)").width(240.0).show(ui);
                controls::TextField::new(&mut self.live.wifi_pass).hint("Password").password(true).width(240.0).show(ui);
                ui.horizontal(|ui| {
                    let can_join = !self.live.wifi_ssid.trim().is_empty();
                    if ui_kit::primary(ui, can_join, ph::WIFI_HIGH, "Join").clicked() {
                        self.live.provision_wifi(&mut self.console);
                    }
                    if ui_kit::button(ui, ph::WIFI_SLASH, "Forget").on_hover_text("Turn the radio off").clicked() {
                        self.live.wifi_ssid.clear();
                        if let Some(s) = &mut self.live.session {
                            let _ = s.send(&wirelab_proto::HostMsg::WifiConfig { ssid: Default::default(), pass: Default::default() });
                        }
                    }
                });
                match wifi {
                    Some((WifiState::Connected, ip)) => {
                        let text = format!("Board IP {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
                        ui_kit::status(ui, ph::GLOBE, &text, ui.visuals().text_color());
                        if self.live.backend != Backend::Tcp
                            && ui_kit::button(ui, ph::SWAP, "Use Wi-Fi link")
                                .on_hover_text("Close the serial session and reconnect over TCP; the USB cable is then only power")
                                .clicked()
                        {
                            self.live.tcp_addr =
                                format!("{}.{}.{}.{}:{}", ip[0], ip[1], ip[2], ip[3], wirelab_link::tcp::DEFAULT_TCP_PORT);
                            self.live.disconnect(&mut self.console);
                            self.live.connect_tcp(&mut self.console);
                        }
                    }
                    Some((WifiState::Failed, _)) => {
                        controls::note(ui, ui_kit::kit(ph::WARNING_CIRCLE), "Join failed: check the SSID and password", ui.visuals().error_fg_color);
                    }
                    _ => {}
                }
                ui.label(RichText::new("Credentials live in board RAM; re-join after a reset.").weak().small());
            });
    }

    /// Compile immediately when no session tick will do it for us.
    pub(crate) fn sync_scripts_offline(&mut self) {
        if !self.live.connected() {
            self.live.scripts.sync(&self.project.circuit, &self.lib);
        }
    }

    /// The rules tab: toolbar, then the trigger -> action rules.
    pub fn show_rules_tab(&mut self, ui: &mut egui::Ui) {
        let m = egui_theme_kit::metrics(ui.ctx());
        egui::Panel::top("rules-toolbar")
            .exact_size(m.bar_h)
            .frame(egui::Frame::NONE.fill(ui.visuals().panel_fill))
            .show_separator_line(false)
            .show(ui, |ui| {
                controls::bar(ui, BarKind::Tool, |ui| {
                    if ui_kit::button(ui, ph::PLUS, "Add rule").clicked() {
                        self.add_rule();
                    }
                    controls::vline(ui, m.control_h);
                    let can_run = self.live.connected() && !self.project.program.rules.is_empty();
                    if self.live.program_running {
                        if ui_kit::button(ui, ph::STOP, "Stop rules").clicked() {
                            self.live.stop_program(&mut self.console);
                        }
                    } else if ui_kit::button_enabled(ui, can_run, ph::PLAY, "Run rules").clicked() {
                        let program = self.project.program.clone();
                        let bindings = self.cache.bindings.clone();
                        self.live.start_program(&program, &bindings, &mut self.console);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let n = self.project.program.rules.len();
                        let (icon, text, tone) = if self.live.program_running {
                            (ph::PULSE, format!("{n} rules · running"), egui_theme_kit::success(ui))
                        } else if self.live.connected() {
                            (ph::PAUSE, format!("{n} rules · stopped"), ui.visuals().weak_text_color())
                        } else {
                            (ph::PLUG, format!("{n} rules · connect to run"), ui.visuals().weak_text_color())
                        };
                        ui_kit::status(ui, icon, &text, tone);
                    });
                });
            });
        if self.project.program.rules.is_empty() {
            let hint = "Rules turn part events into actions without code, e.g. when the button is pressed, toggle the LED.";
            if controls::empty_state(ui, ui_kit::kit(ph::LIST_CHECKS), "No rules yet", hint, &[(controls::Role::Primary, "Add a rule")]).is_some() {
                self.add_rule();
            }
            return;
        }
        ScrollArea::vertical().id_salt("rules").auto_shrink([false, false]).show(ui, |ui| self.show_program(ui));
    }

    fn add_rule(&mut self) {
        let event_comps = self.event_comps();
        let action_comps = self.action_comps();
        let trigger = event_comps
            .first()
            .map(|(id, _, evs)| Trigger::CompEvent { comp: *id, event: evs.first().cloned().unwrap_or_else(|| "pressed".into()) })
            .unwrap_or(Trigger::OnStart);
        self.project.program.rules.push(Rule {
            name: format!("rule {}", self.project.program.rules.len() + 1),
            enabled: true,
            trigger,
            actions: vec![default_action(&action_comps)],
        });
    }

    fn event_comps(&self) -> Vec<(CompId, String, Vec<String>)> {
        self.project
            .circuit
            .components
            .values()
            .filter_map(|c| {
                let def = self.lib.component(&c.def_id)?;
                if def.events.is_empty() {
                    return None;
                }
                let name = if c.label.is_empty() { def.name.clone() } else { c.label.clone() };
                Some((c.id, name, def.events.iter().map(|e| e.id.clone()).collect()))
            })
            .collect()
    }

    fn action_comps(&self) -> Vec<(CompId, String, Vec<String>)> {
        self.project
            .circuit
            .components
            .values()
            .filter_map(|c| {
                let def = self.lib.component(&c.def_id)?;
                if def.actions.is_empty() {
                    return None;
                }
                let name = if c.label.is_empty() { def.name.clone() } else { c.label.clone() };
                Some((c.id, name, def.actions.iter().map(|a| a.id.clone()).collect()))
            })
            .collect()
    }

    pub(crate) fn show_program(&mut self, ui: &mut egui::Ui) {
        let event_comps = self.event_comps();
        let action_comps = self.action_comps();
        let now = self.live.now_ms();
        let recent: Vec<usize> =
            self.live.engine.firings.iter().filter(|f| now.saturating_sub(f.at_ms) < 500).map(|f| f.rule_idx).collect();
        let fired_fill = egui_theme_kit::success(ui).gamma_multiply(0.12);
        let gap = egui_theme_kit::metrics(ui.ctx()).gap.y;
        let mut remove: Option<usize> = None;
        for idx in 0..self.project.program.rules.len() {
            let mut frame = controls::card_frame(ui);
            if recent.contains(&idx) {
                frame = frame.fill(fired_fill);
            }
            frame.show(ui, |ui| {
                let rule = &mut self.project.program.rules[idx];
                ui.horizontal(|ui| {
                    controls::toggle(ui, &mut rule.enabled, "");
                    controls::TextField::new(&mut rule.name).hint("Rule name").width(220.0).show(ui);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui_kit::row_action(ui, ph::TRASH, "Delete rule", ui.visuals().error_fg_color).clicked() {
                            remove = Some(idx);
                        }
                    });
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("When").strong());
                    Self::trigger_editor(ui, idx, &mut rule.trigger, &event_comps);
                });
                let mut remove_action: Option<usize> = None;
                for (ai, action) in rule.actions.iter_mut().enumerate() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(if ai == 0 { "Do" } else { "Then" }).strong());
                        Self::action_editor(ui, idx, ai, action, &action_comps);
                        if ui_kit::row_action(ui, ph::X, "Remove this action", ui.visuals().weak_text_color()).clicked() {
                            remove_action = Some(ai);
                        }
                    });
                }
                if let Some(ai) = remove_action {
                    rule.actions.remove(ai);
                }
                if ui_kit::ghost(ui, ph::PLUS, "Add action").clicked() {
                    rule.actions.push(default_action(&action_comps));
                }
            });
            ui.add_space(gap);
        }
        if let Some(idx) = remove {
            self.project.program.rules.remove(idx);
        }
    }

    fn trigger_editor(
        ui: &mut egui::Ui,
        idx: usize,
        trigger: &mut Trigger,
        event_comps: &[(CompId, String, Vec<String>)],
    ) {
        let kind_name = match trigger {
            Trigger::CompEvent { .. } => "component event",
            Trigger::PinRises { .. } => "pin rises",
            Trigger::PinFalls { .. } => "pin falls",
            Trigger::AnalogAbove { .. } => "analog above",
            Trigger::AnalogBelow { .. } => "analog below",
            Trigger::Every { .. } => "every",
            Trigger::OnStart => "program starts",
        };
        ComboBox::from_id_salt(("trig-kind", idx))
            .selected_text(kind_name)
            .show_ui(ui, |ui| {
                if ui.selectable_label(false, "component event").clicked() {
                    let (comp, event) = event_comps
                        .first()
                        .map(|(id, _, evs)| (*id, evs[0].clone()))
                        .unwrap_or((CompId(0), "pressed".into()));
                    *trigger = Trigger::CompEvent { comp, event };
                }
                if ui.selectable_label(false, "pin rises").clicked() {
                    *trigger = Trigger::PinRises { gpio: 0 };
                }
                if ui.selectable_label(false, "pin falls").clicked() {
                    *trigger = Trigger::PinFalls { gpio: 0 };
                }
                if ui.selectable_label(false, "analog above").clicked() {
                    *trigger = Trigger::AnalogAbove { gpio: 0, millivolts: 1650 };
                }
                if ui.selectable_label(false, "analog below").clicked() {
                    *trigger = Trigger::AnalogBelow { gpio: 0, millivolts: 1650 };
                }
                if ui.selectable_label(false, "every").clicked() {
                    *trigger = Trigger::Every { ms: 1000 };
                }
                if ui.selectable_label(false, "program starts").clicked() {
                    *trigger = Trigger::OnStart;
                }
            });
        match trigger {
            Trigger::CompEvent { comp, event } => {
                let name = event_comps
                    .iter()
                    .find(|(id, _, _)| id == comp)
                    .map(|(_, n, _)| n.clone())
                    .unwrap_or_else(|| format!("#{}", comp.0));
                ComboBox::from_id_salt(("trig-comp", idx))
                    .selected_text(name)
                    .show_ui(ui, |ui| {
                        for (id, n, _) in event_comps {
                            ui.selectable_value(comp, *id, n);
                        }
                    });
                let events = event_comps
                    .iter()
                    .find(|(id, _, _)| id == comp)
                    .map(|(_, _, e)| e.clone())
                    .unwrap_or_default();
                ComboBox::from_id_salt(("trig-ev", idx))
                    .selected_text(event.clone())
                    .show_ui(ui, |ui| {
                        for e in &events {
                            ui.selectable_value(event, e.clone(), e);
                        }
                    });
            }
            Trigger::PinRises { gpio } | Trigger::PinFalls { gpio } => {
                ui.label("GPIO");
                ui.add(DragValue::new(gpio).range(0..=48));
            }
            Trigger::AnalogAbove { gpio, millivolts } | Trigger::AnalogBelow { gpio, millivolts } => {
                ui.label("GPIO");
                ui.add(DragValue::new(gpio).range(0..=48));
                ui.label("mV");
                ui.add(DragValue::new(millivolts).range(0..=3300));
            }
            Trigger::Every { ms } => {
                ui.add(DragValue::new(ms).range(20..=600_000).suffix(" ms"));
            }
            Trigger::OnStart => {}
        }
    }

    fn action_editor(
        ui: &mut egui::Ui,
        rule_idx: usize,
        action_idx: usize,
        action: &mut Action,
        action_comps: &[(CompId, String, Vec<String>)],
    ) {
        let salt = (rule_idx, action_idx);
        let kind_name = match action {
            Action::CompAction { .. } => "component",
            Action::SetPin { .. } => "set pin",
            Action::TogglePin { .. } => "toggle pin",
            Action::SetPwm { .. } => "set pwm",
            Action::Wait { .. } => "wait",
            Action::Log { .. } => "log",
            Action::SetPinMode { .. } => "pin mode",
            Action::SetRgb { .. } => "rgb led",
            Action::WatchAnalog { .. } => "watch analog",
            Action::UartConfig { .. } => "uart config",
            Action::UartWrite { .. } => "uart send",
            Action::SpiConfig { .. } => "spi config",
            Action::SpiTransfer { .. } => "spi transfer",
            Action::I2cConfig { .. } => "i2c config",
            Action::I2cWrite { .. } => "i2c write",
            Action::I2cRead { .. } => "i2c read",
            Action::LcdInit { .. } => "lcd init",
            Action::LcdClear { .. } => "lcd clear",
            Action::LcdRect { .. } => "lcd rect",
            Action::LcdText { .. } => "lcd text",
            Action::BoardMsg { .. } => "board msg",
            Action::HttpGet { .. } => "http get",
        };
        ComboBox::from_id_salt(("act-kind", salt))
            .selected_text(kind_name)
            .show_ui(ui, |ui| {
                if ui.selectable_label(false, "component").clicked() {
                    *action = default_action(action_comps);
                }
                if ui.selectable_label(false, "set pin").clicked() {
                    *action = Action::SetPin { gpio: 2, high: true };
                }
                if ui.selectable_label(false, "toggle pin").clicked() {
                    *action = Action::TogglePin { gpio: 2 };
                }
                if ui.selectable_label(false, "set pwm").clicked() {
                    *action = Action::SetPwm { gpio: 2, freq_hz: 1000, duty_permille: 500 };
                }
                if ui.selectable_label(false, "wait").clicked() {
                    *action = Action::Wait { ms: 500 };
                }
                if ui.selectable_label(false, "log").clicked() {
                    *action = Action::Log { text: "hello".into() };
                }
                if ui.selectable_label(false, "pin mode").clicked() {
                    *action = Action::SetPinMode {
                        gpio: 28,
                        mode: wirelab_proto::PinMode::InputPullUp,
                    };
                }
                if ui.selectable_label(false, "rgb led").clicked() {
                    *action = Action::SetRgb { gpio: 27, r: 64, g: 0, b: 64 };
                }
            });
        match action {
            Action::CompAction { comp, action: verb, params } => {
                let name = action_comps
                    .iter()
                    .find(|(id, _, _)| id == comp)
                    .map(|(_, n, _)| n.clone())
                    .unwrap_or_else(|| format!("#{}", comp.0));
                ComboBox::from_id_salt(("act-comp", salt))
                    .selected_text(name)
                    .show_ui(ui, |ui| {
                        for (id, n, _) in action_comps {
                            ui.selectable_value(comp, *id, n);
                        }
                    });
                let verbs = action_comps
                    .iter()
                    .find(|(id, _, _)| id == comp)
                    .map(|(_, _, v)| v.clone())
                    .unwrap_or_default();
                ComboBox::from_id_salt(("act-verb", salt))
                    .selected_text(verb.clone())
                    .show_ui(ui, |ui| {
                        for v in &verbs {
                            ui.selectable_value(verb, v.clone(), v);
                        }
                    });
                // Common tunables per verb.
                match verb.as_str() {
                    "blink" | "breathe" => {
                        let mut v = params.get("period_ms").copied().unwrap_or(500.0);
                        ui.label("period");
                        if ui.add(DragValue::new(&mut v).range(40.0..=10000.0).suffix(" ms")).changed() {
                            params.insert("period_ms".into(), v);
                        }
                    }
                    "dim" => {
                        let mut v = params.get("percent").copied().unwrap_or(50.0);
                        if ui.add(DragValue::new(&mut v).range(0.0..=100.0).suffix(" %")).changed() {
                            params.insert("percent".into(), v);
                        }
                    }
                    "set_angle" => {
                        let mut v = params.get("degrees").copied().unwrap_or(90.0);
                        if ui.add(DragValue::new(&mut v).range(0.0..=180.0).suffix(" °")).changed() {
                            params.insert("degrees".into(), v);
                        }
                    }
                    "beep" => {
                        let mut v = params.get("ms").copied().unwrap_or(200.0);
                        if ui.add(DragValue::new(&mut v).range(10.0..=5000.0).suffix(" ms")).changed() {
                            params.insert("ms".into(), v);
                        }
                    }
                    "tone" => {
                        let mut f = params.get("freq_hz").copied().unwrap_or(880.0);
                        let mut d = params.get("ms").copied().unwrap_or(300.0);
                        if ui.add(DragValue::new(&mut f).range(20.0..=20000.0).suffix(" Hz")).changed() {
                            params.insert("freq_hz".into(), f);
                        }
                        if ui.add(DragValue::new(&mut d).range(10.0..=10000.0).suffix(" ms")).changed() {
                            params.insert("ms".into(), d);
                        }
                    }
                    _ => {}
                }
            }
            Action::SetPin { gpio, high } => {
                ui.label("GPIO");
                ui.add(DragValue::new(gpio).range(0..=48));
                ui.checkbox(high, "high");
            }
            Action::TogglePin { gpio } => {
                ui.label("GPIO");
                ui.add(DragValue::new(gpio).range(0..=48));
            }
            Action::SetPwm { gpio, freq_hz, duty_permille } => {
                ui.label("GPIO");
                ui.add(DragValue::new(gpio).range(0..=48));
                ui.add(DragValue::new(freq_hz).range(1..=40000).suffix(" Hz"));
                ui.add(DragValue::new(duty_permille).range(0..=1000).suffix(" ‰"));
            }
            Action::Wait { ms } => {
                ui.add(DragValue::new(ms).range(1..=600_000).suffix(" ms"));
            }
            Action::Log { text } => {
                ui.text_edit_singleline(text);
            }
            Action::SetPinMode { gpio, mode } => {
                ui.label("GPIO");
                ui.add(DragValue::new(gpio).range(0..=48));
                use wirelab_proto::PinMode;
                ComboBox::from_id_salt(("act-mode", salt))
                    .selected_text(format!("{mode:?}"))
                    .show_ui(ui, |ui| {
                        for m in [
                            PinMode::Input,
                            PinMode::InputPullUp,
                            PinMode::InputPullDown,
                            PinMode::Output,
                            PinMode::Pwm,
                            PinMode::Analog,
                        ] {
                            ui.selectable_value(mode, m, format!("{m:?}"));
                        }
                    });
            }
            Action::WatchAnalog { gpio, interval_ms } => {
                ui.label("GPIO");
                ui.add(DragValue::new(gpio).range(0..=48));
                ui.add(DragValue::new(interval_ms).range(0..=60000).suffix(" ms"));
            }
            Action::UartConfig { tx, rx, baud } => {
                ui.label("TX");
                ui.add(DragValue::new(tx).range(0..=48));
                ui.label("RX");
                ui.add(DragValue::new(rx).range(0..=48));
                ui.add(DragValue::new(baud).range(300..=921600).suffix(" baud"));
            }
            Action::UartWrite { data } => {
                let mut text = String::from_utf8_lossy(data).to_string();
                if ui.text_edit_singleline(&mut text).changed() {
                    *data = text.into_bytes();
                }
            }
            Action::SpiConfig { sck, mosi, miso, freq_khz } => {
                for (label, v) in [("SCK", sck), ("MOSI", mosi), ("MISO", miso)] {
                    ui.label(label);
                    ui.add(DragValue::new(v).range(0..=48));
                }
                ui.add(DragValue::new(freq_khz).range(1..=40000).suffix(" kHz"));
            }
            Action::SpiTransfer { cs, data } => {
                ui.label("CS");
                ui.add(DragValue::new(cs).range(0..=48));
                ui.label(format!("{} bytes", data.len()));
            }
            Action::I2cConfig { sda, scl, freq_khz } => {
                for (label, v) in [("SDA", sda), ("SCL", scl)] {
                    ui.label(label);
                    ui.add(DragValue::new(v).range(0..=48));
                }
                ui.add(DragValue::new(freq_khz).range(1..=1000).suffix(" kHz"));
            }
            Action::I2cWrite { addr, data } => {
                ui.label("addr");
                ui.add(DragValue::new(addr).range(0..=127));
                ui.label(format!("{} bytes", data.len()));
            }
            Action::I2cRead { addr, reg, len } => {
                ui.label("addr");
                ui.add(DragValue::new(addr).range(0..=127));
                ui.label("reg");
                ui.add(DragValue::new(reg).range(0..=256));
                ui.label("len");
                ui.add(DragValue::new(len).range(1..=48));
            }
            Action::BoardMsg { to, text } => {
                ui.label("board");
                ui.add(egui::TextEdit::singleline(to).desired_width(80.0));
                ui.label("text");
                ui.add(egui::TextEdit::singleline(text).desired_width(120.0));
            }
            Action::HttpGet { url } => {
                ui.label("url");
                ui.add(egui::TextEdit::singleline(url).desired_width(200.0));
            }
            Action::LcdInit { sck, mosi, cs, dc, rst, .. } => {
                for (label, v) in
                    [("SCK", sck), ("MOSI", mosi), ("CS", cs), ("DC", dc), ("RST", rst)]
                {
                    ui.label(label);
                    ui.add(DragValue::new(v).range(0..=48));
                }
            }
            Action::LcdClear { rgb } | Action::LcdRect { rgb, .. } | Action::LcdText { rgb, .. } => {
                let mut color =
                    [rgb[0] as f32 / 255.0, rgb[1] as f32 / 255.0, rgb[2] as f32 / 255.0];
                if ui.color_edit_button_rgb(&mut color).changed() {
                    *rgb = [
                        (color[0] * 255.0) as u8,
                        (color[1] * 255.0) as u8,
                        (color[2] * 255.0) as u8,
                    ];
                }
            }
            Action::SetRgb { gpio, r, g, b } => {
                ui.label("GPIO");
                ui.add(DragValue::new(gpio).range(0..=48));
                let mut color = [
                    *r as f32 / 255.0,
                    *g as f32 / 255.0,
                    *b as f32 / 255.0,
                ];
                if ui.color_edit_button_rgb(&mut color).changed() {
                    *r = (color[0] * 255.0) as u8;
                    *g = (color[1] * 255.0) as u8;
                    *b = (color[2] * 255.0) as u8;
                }
            }
        }
    }
}

fn default_action(action_comps: &[(CompId, String, Vec<String>)]) -> Action {
    action_comps
        .first()
        .map(|(id, _, verbs)| Action::CompAction {
            comp: *id,
            action: verbs.first().cloned().unwrap_or_else(|| "toggle".into()),
            params: Default::default(),
        })
        .unwrap_or(Action::Log { text: "no components with actions yet".into() })
}
