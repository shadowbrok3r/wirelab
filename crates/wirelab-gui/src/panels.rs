//! Side panels: palette, inspector, program editor, console, device bar.

use egui::{Color32, ComboBox, DragValue, RichText, ScrollArea};
use wirelab_core::circuit::CompId;
use wirelab_core::component::{CompState, SimModel};
use wirelab_core::program::{Action, Rule, Trigger};
use wirelab_core::validate::Severity;
use wirelab_link::SessionPhase;
use wirelab_link::discovery::Discovery;
use wirelab_proto::WifiState;

use crate::app::{Selection, WireLabApp};
use crate::live::Backend;

impl WireLabApp {
    pub fn show_palette(&mut self, ui: &mut egui::Ui) {
        ui.heading("Board");
        let current = self.project.circuit.board_id.clone();
        let mut chosen = current.clone();
        ComboBox::from_id_salt("board-pick")
            .width(ui.available_width() - 8.0)
            .selected_text(
                self.lib
                    .board(&current)
                    .map(|b| b.name.clone())
                    .unwrap_or_else(|| current.clone()),
            )
            .show_ui(ui, |ui| {
                for (id, b) in &self.lib.boards {
                    ui.selectable_value(&mut chosen, id.clone(), &b.name);
                }
            });
        if chosen != current {
            if self.live.connected() {
                self.live.disconnect(&mut self.console);
                self.console.push("disconnected: board changed".into());
            }
            self.project.circuit.board_id = chosen;
            self.topo_rev += 1;
        }
        if let Some(board) = self.lib.board(&self.project.circuit.board_id) {
            ui.label(
                RichText::new(format!("{} · {} pins", board.chip.name(), board.pins.len()))
                    .small()
                    .color(Color32::from_gray(140)),
            );
            if !board.specs.is_empty() {
                egui::CollapsingHeader::new(RichText::new("capabilities").small())
                    .default_open(false)
                    .show(ui, |ui| {
                        for s in &board.specs {
                            ui.label(RichText::new(s).small().color(Color32::from_gray(170)));
                        }
                        ui.label(
                            RichText::new("scripts: board_has(\"wifi\"), chip()")
                                .small()
                                .color(Color32::from_gray(120)),
                        );
                    });
            }
            let f = &board.features;
            if f.rgb_led_gpio.is_some() || f.reset_button || f.boot_button_gpio.is_some() {
                let mut bits = Vec::new();
                if let Some(g) = f.rgb_led_gpio {
                    bits.push(format!("RGB LED (GPIO{g})"));
                }
                if f.reset_button {
                    bits.push("RESET".into());
                }
                if f.boot_button_gpio.is_some() {
                    bits.push("BOOT".into());
                }
                ui.label(
                    RichText::new(format!("on board: {}", bits.join(" · ")))
                        .small()
                        .color(Color32::from_gray(140)),
                );
            }
        }
        ui.add_space(8.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.heading("Components");
            if ui
                .small_button("🔌 wiring guide")
                .on_hover_text("circuits 101: LEDs & resistors, pull-ups, dividers…")
                .clicked()
            {
                self.wiring_open = !self.wiring_open;
            }
        });
        ui.label(
            RichText::new("click, then click the canvas to place")
                .small()
                .color(Color32::from_gray(140)),
        );
        ui.add_space(4.0);

        let mut categories: Vec<String> = self
            .lib
            .components
            .values()
            .map(|c| c.category.clone())
            .collect();
        categories.sort();
        categories.dedup();

