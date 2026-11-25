//! The script editor tab: toolbar, find bar, a strip of the script's handlers with their last
//! activity, and a code view with line numbers, problem markers, squiggles and inline messages.

use std::collections::HashMap;

use egui::text::{CCursor, CCursorRange};
use egui::{
    Align2, Color32, FontId, Frame, Id, Panel, Rect, RichText, Sense, Shape, Stroke, Ui, pos2, vec2,
};
use egui_theme_kit::controls::{self, BarKind, MenuButton};
use wirelab_core::circuit::CompId;

use crate::app::WireLabApp;
use crate::dock::Tab;
use crate::rhai_lint::{API_DOCS, CALLBACK_DOCS, LintDiag, Severity};
use crate::script_docs::{Completion, FindKind, ScriptDoc};
use crate::ui_kit::{self, ph};

/// Handlers that fired this recently are highlighted, in ms.
const RECENT_MS: u64 = 700;

impl WireLabApp {
    pub fn show_script_tab(&mut self, ui: &mut Ui, id: CompId) {
        if !self.project.circuit.components.contains_key(&id) {
            controls::empty_state(
                ui,
                ui_kit::kit(ph::FILE_X),
                "Part removed",
                "This part is no longer in the circuit.",
                &[],
            );
            return;
        }
        let m = egui_theme_kit::metrics(ui.ctx());
        let fill = ui.visuals().panel_fill;
        Panel::top(("script-toolbar", id))
            .exact_size(m.bar_h)
            .frame(Frame::NONE.fill(fill))
            .show_separator_line(false)
            .show(ui, |ui| {
                controls::bar(ui, BarKind::Tool, |ui| self.script_toolbar(ui, id))
            });
        if self.docs.doc(id, &self.project.circuit).find_open {
            Panel::top(("script-find", id))
                .exact_size(m.bar_h)
                .frame(Frame::NONE.fill(fill))
                .show_separator_line(false)
                .show(ui, |ui| {
                    controls::bar(ui, BarKind::Tool, |ui| self.script_find_bar(ui, id))
                });
        }
        Panel::bottom(("script-status", id))
            .exact_size(m.header_h)
            .frame(Frame::NONE.fill(fill))
            .show_separator_line(false)
            .show(ui, |ui| {
                controls::bar(ui, BarKind::Status, |ui| self.script_status(ui, id))
            });

        egui::CentralPanel::default()
            .frame(Frame::NONE)
            .show(ui, |ui| {
                let empty = self.docs.doc(id, &self.project.circuit).buffer.is_empty()
                    && self
                        .project
                        .circuit
                        .components
                        .get(&id)
                        .is_some_and(|c| c.script.is_none());
                if empty {
                    let hint =
                        "Scripts react to this part's events and drive the rest of the circuit.";
                    if controls::empty_state(
                        ui,
                        ui_kit::kit(ph::FILE_CODE),
                        "No script yet",
                        hint,
                        &[(controls::Role::Primary, "Attach a starter script")],
                    )
                    .is_some()
                    {
                        self.attach_template_script(id);
                    }
                    return;
                }
                self.handler_strip(ui, id);
                self.script_editor(ui, id);
            });
    }

    fn script_toolbar(&mut self, ui: &mut Ui, id: CompId) {
        let dirty = self.docs.is_dirty(id, &self.project.circuit);
        if ui_kit::primary(ui, dirty, ph::PLAY, "Apply")
            .on_hover_text("Compile and hot-swap (Ctrl+Enter)")
            .clicked()
        {
            self.apply_script(id);
        }
        if ui_kit::tool_enabled(
            ui,
            dirty,
            ph::ARROW_COUNTER_CLOCKWISE,
            "Revert to the applied script",
        )
        .clicked()
        {
            self.docs.revert(id, &self.project.circuit);
        }
        controls::vline(ui, egui_theme_kit::metrics(ui.ctx()).control_h);
        let find_open = self.docs.doc(id, &self.project.circuit).find_open;
        if ui_kit::tool_toggle(
            ui,
            find_open,
            ph::MAGNIFYING_GLASS,
            "Find and replace (Ctrl+F)",
        )
        .clicked()
        {
            let doc = self.docs.doc(id, &self.project.circuit);
            doc.find_open = !doc.find_open;
        }
        let snippets = self
            .lib
            .board(&self.project.circuit.board_id)
            .map(crate::ide_snippets::snippets_for)
            .unwrap_or_default();
        let _ = MenuButton::new("Snippets")
            .icon(ui_kit::kit(ph::CODE_BLOCK))
            .tip("Insert a working template")
            .show(ui, |ui| {
                for sn in &snippets {
                    let row = ui_kit::menu_item(ui, ph::PLUS, sn.title).on_hover_ui(|ui| {
                        ui.label(sn.blurb);
                        ui.label(RichText::new(sn.code).monospace().small().weak());
                    });
                    if row.clicked() {
                        self.docs.doc(id, &self.project.circuit).pending_snippet = Some(sn.code);
                        ui.close();
                    }
                }
            });
        let _ = MenuButton::new("Help")
            .icon(ui_kit::kit(ph::QUESTION))
            .tip("What this script can do")
            .show(ui, |ui| {
                ui.set_max_width(420.0);
                self.script_cheat_sheet(ui, id);
            });
        if ui_kit::tool(ui, ph::BOOK_OPEN, "Rhai reference").clicked() {
            self.open_tab(Tab::Reference);
        }
        let hints = self.docs.hints_on;
        if ui_kit::tool_toggle(ui, hints, ph::TEXT_AA, "Inline type hints").clicked() {
            self.docs.hints_on = !hints;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (icon, text, tone) = self.script_state(ui, id);
            ui_kit::status(ui, icon, text, tone);
        });
    }

    /// Icon, label and tone of a script's state: modified, error, live or idle.
    pub fn script_state(
        &self,
        ui: &Ui,
        id: CompId,
    ) -> (egui_icon_pack::Icon, &'static str, Color32) {
        let has_script = self
            .project
            .circuit
            .components
            .get(&id)
            .is_some_and(|c| c.script.is_some());
        let weak = ui.visuals().weak_text_color();
        if self.docs.is_dirty(id, &self.project.circuit) {
            (ph::PENCIL_SIMPLE, "Modified", ui.visuals().warn_fg_color)
        } else if self.live.scripts.errors.contains_key(&id) {
            (ph::WARNING_CIRCLE, "Error", ui.visuals().error_fg_color)
        } else if has_script && self.live.connected() && self.live.scripts.has_script(id) {
            (ph::PULSE, "Live", egui_theme_kit::success(ui))
        } else if has_script {
            (ph::CHECK_CIRCLE, "Applied", weak)
        } else {
            (ph::CIRCLE_DASHED, "No script", weak)
        }
    }

    fn script_find_bar(&mut self, ui: &mut Ui, id: CompId) {
        let doc = self.docs.doc(id, &self.project.circuit);
        let hits = if doc.find.is_empty() {
            0
        } else {
            doc.buffer.matches(&doc.find).count()
        };
        controls::search_field(ui, &mut doc.find, "Find", 180.0);
        ui.label(RichText::new(format!("{hits} found")).weak());
        if ui_kit::tool(ui, ph::ARROW_DOWN, "Next match").clicked() {
            doc.pending_find = Some(FindKind::Next);
        }
        controls::text_field(ui, &mut doc.replace, "Replace", 180.0);
        if ui_kit::tool(ui, ph::SWAP, "Replace next").clicked() {
            doc.pending_find = Some(FindKind::Replace);
        }
        if ui_kit::tool(ui, ph::CHECKS, "Replace all").clicked() {
            doc.pending_find = Some(FindKind::All);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui_kit::tool(ui, ph::X, "Close").clicked() {
                doc.find_open = false;
            }
        });
    }

    fn script_status(&mut self, ui: &mut Ui, id: CompId) {
        let diags = self.docs.diags(id);
        let errors = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
        let warnings = diags.len() - errors;
        let (error_tone, warn_tone) = (ui.visuals().error_fg_color, ui.visuals().warn_fg_color);
        let weak = ui.visuals().weak_text_color();
        let problems = if errors + warnings == 0 {
            ui_kit::status(ui, ph::CHECK_CIRCLE, "No problems", weak)
        } else {
            let a = ui_kit::status(
                ui,
                ph::X_CIRCLE,
                &errors.to_string(),
                if errors > 0 { error_tone } else { weak },
            );
            a | ui_kit::status(
                ui,
                ph::WARNING,
                &warnings.to_string(),
                if warnings > 0 { warn_tone } else { weak },
            )
        };
        if problems.on_hover_text("Open the Problems panel").clicked() {
            self.open_tab(Tab::Problems);
        }
        let (line, col) = self.docs.get(id).map_or((1, 1), |d| d.cursor);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new("Rhai").weak());
            ui.label(RichText::new(format!("Ln {line}, Col {col}")).weak());
        });
    }

    /// The script's functions as chips: click to jump, highlighted right after they ran.
    fn handler_strip(&mut self, ui: &mut Ui, id: CompId) {
        let doc = self.docs.doc(id, &self.project.circuit);
        let mut fns: Vec<(usize, String)> = doc
            .buffer
            .lines()
            .enumerate()
            .filter_map(|(i, l)| {
                let rest = l.trim_start().strip_prefix("fn ")?;
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                (!name.is_empty()).then_some((i, name))
            })
            .collect();
        fns.sort_by_key(|(_, name)| !name.starts_with("on_"));
        if fns.is_empty() {
            return;
        }
        let now = self.live.now_ms();
        let fired = self.live.scripts.last_fired.get(&id).cloned();
        let mut jump = None;
        ui.horizontal_wrapped(|ui| {
            for (line, name) in &fns {
                let recent = fired
                    .as_ref()
                    .filter(|(f, at)| f == name && now.saturating_sub(*at) < RECENT_MS)
                    .is_some();
                let callback = CALLBACK_DOCS.iter().find(|(n, _)| n == name);
                let icon = if callback.is_some() {
                    ph::LIGHTNING
                } else {
                    ph::BRACKETS_CURLY
                };
                let mut chip = controls::pill(ui, Some(ui_kit::kit(icon)), name, recent);
                if let Some((f, at)) = fired.as_ref().filter(|(f, _)| f == name) {
                    let ago = now.saturating_sub(*at) as f32 / 1000.0;
                    chip = chip.on_hover_text(format!("{f} ran {ago:.1} s ago"));
                } else if let Some((_, doc)) = callback {
                    chip = chip.on_hover_text(*doc);
                }
                if chip.clicked() {
                    jump = Some(*line);
                }
            }
        });
        if jump.is_some() {
            self.docs.doc(id, &self.project.circuit).pending_jump = jump;
        }
        if fired.is_some_and(|(_, at)| now.saturating_sub(at) < RECENT_MS) {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        ui.add_space(egui_theme_kit::metrics(ui.ctx()).gap.y);
    }

    /// Compile the buffer into the component and hot-swap it.
    pub fn apply_script(&mut self, id: CompId) {
        let buffer = self.docs.doc(id, &self.project.circuit).buffer.clone();
        let name = self.comp_name(id);
        let Some(comp) = self.project.circuit.components.get_mut(&id) else {
            return;
        };
        comp.script = (!buffer.trim().is_empty()).then_some(buffer);
        self.script_rev += 1;
        self.sync_scripts_offline();
        let line = match self.live.scripts.errors.get(&id) {
            Some(err) => format!("script error in `{name}`: {err}"),
            None => format!("script for `{name}` applied"),
        };
        self.log(line);
    }

    fn script_editor(&mut self, ui: &mut Ui, id: CompId) {
        let names = wirelab_core::script::component_names(&self.project.circuit, &self.lib);
        let diags: Vec<LintDiag> = self.docs.diags(id).to_vec();
        let lint_key = self.docs.diag_key(id);
        let hints_on = self.docs.hints_on;
        let editor_id = Id::unique(("wirelab-script-editor", id));
        let mut apply = false;
        {
            let doc = self.docs.doc(id, &self.project.circuit);
            if let Some(code) = doc.pending_snippet.take() {
                if !doc.buffer.is_empty() {
                    while !doc.buffer.ends_with("\n\n") {
                        doc.buffer.push('\n');
                    }
                }
                let line = doc.buffer.lines().count();
                doc.buffer.push_str(code);
                doc.pending_jump = Some(line);
            }
            let focused = ui.ctx().memory(|m| m.has_focus(editor_id));
            ui.input_mut(|i| {
                if i.consume_key(egui::Modifiers::COMMAND, egui::Key::F) {
                    doc.find_open = true;
                }
                if focused
                    && (i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter)
                        || i.consume_key(egui::Modifiers::COMMAND, egui::Key::S))
                {
                    apply = true;
                }
            });
            egui::ScrollArea::vertical()
                .id_salt(("script-scroll", id))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    code_view(ui, doc, &diags, lint_key, hints_on, &names, editor_id)
                });
        }
        if apply && self.docs.is_dirty(id, &self.project.circuit) {
            self.apply_script(id);
        }
    }

    /// Callbacks for this part, its verbs, the other parts' verbs and the globals.
    fn script_cheat_sheet(&self, ui: &mut Ui, me: CompId) {
        use wirelab_core::component::SimModel;
        let kind_info = |sim: &SimModel| -> (&'static str, &'static str) {
            match sim {
                SimModel::PushButton => (
                    "on_press() · on_release() · on_change(on)",
                    "me.is_pressed()",
                ),
                SimModel::ToggleSwitch | SimModel::SlideSwitchSpdt | SimModel::DigitalSensor => {
                    ("on_change(on)", "me.is_on()")
                }
                SimModel::Potentiometer { .. }
                | SimModel::Photoresistor { .. }
                | SimModel::AnalogSensor { .. } => ("on_reading(mv)", "me.millivolts()"),
                SimModel::Led { .. } => (
                    "on_start() · on_tick(dt)",
                    "me.on() .off() .toggle() .blink(ms) .breathe(ms) .dim(pct)",
                ),
                SimModel::Buzzer { .. } => (
                    "on_start() · on_tick(dt)",
                    "me.beep(ms) .tone(hz, ms) .on() .off()",
                ),
                SimModel::Servo => ("on_start() · on_tick(dt)", "me.set_angle(deg)"),
                SimModel::RelayModule => ("on_start() · on_tick(dt)", "me.on() .off() .toggle()"),
                SimModel::Resistor { .. } | SimModel::Generic => (
                    "on_start() · on_tick(dt) · on_pin(gpio, high)",
                    "rgb(r,g,b), pin(n)…",
                ),
                SimModel::DcSupply { .. }
                | SimModel::HvBoost { .. }
                | SimModel::Capacitor { .. }
                | SimModel::Diode { .. }
                | SimModel::ArcContact { .. }
                | SimModel::Conductor { .. } => (
                    "on_start() · on_tick(dt)",
                    "power-stage part: read it through a divider on an ADC pin",
                ),
            }
        };
        let names = wirelab_core::script::component_names(&self.project.circuit, &self.lib);
        let def_of = |id: &CompId| {
            self.project
                .circuit
                .components
                .get(id)
                .and_then(|c| self.lib.component(&c.def_id))
        };
        let own = names.get(&me).cloned().unwrap_or_default();
        controls::subheading(
            ui,
            Some(ui_kit::kit(ph::FILE_CODE)),
            &format!("You are `{own}`"),
        );
        ui.label(RichText::new("State lives on `this`; your own handle is `me`.").weak());
        if let Some(def) = def_of(&me) {
            let (callbacks, verbs) = kind_info(&def.sim);
            ui.label(format!(
                "Callbacks: {callbacks} · always: on_start, on_tick, on_pin"
            ));
            ui.label(format!("Yourself: {verbs}"));
        }
        ui.separator();
        let mut others: Vec<(&CompId, &String)> = names.iter().filter(|(c, _)| **c != me).collect();
        others.sort_by_key(|(_, n)| n.as_str());
        for (cid, n) in others {
            if let Some(def) = def_of(cid) {
                let (_, verbs) = kind_info(&def.sim);
                ui.label(
                    RichText::new(format!("{n}: {}", verbs.replace("me.", &format!("{n}."))))
                        .monospace()
                        .small(),
                );
            }
        }
        ui.separator();
        ui.label(RichText::new("Globals: log(x) · millis() · after(ms, || …) · rgb(r, g, b) · pin(n) · board_has(\"wifi\")").weak());
    }
}