        ScrollArea::vertical().id_salt("palette").show(ui, |ui| {
            for cat in categories {
                let n = self.lib.components.values().filter(|c| c.category == cat).count();
                egui::CollapsingHeader::new(
                    RichText::new(format!("{cat} ({n})")).strong(),
                )
                .default_open(true)
                .show(ui, |ui| {
                    for def in self.lib.components.values().filter(|c| c.category == cat) {
                        let active = self.canvas.placing.as_deref() == Some(def.id.as_str());
                        // Trailing grow-atom pushes the label to the left edge.
                        let btn = ui.add_sized(
                            [ui.available_width() - 8.0, 22.0],
                            egui::Button::selectable(
                                active,
                                (RichText::new(&def.name), egui::Atom::grow()),
                            ),
                        );
                        if btn.clicked() {
                            self.canvas.placing =
                                if active { None } else { Some(def.id.clone()) };
                            self.canvas.wire_from = None;
                        }
                        if btn.hovered() && !def.description.is_empty() {
                            btn.on_hover_text(&def.description);
                        }
                    }
                });
            }
        });
    }

    /// Right side: inspector, then the script editor, then the rules program.
    pub fn show_right_panel(&mut self, ui: &mut egui::Ui) {
        ScrollArea::vertical().id_salt("right-panel").show(ui, |ui| {
            egui::CollapsingHeader::new(RichText::new("🗺 Overview").strong())
                .default_open(false)
                .show(ui, |ui| self.show_overview(ui));
            ui.add_space(4.0);
            self.show_inspector(ui);
            ui.add_space(4.0);
            if ui
                .button("⚙ Rules & scripts moved to the IDE →")
                .on_hover_text("Tools → Open IDE, or click here")
                .clicked()
            {
                self.open_program_tab();
            }
        });
    }

    /// Where is the logic? Every component with its script/rule/pin badges.
    fn show_overview(&mut self, ui: &mut egui::Ui) {
        let names = wirelab_core::script::component_names(&self.project.circuit, &self.lib);
        let mut rows: Vec<(CompId, String, String)> = Vec::new();
        for comp in self.project.circuit.components.values() {
            let name = names.get(&comp.id).cloned().unwrap_or_default();
            let mut badges = Vec::new();
            if comp.script.is_some() {
                let state = if self.live.scripts.errors.contains_key(&comp.id) {
                    "📜✖"
                } else {
                    "📜"
                };
                badges.push(state.to_string());
            }
            let rules = self
                .project
                .program
                .rules
                .iter()
                .filter(|r| {
                    let in_trigger = matches!(
                        &r.trigger,
                        wirelab_core::program::Trigger::CompEvent { comp: c, .. } if *c == comp.id
                    );
                    let in_actions = r.actions.iter().any(|a| {
                        matches!(a, wirelab_core::program::Action::CompAction { comp: c, .. } if *c == comp.id)
                    });
                    in_trigger || in_actions
                })
                .count();
            if rules > 0 {
                badges.push(format!("⚙{rules}"));
            }
            if let Some(g) = self.cache.bindings.gpio_of(comp.id) {
                badges.push(format!("GPIO{g}"));
            }
            rows.push((comp.id, name, badges.join("  ")));
        }
        // Logic-bearing components first, then alphabetical.
        rows.sort_by(|a, b| {
            let logic = |s: &String| !s.contains("📜") && !s.contains('⚙');
            logic(&a.2).cmp(&logic(&b.2)).then(a.1.cmp(&b.1))
        });
        if rows.is_empty() {
            ui.label(RichText::new("no components yet").small().color(Color32::from_gray(120)));
        }
        for (id, name, badges) in rows {
            ui.horizontal(|ui| {
                let selected = self.selection.contains_comp(id);
                if ui
                    .add(egui::Button::selectable(
                        selected,
                        (RichText::new(&name).monospace().small(), egui::Atom::grow()),
                    ))
                    .clicked()
                {
                    self.selection = Selection::Comp(id);
                    if self
                        .project
                        .circuit
                        .components
                        .get(&id)
                        .is_some_and(|c| c.script.is_some())
                    {
                        self.open_script_tab(id);
                    }
                }
                if !badges.is_empty() {
                    ui.label(RichText::new(badges).small().color(Color32::from_gray(170)));
                }
            });
        }
        let n_rules = self.project.program.rules.len();
        if n_rules > 0 {
            ui.label(
                RichText::new(format!("program: {n_rules} rule(s)"))
                    .small()
                    .color(Color32::from_gray(140)),
            );
        }
    }

    pub fn show_inspector(&mut self, ui: &mut egui::Ui) {
        ui.heading("Inspector");
        ui.add_space(4.0);
        match self.selection.clone() {
            Selection::Comp(id) => self.inspect_component(ui, id),
            Selection::Comps(ids) => {
                ui.label(RichText::new(format!("{} components selected", ids.len())).strong());
                for id in ids.iter().take(8) {
                    let name = self
                        .project
                        .circuit
                        .components
                        .get(id)
                        .map(|c| {
                            if c.label.is_empty() {
                                self.lib
                                    .component(&c.def_id)
                                    .map(|d| d.name.clone())
                                    .unwrap_or_default()
                            } else {
                                c.label.clone()
                            }
                        })
                        .unwrap_or_default();
                    ui.label(RichText::new(format!("· {name}")).weak());
                }
                if ids.len() > 8 {
                    ui.label(RichText::new(format!("· … {} more", ids.len() - 8)).weak());
                }
                ui.add_space(4.0);
                if ui.button("⚡ Auto wire selection").clicked() {
                    self.apply_auto_wire();
                }
                if ui.button("clear selection").clicked() {
                    self.selection = Selection::None;
                }
            }
            Selection::Wire(id) => {
                ui.label(RichText::new(format!("Wire #{}", id.0)).strong());
                if let Some(w) = self.project.circuit.wires.get(&id) {
                    let names = wirelab_core::script::component_names(
                        &self.project.circuit,
                        &self.lib,
                    );
                    let describe = |ep: &wirelab_core::circuit::Endpoint| match ep {
                        wirelab_core::circuit::Endpoint::BoardPin { key } => key.clone(),
                        wirelab_core::circuit::Endpoint::Terminal { comp, terminal } => {
                            format!(
                                "{}.{terminal}",
                                names.get(comp).cloned().unwrap_or_else(|| comp.0.to_string())
                            )
                        }
                    };
                    ui.label(format!("{}  →  {}", describe(&w.a), describe(&w.b)));
                }
                if ui.button("Delete wire").clicked() {
                    self.project.circuit.remove_wire(id);
                    self.selection = Selection::None;
                    self.topo_rev += 1;
                }
            }
            Selection::Pin(key) => {
                let Some(board) = self.lib.board(&self.project.circuit.board_id) else { return };
                let Some(pin) = board.pin(&key).cloned() else { return };
                ui.label(RichText::new(&pin.key).strong());
                ui.label(format!("kind: {:?}", pin.kind));
                if !pin.caps.is_empty() {
                    ui.label(format!("{:?}", pin.caps));
                }
                if let Some((unit, ch)) = pin.adc {
                    ui.label(format!("ADC{unit}_CH{ch}"));
                }
                if let Some(w) = &pin.warning {
                    let warn = ui.visuals().warn_fg_color;
                    ui.colored_label(warn, format!("⚠ {w}"));
                }
            }
            Selection::None => {
                ui.label(
                    RichText::new("Nothing selected")
                        .small()
                        .color(Color32::from_gray(120)),
                );
            }
        }

        ui.add_space(8.0);
        ui.separator();
        ui.heading("Checks");
        let warn_color = ui.visuals().warn_fg_color;
        let error_color = ui.visuals().error_fg_color;
        let info_color = ui.visuals().weak_text_color();
        let lints = self.cache.lints.clone();
        let mut apply_fix: Option<wirelab_core::validate::LintFix> = None;
        ScrollArea::vertical().id_salt("checks").max_height(180.0).show(ui, |ui| {
            for w in &self.cache.bindings.warnings {
                ui.colored_label(warn_color, format!("⚠ {w}"));
            }
            for lint in &lints {
                let (color, icon) = match lint.severity {
                    Severity::Error => (error_color, "✖"),
                    Severity::Warning => (warn_color, "⚠"),
                    Severity::Info => (info_color, "ℹ"),
                };
                let row = ui.colored_label(color, format!("{icon} {}", lint.message));
                let mut hovered = row.hovered();
                if let Some(fix) = &lint.fix {
                    let label = match fix {
                        wirelab_core::validate::LintFix::SpliceResistor { action, .. } => {
                            format!("🔧 {action}")
                        }
                    };
                    ui.indent(("fix", &lint.message), |ui| {
                        let btn = ui.small_button(label).on_hover_text(
                            "WireLab places the nearest stock resistor and splices it into the wire",
                        );
                        hovered |= btn.hovered();
                        if btn.clicked() {
                            apply_fix = Some(fix.clone());
                        }
                    });
                }
                if hovered && !lint.comps.is_empty() {
                    self.hover_highlight = lint.comps.clone();
                }
            }
            if let Some(out) = &self.live.live_output {
                for w in &out.warnings {
                    ui.colored_label(error_color, format!("✖ {w}"));
                }
            }
            if lints.is_empty() && self.cache.bindings.warnings.is_empty() {
                ui.label(RichText::new("all clear").small().color(Color32::from_gray(120)));
            }
        });
        if let Some(fix) = apply_fix {
            self.apply_lint_fix(&fix);
        }
    }

    fn inspect_component(&mut self, ui: &mut egui::Ui, id: CompId) {
        let Some(def) = self
            .project
            .circuit
            .components
            .get(&id)
            .and_then(|c| self.lib.component(&c.def_id))
            .cloned()
        else {
            return;
        };
        let gpio = self.cache.bindings.gpio_of(id);
        let Some(comp) = self.project.circuit.components.get_mut(&id) else { return };

        ui.label(RichText::new(&def.name).strong());
        if !def.description.is_empty() {
            ui.label(
                RichText::new(&def.description).small().color(Color32::from_gray(160)),
            );
            ui.add_space(2.0);
        }
        match gpio {
            Some(g) => ui.label(format!("bound to GPIO{g}")),
            None => ui.label(RichText::new("not bound to a GPIO").color(Color32::from_gray(140))),
        };
        let has_script = comp.script.is_some();
        ui.horizontal(|ui| {
            ui.label("label");
            ui.text_edit_singleline(&mut comp.label);
        });
        let mut open_script = false;
        ui.horizontal(|ui| {
            let tag = if has_script { "📜 Edit script" } else { "📜 Attach script" };
            if ui.button(tag).clicked() {
                open_script = true;
            }
            if has_script {
                if let Some(err) = self.live.scripts.errors.get(&id) {
                    ui.label(RichText::new("✖").color(ui.visuals().error_fg_color))
                        .on_hover_text(err);
                } else if self.live.scripts.has_script(id) {
                    ui.label(RichText::new("✔").color(Color32::from_rgb(90, 220, 120)))
                        .on_hover_text("script compiled and live");
                }
            }
        });
        let (mut rotate, mut delete) = (false, false);
        ui.horizontal(|ui| {
            rotate = ui.button("rotate").clicked();
            delete = ui.button("delete").clicked();
        });
        if rotate {
            comp.rotation = (comp.rotation + 90) % 360;
            self.topo_rev += 1;
        }
        if delete {
            self.project.circuit.remove_component(id);
            self.selection = Selection::None;
            self.topo_rev += 1;
            return;
        }
        let Some(comp) = self.project.circuit.components.get_mut(&id) else { return };

        for prop in &def.props {
            let mut v = comp.props.get(&prop.key).copied().unwrap_or(prop.default);
            ui.horizontal(|ui| {
                ui.label(&prop.name);
                if ui
                    .add(DragValue::new(&mut v).range(prop.min..=prop.max).speed((prop.max - prop.min) / 200.0))
                    .changed()
                {
                    comp.props.insert(prop.key.clone(), v);
                    self.state_rev += 1;
                }
            });
        }

        // Live pokes for stateful parts.
        match (&def.sim, &mut comp.state) {
            (SimModel::Potentiometer { .. }, CompState::Fraction { value }) => {
                ui.label("wiper");
                if ui.add(egui::Slider::new(value, 0.0..=1.0)).changed() {
                    self.state_rev += 1;
                }
            }
            (SimModel::Photoresistor { .. }, CompState::Fraction { value }) => {
                ui.label("light level");
                if ui.add(egui::Slider::new(value, 0.0..=1.0)).changed() {
                    self.state_rev += 1;
                }
            }
            (SimModel::AnalogSensor { .. }, CompState::Fraction { value }) => {
                ui.label("sensor level");
                if ui.add(egui::Slider::new(value, 0.0..=1.0)).changed() {
                    self.state_rev += 1;
                }
            }
            (SimModel::ToggleSwitch | SimModel::SlideSwitchSpdt | SimModel::DigitalSensor, CompState::Toggle { on }) => {
                if ui.checkbox(on, "on").changed() {
                    self.state_rev += 1;
                }
            }
            (SimModel::PushButton, CompState::Button { pressed }) => {
                ui.label(if *pressed { "pressed (hold it on the canvas)" } else { "released" });
            }
            _ => {}
        }

        if open_script {
            if !has_script {
                self.attach_template_script(id);
            }
            self.open_script_tab(id);
        }
    }

    pub fn show_device_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let connected = self.live.connected();
            if !connected {
                ComboBox::from_id_salt("backend")
                    .selected_text(match self.live.backend {
                        Backend::Simulator => "Simulator",
                        Backend::Serial => "Serial",
                        Backend::Tcp => "Wi-Fi (TCP)",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.live.backend, Backend::Simulator, "Simulator");
                        ui.selectable_value(&mut self.live.backend, Backend::Serial, "Serial");
                        ui.selectable_value(&mut self.live.backend, Backend::Tcp, "Wi-Fi (TCP)");
                    });
                if self.live.backend == Backend::Serial {
                    let selected = self
                        .live
                        .ports
                        .get(self.live.selected_port)
                        .cloned()
                        .unwrap_or_else(|| "no ports".into());
                    ComboBox::from_id_salt("port")
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            for (i, p) in self.live.ports.clone().iter().enumerate() {
                                ui.selectable_value(&mut self.live.selected_port, i, p);
                            }
                        });
                    if ui.button("⟳").on_hover_text("rescan ports").clicked() {
                        self.live.refresh_ports();
                    }
                    let flashing = self.flash.running();
                    let flash_label = if flashing {
                        format!("{} flashing…", egui_phosphor::regular::LIGHTNING)
                    } else {
                        format!("{} Flash firmware", egui_phosphor::regular::LIGHTNING)
                    };
                    if ui
                        .add_enabled(!flashing, egui::Button::new(flash_label))
                        .on_hover_text("build the WireLab firmware for this board's chip and espflash it onto the selected port")
                        .clicked()
                    {
                        let chip = self
                            .lib
                            .board(&self.project.circuit.board_id)
                            .map(|b| b.chip);
                        let port = self.live.ports.get(self.live.selected_port).cloned();
                        match (chip, port) {
                            (Some(chip), Some(port)) => {
                                self.flash.start(chip, &port, &mut self.console);
                            }
                            _ => self.console.push("pick a board and a port first".into()),
                        }
                    }
                    if flashing {
                        ui.spinner();
                    }
                }
                if self.live.backend == Backend::Tcp {
                    // The beacon listener runs while the TCP backend is selected.
                    let disco = self.live.discovery.get_or_insert_with(Discovery::listen);
                    let boards = disco.boards();
                    let err = disco.error();
                    let selected = if self.live.tcp_addr.is_empty() {
                        "boards on the network".to_string()
                    } else {
                        self.live.tcp_addr.clone()
                    };
                    ComboBox::from_id_salt("tcp-board")
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            if boards.is_empty() {
                                ui.label(RichText::new("listening for beacons…").weak());
                            }
                            for b in &boards {
                                ui.selectable_value(
                                    &mut self.live.tcp_addr,
                                    b.addr.clone(),
                                    format!("{} ({})", b.addr, b.chip),
                                );
                            }
                        })
                        .response
                        .on_hover_text(
                            "boards announce themselves over UDP once their Wi-Fi is set up \
                             (Inspector → board → Wi-Fi); you can also type ip:port below",
                        );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.live.tcp_addr)
                            .hint_text("192.168.1.x:4518")
                            .desired_width(140.0),
                    );
                    if let Some(err) = err {
                        ui.label(RichText::new("!").color(Color32::from_rgb(255, 100, 90)))
                            .on_hover_text(format!("discovery listener: {err}"));
                    }
                } else if self.live.discovery.is_some() {
                    self.live.discovery = None;
                }
                if ui.button("Connect").clicked() {
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
            } else {
                let (phase, desc, chip) = {
                    let s = self.live.session.as_ref().unwrap();
                    (s.phase, s.device.description(), s.info.map(|i| i.chip))
                };
                use egui_phosphor::regular as icons;
                let status = match phase {
                    SessionPhase::Ready => RichText::new(format!("{} live", icons::PLUGS_CONNECTED))
                        .color(Color32::from_rgb(90, 220, 120)),
                    SessionPhase::AwaitingHello => {
                        RichText::new(format!("{} waiting", icons::HOURGLASS_MEDIUM))
                            .color(Color32::from_rgb(240, 200, 90))
                    }
                    SessionPhase::Dead => RichText::new(format!("{} dead", icons::PLUGS))
                        .color(Color32::from_rgb(255, 100, 90)),
                };
                ui.label(status);
                ui.label(desc);
                if let Some(chip) = chip {
                    ui.label(RichText::new(chip.name()).color(Color32::from_gray(160)));
                }
                self.wifi_menu(ui);
                if ui.button("Disconnect").clicked() {
                    self.live.disconnect(&mut self.console);
                }
            }

            ui.separator();
            let running = self.live.program_running;
            let can_run = self.live.connected() && !self.project.program.rules.is_empty();
            if !running {
                if ui
                    .add_enabled(can_run, egui::Button::new(RichText::new("▶ Run program").strong()))
                    .clicked()
                {
                    let program = self.project.program.clone();
                    let bindings = self.cache.bindings.clone();
                    self.live.start_program(&program, &bindings, &mut self.console);
                }
            } else if ui.button("⏹ Stop").clicked() {
                self.live.stop_program(&mut self.console);
            }
        });
    }

    /// Wi-Fi status + provisioning for the live board: join a network over
    /// the current link, then optionally hop the session onto TCP.
    fn wifi_menu(&mut self, ui: &mut egui::Ui) {
        use egui_phosphor::regular as icons;
        let wifi = self.live.session.as_ref().and_then(|s| s.wifi);
        let (icon_color, tip) = match wifi {
            Some((WifiState::Connected, ip)) => (
                Color32::from_rgb(90, 220, 120),
                format!("Wi-Fi: connected, {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]),
            ),
            Some((WifiState::Connecting, _)) => {
                (Color32::from_rgb(240, 200, 90), "Wi-Fi: connecting…".into())
            }
            Some((WifiState::Failed, _)) => {
                (Color32::from_rgb(255, 100, 90), "Wi-Fi: join failed".into())
            }
            Some((WifiState::Off, _)) | None => {
                (Color32::from_gray(140), "Wi-Fi: off — click to set up".into())
            }
        };
        let label = RichText::new(icons::WIFI_HIGH.to_string()).color(icon_color);
        // A Popup (not a menu) so typing in the SSID/password fields and clicking
        // Join/Forget don't dismiss it — only a click outside closes it.
        let btn = ui.button(label).on_hover_text(tip);
        egui::Popup::from_toggle_button_response(&btn)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
            ui.set_min_width(240.0);
            ui.label(RichText::new("Board Wi-Fi").strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.live.wifi_ssid)
                    .hint_text("network name (SSID)"),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.live.wifi_pass)
                    .hint_text("password")
                    .password(true),
            );
            ui.horizontal(|ui| {
                let can_join = !self.live.wifi_ssid.trim().is_empty();
                if ui.add_enabled(can_join, egui::Button::new("Join")).clicked() {
                    self.live.provision_wifi(&mut self.console);
                }
                if ui.button("Forget").on_hover_text("turn the radio off").clicked() {
                    self.live.wifi_ssid.clear();
                    if let Some(s) = &mut self.live.session {
                        let _ = s.send(&wirelab_proto::HostMsg::WifiConfig {
                            ssid: Default::default(),
                            pass: Default::default(),
                        });
                    }
                }
            });
            match wifi {
                Some((WifiState::Connected, ip)) => {
                    ui.separator();
                    ui.label(format!("board IP: {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]));
                    if self.live.backend != Backend::Tcp
                        && ui
                            .button(format!("{} Switch link to Wi-Fi", icons::SWAP))
                            .on_hover_text(
                                "close the serial session and reconnect to the board over TCP \
                                 — after this the USB cable is only power",
                            )
                            .clicked()
                    {
                        self.live.tcp_addr = format!(
                            "{}.{}.{}.{}:{}",
                            ip[0], ip[1], ip[2], ip[3],
                            wirelab_link::tcp::DEFAULT_TCP_PORT
                        );
                        self.live.disconnect(&mut self.console);
                        self.live.connect_tcp(&mut self.console);
                    }
                }
                Some((WifiState::Failed, _)) => {
                    ui.label(
                        RichText::new("join failed — check the SSID and password")
                            .color(Color32::from_rgb(255, 100, 90)),
                    );
                }
                _ => {}
            }
            ui.label(
                RichText::new("credentials live in board RAM; re-join after a reset")
                    .weak()
                    .small(),
            );
        });
    }

    /// The bottom panel: hardware tree on the left, tabbed IDE on the right.
    /// Main-window bottom strip: just the console (the IDE has its own).
    pub fn show_bottom(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Console").strong());
            if ui.small_button("clear").clicked() {
                self.console.clear();
            }
            if ui
                .small_button("📜 open IDE")
                .on_hover_text("scripts live in the IDE window")
                .clicked()
            {
                self.ide.open = true;
            }
        });
        ui.separator();
        self.show_console(ui);
    }


    pub(crate) fn show_script_cheat_sheet(
        &self,
        ui: &mut egui::Ui,
        sim: &SimModel,
        own: &str,
        names: &std::collections::HashMap<CompId, String>,
        me: CompId,
    ) {
        let kind_info = |sim: &SimModel| -> (&'static str, &'static str) {
            match sim {
                SimModel::PushButton => ("on_press() · on_release() · on_change(on)", "me.is_pressed()"),
                SimModel::ToggleSwitch | SimModel::SlideSwitchSpdt | SimModel::DigitalSensor => {
                    ("on_change(on)", "me.is_on()")
                }
                SimModel::Potentiometer { .. }
                | SimModel::Photoresistor { .. }
                | SimModel::AnalogSensor { .. } => ("on_reading(mv)", "me.millivolts()"),
                SimModel::Led { .. } => ("on_start() · on_tick(dt)", "me.on() .off() .toggle() .blink(ms) .breathe(ms) .dim(pct)"),
                SimModel::Buzzer { .. } => ("on_start() · on_tick(dt)", "me.beep(ms) .tone(hz, ms) .on() .off()"),
                SimModel::Servo => ("on_start() · on_tick(dt)", "me.set_angle(deg)"),
                SimModel::RelayModule => ("on_start() · on_tick(dt)", "me.on() .off() .toggle()"),
                SimModel::Resistor { .. } | SimModel::Generic => {
                    ("on_start() · on_tick(dt) · on_pin(gpio, high)", "rgb(r,g,b), pin(n)…")
                }
            }
        };
        egui::CollapsingHeader::new(RichText::new("❓ what can I write here?").small())
            .default_open(false)
            .show(ui, |ui| {
                let gray = Color32::from_gray(165);
                let (callbacks, mine) = kind_info(sim);
                ui.label(RichText::new(format!("you are `{own}` — state lives on `this`, your handle is `me`")).small().color(gray));
                ui.label(RichText::new(format!("callbacks: {callbacks} · always: on_start, on_tick, on_pin")).small().color(gray));
                ui.label(RichText::new(format!("yourself: {mine}")).small().color(gray));
                let mut sorted: Vec<(&CompId, &String)> =
                    names.iter().filter(|(c, _)| **c != me).collect();
                sorted.sort_by_key(|(_, n)| n.as_str());
                for (cid, n) in sorted.iter().take(8) {
                    let Some(sim) = self
                        .project
                        .circuit
                        .components
                        .get(cid)
                        .and_then(|c| self.lib.component(&c.def_id))
                        .map(|d| &d.sim)
                    else {
                        continue;
                    };
                    let (_, verbs) = kind_info(sim);
                    ui.label(
                        RichText::new(format!("{n}: {}", verbs.replace("me.", &format!("{n}."))))
                            .small()
                            .color(gray),
                    );
                }
                ui.label(
                    RichText::new("globals: log(x) · millis() · after(ms, ||…) · rgb(r,g,b) · pin(n) · board_has(\"wifi\")")
                        .small()
                        .color(gray),
                );
            });
    }

    /// Completion popup + hover docs for the script editor, LSP-style.
    pub(crate) fn script_completion_and_hover(
        &mut self,
        ui: &mut egui::Ui,
        names: &std::collections::HashMap<wirelab_core::circuit::CompId, String>,
        editor_id: egui::Id,
        accept: Option<usize>,
        out: egui::text_edit::TextEditOutput,
    ) {
        use crate::rhai_lint::{API_DOCS, CALLBACK_DOCS};
        use egui::text::{CCursor, CCursorRange};

        let chars: Vec<char> = self.script_ed.buffer.chars().collect();
        let is_ident = |c: char| c.is_alphanumeric() || c == '_';
        let char_to_byte = |buf: &str, ci: usize| -> usize {
            buf.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(buf.len())
        };

        // Accept via keyboard (Tab/Enter) or a click from last frame's popup.
        let mut apply: Option<usize> = accept;

        // Recompute candidates when the cursor or buffer moved.
        let focused = out.response.response.has_focus();
        let cursor_ci = out.cursor_range.map(|cr| cr.primary.index.0.min(chars.len()));
        let key = (cursor_ci.unwrap_or(usize::MAX), self.script_ed.lint_hash);
        if apply.is_none() && (key != self.script_ed.completion_key || !focused) {
            self.script_ed.completion_key = key;
            self.script_ed.completion = None;
            if focused && let Some(cr) = out.cursor_range {
                let ci = cr.primary.index.0.min(chars.len());
                let mut ws = ci;
                while ws > 0 && is_ident(chars[ws - 1]) {
                    ws -= 1;
                }
                let prefix: String = chars[ws..ci].iter().collect();
                let member = ws > 0 && chars[ws - 1] == '.';
                if member || !prefix.is_empty() {
                    let mut items: Vec<(String, String, usize, String)> = Vec::new();
                    for d in API_DOCS {
                        if d.member != member || d.name == "me" || !d.name.starts_with(&prefix)
                        {
                            continue;
                        }
                        let (insert, back) =
                            if d.args { (format!("{}()", d.name), 1) } else if d.member || d.sig.contains("()") {
                                (format!("{}()", d.name), 0)
                            } else {
                                (d.name.to_string(), 0)
                            };
                        items.push((d.name.to_string(), insert, back, format!("{} — {}", d.sig, d.doc)));
                    }
                    if !member {
                        if "me".starts_with(&prefix) && !prefix.is_empty() {
                            items.push(("me".into(), "me".into(), 0, "this component".into()));
                        }
                        let mut comp_names: Vec<&String> = names.values().collect();
                        comp_names.sort();
                        for n in comp_names {
                            if n.starts_with(&prefix) {
                                items.push((n.clone(), n.clone(), 0, "component".into()));
                            }
                        }
                    }
                    let exact_only =
                        items.len() == 1 && (items[0].0 == prefix || items[0].1 == prefix);
                    if !items.is_empty() && !exact_only {
                        items.truncate(10);
                        self.script_ed.completion =
                            Some(crate::app::Completion { items, selected: 0, word_start: ws });
                    }
                }
            }
        }

        // Popup, anchored under the word being completed.
        if apply.is_none()
            && let Some(c) = &self.script_ed.completion {
                let anchor = out.galley.pos_from_cursor(CCursor::new(c.word_start));
                let pos = out.galley_pos + anchor.left_bottom().to_vec2() + egui::vec2(0.0, 2.0);
                let mut clicked = None;
                egui::Area::new(editor_id.with("completion"))
                    .order(egui::Order::Foreground)
                    .fixed_pos(pos)
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            ui.set_max_width(360.0);
                            for (i, (label, _, _, detail)) in c.items.iter().enumerate() {
                                let sel = i == c.selected;
                                if ui
                                    .selectable_label(sel, RichText::new(label).monospace())
                                    .clicked()
                                {
                                    clicked = Some(i);
                                }
                                if sel {
                                    ui.label(RichText::new(detail).small().weak());
                                }
                            }
                            ui.label(
                                RichText::new(format!(
                                    "Tab/Enter accept · {}{} choose · Esc close",
                                    egui_phosphor::regular::ARROW_UP,
                                    egui_phosphor::regular::ARROW_DOWN,
                                ))
                                .small()
                                .color(Color32::from_gray(110)),
                            );
                        });
                    });
                apply = clicked;
            }

        // Insert the chosen completion and move the caret after it.
        if let Some(idx) = apply
            && let Some(c) = self.script_ed.completion.take()
                && let Some((_, insert, back, _)) = c.items.get(idx) {
                    let buf = self.script_ed.buffer.clone();
                    let mut we = c.word_start;
                    while we < chars.len() && is_ident(chars[we]) {
                        we += 1;
                    }
                    let (sb, eb) = (char_to_byte(&buf, c.word_start), char_to_byte(&buf, we));
                    self.script_ed.buffer.replace_range(sb..eb, insert);
                    let caret = c.word_start + insert.chars().count() - back;
                    let mut state = out.state.clone();
                    state
                        .cursor
                        .set_char_range(Some(CCursorRange::one(CCursor::new(caret))));
                    state.store(ui.ctx(), editor_id);
                    ui.ctx().request_repaint();
                    return;
                }

        // Hover docs: diagnostics first, then API / callbacks / components.
        if self.script_ed.completion.is_none()
            && out.response.response.hovered()
            && let Some(p) = ui.ctx().pointer_hover_pos() {
                let cc = out.galley.cursor_from_pos(p - out.galley_pos);
                let ci = cc.index.0.min(chars.len());
                let (mut ws, mut we) = (ci, ci);
                while ws > 0 && is_ident(chars[ws - 1]) {
                    ws -= 1;
                }
                while we < chars.len() && is_ident(chars[we]) {
                    we += 1;
                }
                if ws < we {
                    let word: String = chars[ws..we].iter().collect();
                    let byte = char_to_byte(&self.script_ed.buffer, ci);
                    let member = ws > 0 && chars[ws - 1] == '.';
                    let mut hover: Option<(String, String)> = None;
                    if let Some(d) =
                        self.script_ed.lint.iter().find(|d| d.start <= byte && byte < d.end)
                    {
                        hover = Some(("problem".into(), d.message.clone()));
                    } else if let Some(d) =
                        API_DOCS.iter().find(|d| d.name == word && d.member == member)
                    {
                        hover = Some((d.sig.to_string(), d.doc.to_string()));
                    } else if let Some((n, doc)) =
                        CALLBACK_DOCS.iter().find(|(n, _)| *n == word)
                    {
                        hover = Some((format!("fn {n}(...)"), doc.to_string()));
                    } else if names.values().any(|n| n == &word) {
                        hover = Some((word.clone(), "component in this circuit".into()));
                    }
                    if let Some((sig, doc)) = hover {
                        out.response.response.clone().on_hover_ui_at_pointer(|ui| {
                            ui.set_max_width(360.0);
                            ui.label(RichText::new(sig).monospace().strong());
                            ui.label(doc);
                        });
                    }
                }
            }
    }

    /// Park the current buffer so switching selection never loses edits.
    pub(crate) fn stash_script_buffer(&mut self) {
        if let Some(prev) = self.script_ed.comp.take() {
            let saved = self
                .project
                .circuit
                .components
                .get(&prev)
                .and_then(|c| c.script.as_deref());
            if !self.script_ed.buffer.is_empty() && saved != Some(self.script_ed.buffer.as_str()) {
                let buf = std::mem::take(&mut self.script_ed.buffer);
                self.script_ed.stash.insert(prev, buf);
            } else {
                self.script_ed.buffer.clear();
            }
        }
    }

    /// Compile immediately when no session tick will do it for us.
    pub(crate) fn sync_scripts_offline(&mut self) {
        if !self.live.connected() {
            self.live.scripts.sync(&self.project.circuit, &self.lib);
        }
    }

    pub(crate) fn show_console(&mut self, ui: &mut egui::Ui) {
        ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
            for line in &self.console {
                ui.label(RichText::new(line).monospace().size(12.0));
            }
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
        let recent: Vec<usize> = self
            .live
            .engine
            .firings
            .iter()
            .filter(|f| now.saturating_sub(f.at_ms) < 500)
            .map(|f| f.rule_idx)
            .collect();

        let mut remove: Option<usize> = None;
        {
            let ui = &mut *ui;
            let n_rules = self.project.program.rules.len();
            for idx in 0..n_rules {
                let fired = recent.contains(&idx);
                let frame = egui::Frame::group(ui.style()).fill(if fired {
                    Color32::from_rgb(40, 70, 45)
                } else {
                    Color32::from_gray(30)
                });
                frame.show(ui, |ui| {
                    let rule = &mut self.project.program.rules[idx];
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut rule.enabled, "");
                        ui.text_edit_singleline(&mut rule.name);
                        if ui.button("✖").on_hover_text("delete rule").clicked() {
                            remove = Some(idx);
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("when").strong());
                        Self::trigger_editor(ui, idx, &mut rule.trigger, &event_comps);
                    });
                    let mut remove_action: Option<usize> = None;
                    for (ai, action) in rule.actions.iter_mut().enumerate() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(if ai == 0 { "do" } else { "then" }).strong());
                            Self::action_editor(ui, idx, ai, action, &action_comps);
                            if ui.small_button("✖").clicked() {
                                remove_action = Some(ai);
                            }
                        });
                    }
                    if let Some(ai) = remove_action {
                        rule.actions.remove(ai);
                    }
                    if ui.small_button("+ action").clicked() {
                        rule.actions.push(default_action(&action_comps));
                    }
                });
                ui.add_space(4.0);
            }
            if let Some(idx) = remove {
                self.project.program.rules.remove(idx);
            }
            if ui.button("+ add rule").clicked() {
                let trigger = event_comps
                    .first()
                    .map(|(id, _, evs)| Trigger::CompEvent {
                        comp: *id,
                        event: evs.first().cloned().unwrap_or_else(|| "pressed".into()),
                    })
                    .unwrap_or(Trigger::OnStart);
                self.project.program.rules.push(Rule {
                    name: format!("rule {}", self.project.program.rules.len() + 1),
                    enabled: true,
                    trigger,
                    actions: vec![default_action(&action_comps)],
                });
            }
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