fn severity_color(ui: &Ui, severity: Severity) -> Color32 {
    match severity {
        Severity::Error => ui.visuals().error_fg_color,
        Severity::Warning => ui.visuals().warn_fg_color,
    }
}

/// Byte offset to char index.
fn char_index(text: &str, byte: usize) -> usize {
    let mut b = byte.min(text.len());
    while b > 0 && !text.is_char_boundary(b) {
        b -= 1;
    }
    text[..b].chars().count()
}

/// A zigzag underline from `x0` to `x1` at `y`.
fn squiggle(painter: &egui::Painter, x0: f32, x1: f32, y: f32, color: Color32) {
    let (step, amp) = (2.5, 1.5);
    let mut points = Vec::new();
    let mut x = x0;
    let mut up = false;
    while x < x1.max(x0 + step) {
        points.push(pos2(x, if up { y - amp } else { y + amp }));
        up = !up;
        x += step;
    }
    points.push(pos2(x1.max(x0 + step), if up { y - amp } else { y + amp }));
    painter.add(Shape::line(points, Stroke::new(1.1, color)));
}

#[allow(clippy::too_many_arguments)]
fn code_view(
    ui: &mut Ui,
    doc: &mut ScriptDoc,
    diags: &[LintDiag],
    lint_key: u64,
    hints_on: bool,
    names: &HashMap<CompId, String>,
    editor_id: Id,
) {
    // A popup built before this frame's typing is stale; keys go to the text.
    if ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(_)))) {
        doc.completion = None;
    }
    if doc.completion.is_none() && ui.ctx().memory(|m| m.has_focus(editor_id)) {
        indent_enters(ui, &doc.buffer, editor_id);
    }
    let mut accept: Option<usize> = None;
    if doc.completion.is_some() {
        use egui::{Key, Modifiers};
        let (mut up, mut down, mut esc, mut tab, mut enter) = (false, false, false, false, false);
        ui.input_mut(|i| {
            up = i.consume_key(Modifiers::NONE, Key::ArrowUp);
            down = i.consume_key(Modifiers::NONE, Key::ArrowDown);
            esc = i.consume_key(Modifiers::NONE, Key::Escape);
            tab = i.consume_key(Modifiers::NONE, Key::Tab);
            enter = i.consume_key(Modifiers::NONE, Key::Enter);
        });
        if esc {
            doc.completion = None;
        } else if let Some(c) = &mut doc.completion {
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

    let matches: Vec<(usize, usize)> = if doc.find_open && !doc.find.is_empty() {
        doc.buffer
            .match_indices(&doc.find)
            .map(|(i, m)| (i, i + m.len()))
            .collect()
    } else {
        Vec::new()
    };
    let match_bg = ui.visuals().selection.bg_fill;
    let theme = egui_extras::syntax_highlighting::CodeTheme::from_memory(ui.ctx(), ui.style());
    let mut layouter = |ui: &Ui, buf: &dyn egui::TextBuffer, wrap_width: f32| {
        let mut job = egui_extras::syntax_highlighting::highlight(
            ui.ctx(),
            ui.style(),
            &theme,
            buf.as_str(),
            "rs",
        );
        if !matches.is_empty() {
            for section in &mut job.sections {
                let r = &section.byte_range;
                if matches.iter().any(|(s, e)| r.start.0 < *e && *s < r.end.0) {
                    section.format.background = match_bg;
                }
            }
        }
        job.wrap.max_width = wrap_width;
        ui.fonts_mut(|f| f.layout_job(job))
    };

    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let row_h = ui.text_style_height(&egui::TextStyle::Monospace).max(1.0);
    let line_count = doc.buffer.lines().count().max(1);
    let digit_w = ui.fonts_mut(|f| f.glyph_width(&mono, '0'));
    let gutter = (line_count.to_string().len().max(2) as f32 * digit_w + 26.0)
        .ceil()
        .min(120.0);
    let rows = ((ui.available_height() / row_h).ceil() as usize).max(8);
    let out = egui::TextEdit::multiline(&mut doc.buffer)
        .id(editor_id)
        .code_editor()
        .desired_width(f32::INFINITY)
        .desired_rows(rows)
        .margin(egui::Margin {
            left: gutter as i8,
            right: 6,
            top: 4,
            bottom: 4,
        })
        .layouter(&mut layouter)
        .show(ui);

    if let Some(cr) = out.cursor_range {
        let ci = cr.primary.index.0;
        let before: String = doc.buffer.chars().take(ci).collect();
        let line = before.matches('\n').count() + 1;
        let col = before.chars().rev().take_while(|c| *c != '\n').count() + 1;
        doc.cursor = (line, col);
    }

    // Row geometry per source line: (top-left of its first row, end of its last row).
    let mut line_rows: Vec<(Rect, f32)> = Vec::new();
    let mut first: Option<Rect> = None;
    for (ri, placed) in out.galley.rows.iter().enumerate() {
        let rect = Rect::from_min_size(
            out.galley_pos + placed.pos.to_vec2(),
            vec2(placed.row.size.x, placed.row.size.y.max(row_h)),
        );
        let start = *first.get_or_insert(rect);
        if placed.ends_with_newline || ri + 1 == out.galley.rows.len() {
            line_rows.push((start, rect.right()));
            first = None;
        }
    }

    let painter = ui.painter().with_clip_rect(out.response.response.rect);
    let frame = out.response.response.rect;
    let gutter_rect =
        Rect::from_min_max(frame.min, pos2(frame.left() + gutter - 6.0, frame.bottom()));
    painter.rect_filled(gutter_rect, 0.0, ui.visuals().faint_bg_color);

    let mut worst: HashMap<usize, (Severity, &LintDiag)> = HashMap::new();
    for d in diags {
        let entry = worst.entry(d.line).or_insert((d.severity, d));
        if d.severity < entry.0 {
            *entry = (d.severity, d);
        }
    }
    let number_font = FontId::monospace(mono.size * 0.85);
    let weak = ui.visuals().weak_text_color();
    let strong = ui.visuals().strong_text_color();
    for (i, (rect, _)) in line_rows.iter().enumerate() {
        let line = i + 1;
        let y = rect.center().y;
        let color = if line == doc.cursor.0 {
            strong
        } else {
            weak.gamma_multiply(0.8)
        };
        painter.text(
            pos2(gutter_rect.right() - 6.0, y),
            Align2::RIGHT_CENTER,
            line.to_string(),
            number_font.clone(),
            color,
        );
        if let Some((severity, _)) = worst.get(&line) {
            painter.circle_filled(
                pos2(gutter_rect.left() + 8.0, y),
                3.5,
                severity_color(ui, *severity),
            );
        }
    }

    // Squiggles under each problem, and its message after the line's end.
    let text_painter = ui.painter().with_clip_rect(out.text_clip_rect);
    let lens_font = FontId::monospace(mono.size * 0.9);
    for d in diags {
        let color = severity_color(ui, d.severity);
        let a = out
            .galley
            .pos_from_cursor(CCursor::new(char_index(&doc.buffer, d.start)));
        let b = out
            .galley
            .pos_from_cursor(CCursor::new(char_index(&doc.buffer, d.end)));
        let (a, b) = (
            a.translate(out.galley_pos.to_vec2()),
            b.translate(out.galley_pos.to_vec2()),
        );
        let row_end = line_rows
            .get(d.line.saturating_sub(1))
            .map_or(a.right() + 8.0, |(_, end)| *end);
        let x1 = if (b.top() - a.top()).abs() < 1.0 {
            b.left()
        } else {
            row_end
        };
        squiggle(
            &text_painter,
            a.left(),
            x1.max(a.left() + 6.0),
            a.bottom() - 1.0,
            color,
        );
    }
    for (line, (severity, d)) in &worst {
        if let Some((rect, end)) = line_rows.get(line - 1) {
            let text = d.message.lines().next().unwrap_or_default();
            let color = severity_color(ui, *severity).gamma_multiply(0.85);
            text_painter.rect_filled(
                Rect::from_min_max(
                    pos2(rect.left(), rect.top()),
                    pos2(out.text_clip_rect.right(), rect.bottom()),
                ),
                0.0,
                color.gamma_multiply(0.06),
            );
            text_painter.text(
                pos2(end + 24.0, rect.center().y),
                Align2::LEFT_CENTER,
                text,
                lens_font.clone(),
                color,
            );
        }
    }

    // Inline type hints past each line's end, on lines without a problem.
    if hints_on {
        let hint_font = FontId::monospace(mono.size * 0.75);
        let color = ui.visuals().weak_text_color().gamma_multiply(0.75);
        for (i, ((rect, end), text)) in line_rows.iter().zip(doc.buffer.lines()).enumerate() {
            if worst.contains_key(&(i + 1)) {
                continue;
            }
            if let Some(hint) = line_hint(text) {
                text_painter.text(
                    pos2(end + 14.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    hint,
                    hint_font.clone(),
                    color,
                );
            }
        }
    }

    // Gutter tooltips for problem markers.
    let gutter_response = ui.interact(gutter_rect, editor_id.with("gutter"), Sense::hover());
    if let Some(p) = gutter_response.hover_pos()
        && let Some(i) = line_rows
            .iter()
            .position(|(r, _)| r.top() <= p.y && p.y < r.top() + row_h)
    {
        let here: Vec<&LintDiag> = diags.iter().filter(|d| d.line == i + 1).collect();
        if !here.is_empty() {
            gutter_response.on_hover_ui_at_pointer(|ui| {
                ui.set_max_width(420.0);
                for d in here {
                    ui.colored_label(severity_color(ui, d.severity), &d.message);
                }
            });
        }
    }

    // Requests from the toolbar, find bar and handler strip.
    let pending_jump = doc.pending_jump.take();
    let pending_find = doc.pending_find.take();
    if pending_jump.is_some() || pending_find.is_some() {
        let caret_to = |ui: &Ui, state: &egui::text_edit::TextEditState, ci: usize| {
            let mut state = state.clone();
            state
                .cursor
                .set_char_range(Some(CCursorRange::one(CCursor::new(ci))));
            state.store(ui.ctx(), editor_id);
            ui.ctx().memory_mut(|m| m.request_focus(editor_id));
        };
        if let Some(line) = pending_jump {
            let ci = doc
                .buffer
                .split('\n')
                .take(line)
                .map(|l| l.chars().count() + 1)
                .sum::<usize>();
            caret_to(ui, &out.state, ci);
            if let Some((rect, _)) = line_rows.get(line) {
                ui.scroll_to_rect(rect.expand(row_h * 3.0), None);
            }
        } else if let Some(kind) = pending_find
            && !doc.find.is_empty()
        {
            let find = doc.find.clone();
            let cur = out
                .cursor_range
                .map_or(0, |c| c.primary.index.0)
                .min(doc.buffer.chars().count());
            let cur_byte = doc
                .buffer
                .char_indices()
                .nth(cur)
                .map_or(doc.buffer.len(), |(b, _)| b);
            let hit = doc.buffer[cur_byte..]
                .find(&find)
                .map(|o| cur_byte + o)
                .or_else(|| doc.buffer.find(&find));
            match (kind, hit) {
                (FindKind::Next, Some(b)) => {
                    let ci = doc.buffer[..b].chars().count() + find.chars().count();
                    caret_to(ui, &out.state, ci);
                }
                (FindKind::Replace, Some(b)) => {
                    let repl = doc.replace.clone();
                    doc.buffer.replace_range(b..b + find.len(), &repl);
                    let ci = doc.buffer[..b].chars().count() + repl.chars().count();
                    caret_to(ui, &out.state, ci);
                }
                (FindKind::All, _) => doc.buffer = doc.buffer.replace(&find, &doc.replace),
                _ => {}
            }
        }
        ui.ctx().request_repaint();
        return;
    }

    completion_and_hover(ui, doc, diags, lint_key, names, editor_id, accept, out);
}

/// Turns each plain Enter of this frame into a newline carrying the indentation of the line it
/// ends, plus one level after `{`, and dedents a `}` typed on a blank indented line; in event
/// order, so text typed in the same frame lands right.
fn indent_enters(ui: &Ui, buffer: &str, editor_id: Id) {
    let Some(state) = egui::TextEdit::load_state(ui.ctx(), editor_id) else {
        return;
    };
    let Some(range) = state.cursor.char_range() else {
        return;
    };
    let caret = range.primary.index.0.min(range.secondary.index.0);
    let before: String = buffer.chars().take(caret).collect();
    let mut line = before.rsplit('\n').next().unwrap_or_default().to_string();
    ui.input_mut(|i| {
        let mut events = Vec::with_capacity(i.events.len());
        for event in i.events.drain(..) {
            match event {
                egui::Event::Text(t) => {
                    let blank = !line.is_empty() && line.chars().all(|c| c == ' ');
                    if t.starts_with('}') && blank {
                        for _ in 0..line.len().min(4) {
                            events.push(egui::Event::Key {
                                key: egui::Key::Backspace,
                                physical_key: None,
                                pressed: true,
                                repeat: false,
                                modifiers: egui::Modifiers::NONE,
                            });
                            line.pop();
                        }
                    }
                    line.push_str(&t);
                    events.push(egui::Event::Text(t));
                }
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    modifiers,
                    ..
                } if modifiers.is_none() => {
                    let indent: String = line
                        .chars()
                        .take_while(|c| *c == ' ' || *c == '\t')
                        .collect();
                    let extra = if line.trim_end().ends_with('{') {
                        "    "
                    } else {
                        ""
                    };
                    line = format!("{indent}{extra}");
                    // A bare newline stays an Enter key; the editor drops a lone "\n" text event.
                    if line.is_empty() {
                        events.push(event);
                    } else {
                        events.push(egui::Event::Text(format!("\n{line}")));
                    }
                }
                other => events.push(other),
            }
        }
        i.events = events;
    });
}

/// Completion popup and hover docs, LSP-style.
#[allow(clippy::too_many_arguments)]
fn completion_and_hover(
    ui: &mut Ui,
    doc: &mut ScriptDoc,
    diags: &[LintDiag],
    lint_key: u64,
    names: &HashMap<CompId, String>,
    editor_id: Id,
    accept: Option<usize>,
    out: egui::text_edit::TextEditOutput,
) {
    let chars: Vec<char> = doc.buffer.chars().collect();
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let char_to_byte =
        |buf: &str, ci: usize| buf.char_indices().nth(ci).map_or(buf.len(), |(b, _)| b);
    let mut apply = accept;

    let focused = out.response.response.has_focus();
    let cursor_ci = out
        .cursor_range
        .map(|cr| cr.primary.index.0.min(chars.len()));
    let key = (cursor_ci.unwrap_or(usize::MAX), lint_key);
    if apply.is_none() && (key != doc.completion_key || !focused) {
        doc.completion_key = key;
        doc.completion = None;
        if focused && let Some(ci) = cursor_ci {
            let comp_names: Vec<String> = names.values().cloned().collect();
            if let Some(comp) = wirelab_core::script_api::completions(&doc.buffer, ci, &comp_names)
            {
                let items = comp
                    .items
                    .into_iter()
                    .map(|i| (i.label, i.insert, i.back, i.detail))
                    .collect();
                doc.completion = Some(Completion {
                    items,
                    selected: 0,
                    word_start: comp.word_start,
                });
            }
        }
    }

    if apply.is_none()
        && let Some(c) = &doc.completion
    {
        let anchor = out.galley.pos_from_cursor(CCursor::new(c.word_start));
        let pos = out.galley_pos + anchor.left_bottom().to_vec2() + vec2(0.0, 2.0);
        let mut clicked = None;
        egui::Area::new(editor_id.with("completion"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                Frame::popup(ui.style()).show(ui, |ui| {
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
                        RichText::new("Tab/Enter accept · Up/Down choose · Esc close")
                            .small()
                            .weak(),
                    );
                });
            });
        apply = clicked;
    }

    if let Some(idx) = apply
        && let Some(c) = doc.completion.take()
        && let Some((_, insert, back, _)) = c.items.get(idx)
    {
        let buf = doc.buffer.clone();
        let mut we = c.word_start;
        while we < chars.len() && is_ident(chars[we]) {
            we += 1;
        }
        let (sb, eb) = (char_to_byte(&buf, c.word_start), char_to_byte(&buf, we));
        doc.buffer.replace_range(sb..eb, insert);
        let caret = c.word_start + insert.chars().count() - back;
        let mut state = out.state.clone();
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(caret))));
        state.store(ui.ctx(), editor_id);
        ui.ctx().request_repaint();
        return;
    }

    if doc.completion.is_none()
        && out.response.response.hovered()
        && let Some(p) = ui.ctx().pointer_hover_pos()
    {
        let ci = out
            .galley
            .cursor_from_pos(p - out.galley_pos)
            .index
            .0
            .min(chars.len());
        let (mut ws, mut we) = (ci, ci);
        while ws > 0 && is_ident(chars[ws - 1]) {
            ws -= 1;
        }
        while we < chars.len() && is_ident(chars[we]) {
            we += 1;
        }
        let byte = char_to_byte(&doc.buffer, ci);
        let problem = diags.iter().find(|d| d.start <= byte && byte < d.end);
        let word: String = chars[ws..we].iter().collect();
        let member = ws > 0 && chars[ws - 1] == '.';
        let hover: Option<(String, String, Option<Color32>)> = if let Some(d) = problem {
            Some((
                if d.severity == Severity::Error {
                    "Error"
                } else {
                    "Warning"
                }
                .into(),
                d.message.clone(),
                Some(severity_color(ui, d.severity)),
            ))
        } else if ws == we {
            None
        } else if let Some(d) = API_DOCS
            .iter()
            .find(|d| d.name == word && d.member == member)
        {
            Some((d.sig.to_string(), d.doc.to_string(), None))
        } else if let Some((n, doc)) = CALLBACK_DOCS.iter().find(|(n, _)| *n == word) {
            Some((format!("fn {n}(...)"), doc.to_string(), None))
        } else if names.values().any(|n| n == &word) {
            Some((word.clone(), "component in this circuit".into(), None))
        } else {
            None
        };
        if let Some((sig, text, tone)) = hover {
            out.response.response.clone().on_hover_ui_at_pointer(|ui| {
                ui.set_max_width(360.0);
                let head = RichText::new(sig).monospace().strong();
                ui.label(match tone {
                    Some(c) => head.color(c),
                    None => head,
                });
                ui.label(text);
            });
        }
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
    let rest = t
        .strip_prefix("let ")
        .or_else(|| t.strip_prefix("const "))?;
    let (_, expr) = rest.split_once('=')?;
    infer_type(expr).map(|ty| format!(": {ty}"))
}

/// Cheap, literal-and-known-call type inference; None when unsure.
fn infer_type(expr: &str) -> Option<&'static str> {
    let e = expr.trim().trim_end_matches(';').trim();
    if e.is_empty() {
        return None;
    }
    const CALLS: &[(&str, &str)] = &[
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
        assert_eq!(
            line_hint("let ok = !btn.is_on();").as_deref(),
            Some(": bool")
        );
        assert_eq!(line_hint("let p = pin(4);").as_deref(), Some(": Pin"));
        assert_eq!(
            line_hint("let mv = pot.millivolts();").as_deref(),
            Some(": int (mV)")
        );
        assert_eq!(line_hint("let a = [1, 2];").as_deref(), Some(": array"));
        assert_eq!(line_hint("let m = #{ a: 1 };").as_deref(), Some(": map"));
        assert_eq!(
            line_hint("let t = millis() + 500;").as_deref(),
            Some(": int (ms)")
        );
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

    #[test]
    fn squiggle_positions_use_char_indices() {
        assert_eq!(char_index("aé b", 4), 3);
        assert_eq!(char_index("abc", 99), 3);
    }
}
