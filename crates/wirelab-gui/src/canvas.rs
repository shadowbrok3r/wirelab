//! Interactive circuit canvas: pan/zoom, placement, wiring, live pokes.

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};
use wirelab_core::circuit::{CompId, Endpoint, PlacedComponent, WireId};
use wirelab_core::component::{CompState, SimModel};
use wirelab_core::netlist::{WireVerdict, wire_verdict};
use wirelab_proto::PinMode;

use crate::app::{Selection, WireLabApp};
use crate::draw::{
    self, BoardControl, PIN_HIT_RADIUS_PX, PinLive, View, board_feature_rects,
    board_pin_world_pos, terminal_world_pos,
};

pub struct CanvasState {
    pub offset: Vec2,
    pub zoom: f32,
    pub placing: Option<String>,
    pub wire_from: Option<Endpoint>,
    pub dragging: Option<(CompId, Vec2)>,
    pub pressed_button: Option<CompId>,
    /// Screen-space anchor of an in-progress rubber-band selection.
    pub select_start: Option<Pos2>,
    /// Hit and world position captured when the context menu opened.
    ctx_hit: Option<Hit>,
    ctx_world: [f32; 2],
}

impl Default for CanvasState {
    fn default() -> Self {
        CanvasState {
            offset: Vec2::new(60.0, 40.0),
            zoom: 1.4,
            placing: None,
            wire_from: None,
            dragging: None,
            pressed_button: None,
            select_start: None,
            ctx_hit: None,
            ctx_world: [0.0, 0.0],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Hit {
    Pin(String),
    Terminal(CompId, usize),
    Comp(CompId),
    Wire(WireId),
    Feature(BoardControl),
}

const WIRE_PALETTE: [[u8; 3]; 6] = [
    [86, 156, 255],
    [255, 150, 60],
    [190, 120, 255],
    [90, 210, 200],
    [250, 220, 80],
    [240, 110, 170],
];

impl WireLabApp {
    fn endpoint_world(&self, ep: &Endpoint) -> Option<[f32; 2]> {
        match ep {
            Endpoint::BoardPin { key } => {
                let board = self.lib.board(&self.project.circuit.board_id)?;
                let pin = board.pin(key)?;
                Some(board_pin_world_pos(board, pin, self.project.circuit.board_pos))
            }
            Endpoint::Terminal { comp, terminal } => {
                let c = self.project.circuit.components.get(comp)?;
                let def = self.lib.component(&c.def_id)?;
                let idx = def.terminals.iter().position(|t| &t.id == terminal)?;
                Some(terminal_world_pos(c, def, idx))
            }
        }
    }

    /// Which way a wire should leave this endpoint: away from the board
    /// edge for pins, away from the body for component terminals.
    fn endpoint_exit_dir(&self, ep: &Endpoint) -> Vec2 {
        match ep {
            Endpoint::BoardPin { key } => {
                let Some(pin) = self
                    .lib
                    .board(&self.project.circuit.board_id)
                    .and_then(|b| b.pin(key))
                else {
                    return Vec2::ZERO;
                };
                match pin.side {
                    wirelab_core::board::Side::Left => Vec2::new(-1.0, 0.0),
                    wirelab_core::board::Side::Right => Vec2::new(1.0, 0.0),
                    wirelab_core::board::Side::Top => Vec2::new(0.0, -1.0),
                    wirelab_core::board::Side::Bottom => Vec2::new(0.0, 1.0),
                }
            }
            Endpoint::Terminal { comp, .. } => {
                let Some(c) = self.project.circuit.components.get(comp) else {
                    return Vec2::ZERO;
                };
                let Some(t) = self.endpoint_world(ep) else { return Vec2::ZERO };
                Vec2::new(t[0] - c.pos[0], t[1] - c.pos[1])
            }
        }
    }

    /// Pixels a doubling-back wire must travel to clear this endpoint's body.
    fn endpoint_clearance(&self, view: &View, ep: &Endpoint) -> f32 {
        match ep {
            Endpoint::BoardPin { .. } => 0.0,
            Endpoint::Terminal { comp, .. } => self
                .project
                .circuit
                .components
                .get(comp)
                .and_then(|c| self.lib.component(&c.def_id))
                .map(|d| view.px(d.visual.width_mm.max(d.visual.height_mm) / 2.0 + 2.5))
                .unwrap_or(0.0),
        }
    }

    /// Screen-space polyline for a wire, shared by drawing and hit-testing.
    fn wire_screen_path(
        &self,
        view: &View,
        wire: &wirelab_core::circuit::Wire,
    ) -> Option<Vec<Pos2>> {
        let a = self.endpoint_world(&wire.a)?;
        let b = self.endpoint_world(&wire.b)?;
        let route = draw::Route {
            exit_a: self.endpoint_exit_dir(&wire.a),
            exit_b: self.endpoint_exit_dir(&wire.b),
            lane: (wire.id.0 % 5) as i32 - 2,
            stub: view.px(3.0).clamp(8.0, 20.0),
            clear_a: self.endpoint_clearance(view, &wire.a),
            clear_b: self.endpoint_clearance(view, &wire.b),
        };
        Some(draw::wire_path(view.to_screen(a), view.to_screen(b), route))
    }

    fn hit_test(&self, view: &View, pointer: Pos2) -> Option<Hit> {
        // Terminals first: small targets above bodies.
        for comp in self.project.circuit.components.values() {
            let Some(def) = self.lib.component(&comp.def_id) else { continue };
            for i in 0..def.terminals.len() {
                let p = view.to_screen(terminal_world_pos(comp, def, i));
                if p.distance(pointer) < PIN_HIT_RADIUS_PX {
                    return Some(Hit::Terminal(comp.id, i));
                }
            }
        }
        if let Some(board) = self.lib.board(&self.project.circuit.board_id) {
            for pin in &board.pins {
                let p = view.to_screen(board_pin_world_pos(
                    board,
                    pin,
                    self.project.circuit.board_pos,
                ));
                if p.distance(pointer) < PIN_HIT_RADIUS_PX {
                    return Some(Hit::Pin(pin.key.clone()));
                }
            }
            for (kind, world_rect) in board_feature_rects(board, self.project.circuit.board_pos)
            {
                let r = Rect::from_min_max(
                    view.to_screen([world_rect.min.x, world_rect.min.y]),
                    view.to_screen([world_rect.max.x, world_rect.max.y]),
                );
                if r.expand(2.0).contains(pointer) {
                    return Some(Hit::Feature(kind));
                }
            }
        }
        for comp in self.project.circuit.components.values() {
            let Some(def) = self.lib.component(&comp.def_id) else { continue };
            let center = view.to_screen(comp.pos);
            let half = Vec2::new(
                view.px(def.visual.width_mm) / 2.0 + 4.0,
                view.px(def.visual.height_mm) / 2.0 + 4.0,
            );
            if Rect::from_center_size(center, half * 2.0).contains(pointer) {
                return Some(Hit::Comp(comp.id));
            }
        }
        for wire in self.project.circuit.wires.values() {
            let Some(pts) = self.wire_screen_path(view, wire) else { continue };
            if draw::wire_hit_distance(&pts, pointer) < 6.0 {
                return Some(Hit::Wire(wire.id));
            }
        }
        None
    }

    fn wire_color_for(&self, a: &Endpoint, b: &Endpoint) -> [u8; 3] {
        let rail = |ep: &Endpoint| -> Option<[u8; 3]> {
            let Endpoint::BoardPin { key } = ep else { return None };
            let board = self.lib.board(&self.project.circuit.board_id)?;
            match board.pin(key)?.kind {
                wirelab_core::board::PinKind::Gnd => Some([40, 40, 40]),
                wirelab_core::board::PinKind::V3_3 => Some([220, 60, 50]),
                wirelab_core::board::PinKind::V5 => Some([230, 60, 160]),
                _ => None,
            }
        };
        if let Some(c) = rail(a).or_else(|| rail(b)) {
            return c;
        }
        // Avoid a color already used by a wire touching either component,
        // so neighbours stay tellable-apart.
        let comp_of = |ep: &Endpoint| match ep {
            Endpoint::Terminal { comp, .. } => Some(*comp),
            _ => None,
        };
        let neighbours: Vec<[u8; 3]> = self
            .project
            .circuit
            .wires
            .values()
            .filter(|w| {
                [comp_of(&w.a), comp_of(&w.b)]
                    .into_iter()
                    .flatten()
                    .any(|c| Some(c) == comp_of(a) || Some(c) == comp_of(b))
            })
            .map(|w| w.color)
            .collect();
        let start = self.project.circuit.wires.len();
        for i in 0..WIRE_PALETTE.len() {
            let c = WIRE_PALETTE[(start + i) % WIRE_PALETTE.len()];
            if !neighbours.contains(&c) {
                return c;
            }
        }
        WIRE_PALETTE[start % WIRE_PALETTE.len()]
    }

    /// While placing: the wire under the cursor this component would splice
    /// into, with the terminal order that avoids crossing the leads.
    fn splice_target(
        &self,
        view: &View,
        pointer: Pos2,
        def: &wirelab_core::component::ComponentDef,
        world: [f32; 2],
    ) -> Option<(WireId, String, String)> {
        if def.terminals.is_empty() || def.terminals.len() > 2 {
            return None;
        }
        let mut best: Option<(WireId, f32)> = None;
        for wire in self.project.circuit.wires.values() {
            let Some(pts) = self.wire_screen_path(view, wire) else { continue };
            let d = draw::wire_hit_distance(&pts, pointer);
            if d < 9.0 && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((wire.id, d));
            }
        }
        let (wid, _) = best?;
        let w = self.project.circuit.wires.get(&wid)?;
        let ghost = PlacedComponent {
            id: CompId(0),
            def_id: def.id.clone(),
            pos: [world[0].round(), world[1].round()],
            rotation: 0,
            label: String::new(),
            props: Default::default(),
            state: CompState::None,
            script: None,
        };
        let (ia, ib) = if def.terminals.len() == 1 {
            (0, 0)
        } else {
            let a = self.endpoint_world(&w.a)?;
            let b = self.endpoint_world(&w.b)?;
            let t0 = terminal_world_pos(&ghost, def, 0);
            let t1 = terminal_world_pos(&ghost, def, 1);
            let d2 = |p: [f32; 2], q: [f32; 2]| (p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2);
            if d2(a, t0) + d2(b, t1) <= d2(a, t1) + d2(b, t0) { (0, 1) } else { (1, 0) }
        };
        Some((wid, def.terminals[ia].id.clone(), def.terminals[ib].id.clone()))
    }

    fn comp_is_dot(&self, id: CompId) -> bool {
        self.project
            .circuit
            .components
            .get(&id)
            .and_then(|c| self.lib.component(&c.def_id))
            .is_some_and(|d| d.visual.shape == wirelab_core::component::Shape::Dot)
    }

    /// Judge a candidate wire against the current netlist.
    fn draft_verdict(&self, from: &Endpoint, to: &Endpoint) -> WireVerdict {
        let Some(board) = self.lib.board(&self.project.circuit.board_id) else {
            return WireVerdict::Ok;
        };
        let outs: Vec<u8> = self.cache.bindings.outputs.values().map(|b| b.gpio).collect();
        wire_verdict(&self.cache.netlist, board, from, to, &outs)
    }

    /// Endpoint a hit refers to, if it is a wireable point.
    fn hit_endpoint(&self, hit: &Hit) -> Option<Endpoint> {
        match hit {
            Hit::Pin(k) => Some(Endpoint::BoardPin { key: k.clone() }),
            Hit::Terminal(c, i) => self.terminal_endpoint(*c, *i),
            _ => None,
        }
    }

    /// Poke a component in live mode; returns true when handled.
    fn live_interact_click(&mut self, id: CompId) -> bool {
        if !self.live.connected() {
            return false;
        }
        let Some(comp) = self.project.circuit.components.get_mut(&id) else { return false };
        let Some(def) = self.lib.component(&comp.def_id) else { return false };
        match def.sim {
            SimModel::ToggleSwitch | SimModel::SlideSwitchSpdt | SimModel::DigitalSensor => {
                if let CompState::Toggle { on } = comp.state {
                    comp.state = CompState::Toggle { on: !on };
                    self.state_rev += 1;
                }
                true
            }
            _ => false,
        }
    }

    pub fn show_canvas(&mut self, ui: &mut egui::Ui) {
        let accent = ui.visuals().selection.stroke.color;
        let canvas_bg = ui.visuals().extreme_bg_color;
        let grid_dot = ui.visuals().faint_bg_color.linear_multiply(2.0);
        let hint_color = ui.visuals().weak_text_color();
        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let rect = response.rect;
        let view = View {
            origin: rect.min + self.canvas.offset,
            scale: self.canvas.zoom * 4.0,
        };
        let pointer = response.hover_pos();

        // Zoom around the cursor.
        if let Some(p) = pointer {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                let factor = (scroll * 0.0015).exp();
                let new_zoom = (self.canvas.zoom * factor).clamp(0.25, 8.0);
                let factor = new_zoom / self.canvas.zoom;
                let rel = p - rect.min - self.canvas.offset;
                self.canvas.offset += rel - rel * factor;
                self.canvas.zoom = new_zoom;
            }
        }
        // Pan with middle/right drag.
        if response.dragged_by(egui::PointerButton::Middle)
            || response.dragged_by(egui::PointerButton::Secondary)
        {
            self.canvas.offset += response.drag_delta();
        }

        let hover = pointer.and_then(|p| self.hit_test(&view, p));

        // Background + grid.
        painter.rect_filled(rect, CornerRadius::ZERO, canvas_bg);
        let step = view.px(5.0);
        if step > 7.0 {
            let mut x = rect.min.x + (view.origin.x - rect.min.x).rem_euclid(step);
            while x < rect.max.x {
                let mut y = rect.min.y + (view.origin.y - rect.min.y).rem_euclid(step);
                while y < rect.max.y {
                    painter.circle_filled(Pos2::new(x, y), 1.0, grid_dot);
                    y += step;
                }
                x += step;
            }
        }

        // Board.
        let live_bank = self.live.effective_bank();
        let live_out = self.live.live_output.clone();
        if let Some(board) = self.lib.board(&self.project.circuit.board_id).cloned() {
            let hovered_pin = match &hover {
                Some(Hit::Pin(k)) => Some(k.as_str()),
                _ => None,
            };
            let hovered_feature = match &hover {
                Some(Hit::Feature(f)) => Some(*f),
                _ => None,
            };
            // Function-group siblings light up while a pin is hovered.
            let group = hovered_pin.and_then(|k| board.pin_group(k));
            let group_keys: Vec<String> =
                group.as_ref().map(|(_, keys)| keys.clone()).unwrap_or_default();
            let group_color = ui.visuals().warn_fg_color;
            let bank = live_bank.clone();
            let out = live_out.clone();
            let serial_levels = (self.live.backend != crate::live::Backend::Simulator)
                .then_some(self.live.telemetry_levels);
            let live_fn = bank.map(|bank| {
                let out = out.clone();
                move |gpio: u8| -> PinLive {
                    let d = bank.get(gpio);
                    let high = match d.mode {
                        // Real hardware reports input levels via telemetry.
                        m if m.is_input() => match serial_levels {
                            Some(levels) => levels & (1u64 << gpio.min(63)) != 0,
                            None => out
                                .as_ref()
                                .and_then(|o| o.digital.get(&gpio).copied())
                                .unwrap_or(false),
                        },
                        PinMode::Pwm => d.duty > 0.5,
                        m if m.is_output() => d.out_high,
                        _ => false,
                    };
                    let mv = out.as_ref().and_then(|o| o.analog_mv.get(&gpio).copied());
                    PinLive { mode: d.mode, high, millivolts: mv }
                }
            });
            match &live_fn {
                Some(f) => draw::draw_board(
                    &painter,
                    &view,
                    &board,
                    self.project.circuit.board_pos,
                    hovered_pin,
                    Some(f),
                    &group_keys,
                    group_color,
                    hovered_feature,
                    live_out.as_ref().and_then(|o| o.rgb),
                ),
                None => draw::draw_board(
                    &painter,
                    &view,
                    &board,
                    self.project.circuit.board_pos,
                    hovered_pin,
                    None,
                    &group_keys,
                    group_color,
                    hovered_feature,
                    live_out.as_ref().and_then(|o| o.rgb),
                ),
            }
        }

        // Wires.
        let time = ui.input(|i| i.time);
        let flow = ui.visuals().warn_fg_color;
        for wire in self.project.circuit.wires.values() {
            let Some(pts) = self.wire_screen_path(&view, wire) else { continue };
            let selected = self.selection == Selection::Wire(wire.id);
            let live_mv = live_out.as_ref().and_then(|o| {
                let net = self.cache.netlist.net_of(&wire.a)?;
                o.net_avg_mv.get(net).copied().flatten()
            });
            draw::draw_wire(
                &painter,
                &pts,
                wire.color,
                selected,
                accent,
                flow,
                live_mv,
                time,
            );
            // Make it obvious where the selected wire lands.
            if selected
                && let (Some(pa), Some(pb)) = (pts.first(), pts.last()) {
                    for p in [pa, pb] {
                        painter.circle_stroke(*p, 6.5, Stroke::new(2.0, accent));
                    }
                }
        }

        // Components; live ones get an activity outline.
        let active = live_out.as_ref().map(|_| flow);
        let comp_ids: Vec<CompId> = self.project.circuit.components.keys().copied().collect();
        for id in &comp_ids {
            let comp = self.project.circuit.components.get(id).unwrap().clone();
            let Some(def) = self.lib.component(&comp.def_id).cloned() else { continue };
            let vis = live_out.as_ref().and_then(|o| o.visuals.get(id).copied());
            let selected = self.selection.contains_comp(*id);
            draw::draw_component(&painter, &view, &comp, &def, vis, selected, accent, active, time);
            // The simulated / mirrored screen contents of SPI displays.
            if def.id.starts_with("st7735")
                && let Some(ops) = live_out.as_ref().and_then(|o| o.lcd.as_ref()) {
                    draw::draw_lcd(&painter, &view, &comp, &def, ops);
                }
        }

        // Spotlight the components a hovered warning refers to.
        if !self.hover_highlight.is_empty() {
            let hl = ui.visuals().warn_fg_color;
            let pulse = 3.0 + (((time * 6.0).sin() as f32) + 1.0) * 2.0;
            for id in &self.hover_highlight {
                let Some(comp) = self.project.circuit.components.get(id) else { continue };
                let Some(def) = self.lib.component(&comp.def_id) else { continue };
                let body = Rect::from_center_size(
                    view.to_screen(comp.pos),
                    Vec2::new(view.px(def.visual.width_mm), view.px(def.visual.height_mm)),
                );
                painter.rect_stroke(
                    body.expand(pulse + 4.0),
                    CornerRadius::same(5),
                    Stroke::new(2.0, hl),
                    egui::StrokeKind::Outside,
                );
            }
            ui.ctx().request_repaint();
        }

        // Valid wire targets glow while a draft is active; shorts get crossed out.
        if let Some(from) = self.canvas.wire_from.clone()
            && let Some(board) = self.lib.board(&self.project.circuit.board_id).cloned() {
                let error_color = ui.visuals().error_fg_color;
                let pulse = 2.0 + (((time * 5.0).sin() as f32) + 1.0) * 1.2;
                let mark = |pos: Pos2, r: f32, verdict: &WireVerdict| match verdict {
                    WireVerdict::Ok => {
                        painter.circle_stroke(pos, r + pulse, Stroke::new(1.6, accent));
                    }
                    WireVerdict::Blocked(_) => {
                        painter.circle_filled(pos, r + 1.5, Color32::from_black_alpha(160));
                        let d = r * 0.8;
                        painter.line_segment(
                            [pos + Vec2::new(-d, -d), pos + Vec2::new(d, d)],
                            Stroke::new(1.5, error_color),
                        );
                    }
                    WireVerdict::Redundant => {}
                };
                for pin in &board.pins {
                    let ep = Endpoint::BoardPin { key: pin.key.clone() };
                    if ep == from {
                        continue;
                    }
                    let pos = view.to_screen(board_pin_world_pos(
                        &board,
                        pin,
                        self.project.circuit.board_pos,
                    ));
                    mark(pos, view.px(1.1).clamp(2.5, 7.0), &self.draft_verdict(&from, &ep));
                }
                for comp in self.project.circuit.components.values() {
                    let Some(def) = self.lib.component(&comp.def_id) else { continue };
                    for (i, t) in def.terminals.iter().enumerate() {
                        let ep =
                            Endpoint::Terminal { comp: comp.id, terminal: t.id.clone() };
                        if ep == from {
                            continue;
                        }
                        let pos = view.to_screen(terminal_world_pos(comp, def, i));
                        mark(pos, view.px(0.9).clamp(2.5, 6.0), &self.draft_verdict(&from, &ep));
                    }
                }
            }

        // Wire rubber band / placement ghost.
        if let (Some(from), Some(p)) = (&self.canvas.wire_from, pointer)
            && let Some(a) = self.endpoint_world(from) {
                let route = draw::Route {
                    exit_a: self.endpoint_exit_dir(from),
                    stub: view.px(3.0).clamp(8.0, 20.0),
                    ..Default::default()
                };
                let pts = draw::wire_path(view.to_screen(a), p, route);
                draw::draw_wire(
                    &painter,
                    &pts,
                    [accent.r(), accent.g(), accent.b()],
                    false,
                    accent,
                    flow,
                    None,
                    time,
                );
            }
        if let (Some(def_id), Some(p)) = (self.canvas.placing.clone(), pointer)
            && let Some(def) = self.lib.component(&def_id).cloned() {
                let world = view.to_world(p);
                let ghost = PlacedComponent {
                    id: CompId(0),
                    def_id,
                    pos: [world[0].round(), world[1].round()],
                    rotation: 0,
                    label: String::new(),
                    props: Default::default(),
                    state: CompState::initial(&def.sim),
                    script: None,
                };
                // Preview series insertion when hovering a wire (Ctrl skips).
                let ctrl = ui.input(|i| i.modifiers.ctrl);
                if !ctrl
                    && let Some((wid, ta, tb)) = self.splice_target(&view, p, &def, world)
                        && let Some(w) = self.project.circuit.wires.get(&wid).cloned() {
                            let t_of = |tid: &str| {
                                def.terminals
                                    .iter()
                                    .position(|t| t.id == tid)
                                    .map(|i| terminal_world_pos(&ghost, &def, i))
                            };
                            if let (Some(a), Some(b), Some(ta_w), Some(tb_w)) = (
                                self.endpoint_world(&w.a),
                                self.endpoint_world(&w.b),
                                t_of(&ta),
                                t_of(&tb),
                            ) {
                                for (from, to) in
                                    [(a, ta_w), (tb_w, b)]
                                {
                                    let pts = draw::wire_points(
                                        view.to_screen(from),
                                        view.to_screen(to),
                                    );
                                    draw::draw_wire(
                                        &painter,
                                        &pts,
                                        [accent.r(), accent.g(), accent.b()],
                                        false,
                                        accent,
                                        flow,
                                        None,
                                        time,
                                    );
                                }
                            }
                        }
                draw::draw_component(&painter, &view, &ghost, &def, None, false, accent, None, time);
            }

        // Hover tooltips for pins and board features.
        if let Some(Hit::Pin(key)) = &hover
            && let Some(board) = self.lib.board(&self.project.circuit.board_id)
                && let Some(pin) = board.pin(key) {
                    let mut text = format!("{} ({:?})", pin.key, pin.kind);
                    if !pin.caps.is_empty() {
                        text += &format!("\n{:?}", pin.caps);
                    }
                    if let Some((label, keys)) = board.pin_group(key)
                        && keys.len() > 1 {
                            text += &format!("\ngroup: {label} ({} pins)", keys.len());
                        }
                    if let Some(w) = &pin.warning {
                        text += &format!("\n⚠ {w}");
                    }
                    if let (Some(g), Some(out)) = (pin.kind.gpio(), &live_out)
                        && let Some(mv) = out.analog_mv.get(&g) {
                            text += &format!("\n{} mV", mv);
                        }
                    response.clone().on_hover_text(text);
                }
        if let Some(Hit::Feature(f)) = &hover
            && let Some(board) = self.lib.board(&self.project.circuit.board_id) {
                let text = match f {
                    draw::BoardControl::Reset => {
                        "RESET — click to reboot the board (EN pulse over serial)".to_string()
                    }
                    draw::BoardControl::Boot => format!(
                        "BOOT (GPIO{}) — click to reset into ROM download mode for flashing",
                        board.features.boot_button_gpio.unwrap_or(0)
                    ),
                    draw::BoardControl::RgbLed => format!(
                        "WS2812 RGB LED on GPIO{} (script color control coming with an RMT driver)",
                        board.features.rgb_led_gpio.unwrap_or(0)
                    ),
                };
                response.clone().on_hover_text(text);
            }

        // Button press-and-hold in live mode.
        let primary_down = ui.input(|i| i.pointer.primary_down());
        if let Some(id) = self.canvas.pressed_button
            && !primary_down {
                if let Some(c) = self.project.circuit.components.get_mut(&id) {
                    c.state = CompState::Button { pressed: false };
                    self.state_rev += 1;
                }
                self.canvas.pressed_button = None;
            }
        if self.live.connected() && ui.input(|i| i.pointer.primary_pressed())
            && let Some(Hit::Comp(id)) = &hover {
                let is_button = self
                    .project
                    .circuit
                    .components
                    .get(id)
                    .and_then(|c| self.lib.component(&c.def_id))
                    .is_some_and(|d| matches!(d.sim, SimModel::PushButton));
                if is_button
                    && let Some(c) = self.project.circuit.components.get_mut(id) {
                        c.state = CompState::Button { pressed: true };
                        self.state_rev += 1;
                        self.canvas.pressed_button = Some(*id);
                    }
            }

        // Context menu: remember what was under the cursor when it opened.
        if response.secondary_clicked()
            && let Some(p) = pointer {
                self.canvas.ctx_hit = self.hit_test(&view, p);
                self.canvas.ctx_world = view.to_world(p);
                if let Some(Hit::Comp(id)) = &self.canvas.ctx_hit
                    && !self.selection.contains_comp(*id) {
                        self.selection = Selection::Comp(*id);
                    }
            }
        response.context_menu(|ui| self.canvas_context_menu(ui));

        // Click handling.
        if response.clicked() {
            let hit = pointer.and_then(|p| self.hit_test(&view, p));
            if let Some(def_id) = self.canvas.placing.clone() {
                if let (Some(p), Some(def)) = (pointer, self.lib.component(&def_id).cloned()) {
                    let world = view.to_world(p);
                    let ctrl = ui.input(|i| i.modifiers.ctrl);
                    let splice = if ctrl {
                        None
                    } else {
                        self.splice_target(&view, p, &def, world)
                    };
                    let id = self.project.circuit.add_component(PlacedComponent {
                        id: CompId(0),
                        def_id,
                        pos: [world[0].round(), world[1].round()],
                        rotation: 0,
                        label: String::new(),
                        props: Default::default(),
                        state: CompState::initial(&def.sim),
                        script: None,
                    });
                    if let Some((wid, ta, tb)) = splice
                        && self.project.circuit.splice_component(wid, id, &ta, &tb) {
                            self.console.push(format!("{} spliced into the wire", def.name));
                        }
                    self.topo_rev += 1;
                    if !ui.input(|i| i.modifiers.shift) {
                        self.canvas.placing = None;
                    }
                }
            } else if let Some(from) = self.canvas.wire_from.clone() {
                let target = hit.as_ref().and_then(|h| self.hit_endpoint(h));
                match target {
                    Some(to) if to != from => match self.draft_verdict(&from, &to) {
                        WireVerdict::Ok => {
                            let color = self.wire_color_for(&from, &to);
                            self.project.circuit.add_wire(from, to, color);
                            self.topo_rev += 1;
                            self.canvas.wire_from = None;
                        }
                        WireVerdict::Redundant => {
                            self.console
                                .push("already connected — pick a different point".into());
                        }
                        WireVerdict::Blocked(why) => {
                            self.console.push(format!("✖ wire blocked: {why}"));
                        }
                    },
                    _ => self.canvas.wire_from = None,
                }
            } else {
                match &hit {
                    Some(Hit::Feature(f)) => match f {
                        BoardControl::Reset => self.live.board_reset(&mut self.console),
                        BoardControl::Boot => {
                            let boot = self
                                .lib
                                .board(&self.project.circuit.board_id)
                                .and_then(|b| b.features.boot_button_gpio);
                            self.live.board_boot_mode(boot, &mut self.console);
                        }
                        BoardControl::RgbLed => {
                            self.console
                                .push("RGB LED: script color control lands with the RMT driver".into());
                        }
                    },
                    Some(Hit::Pin(k)) => {
                        self.selection = Selection::Pin(k.clone());
                        self.canvas.wire_from = Some(Endpoint::BoardPin { key: k.clone() });
                    }
                    Some(Hit::Terminal(c, i)) => {
                        self.selection = Selection::Comp(*c);
                        self.canvas.wire_from = self.terminal_endpoint(*c, *i);
                    }
                    Some(Hit::Comp(id)) => {
                        self.selection = Selection::Comp(*id);
                        self.live_interact_click(*id);
                    }
                    Some(Hit::Wire(id)) => self.selection = Selection::Wire(*id),
                    None => self.selection = Selection::None,
                }
            }
        }

        // Component dragging or rubber-band selection (left drag, edit mode).
        if response.drag_started_by(egui::PointerButton::Primary)
            && self.canvas.placing.is_none()
            && self.canvas.wire_from.is_none()
            && self.canvas.pressed_button.is_none()
        {
            let start_pos = pointer.map(|p| p - response.drag_delta());
            let grabbed = match start_pos.and_then(|p| self.hit_test(&view, p)) {
                Some(Hit::Comp(id)) => Some(id),
                // A dot's centre terminal covers its whole body; dragging it
                // moves the dot (clicking still starts a wire).
                Some(Hit::Terminal(id, _)) if self.comp_is_dot(id) => Some(id),
                None => {
                    self.canvas.select_start = start_pos;
                    None
                }
                _ => None,
            };
            if let Some(id) = grabbed
                && let Some(c) = self.project.circuit.components.get(&id) {
                    let start = view.to_screen(c.pos);
                    let grab = pointer.unwrap_or(start) - start;
                    self.canvas.dragging = Some((id, grab));
                    if !self.selection.contains_comp(id) {
                        self.selection = Selection::Comp(id);
                    }
                }
        }
        if let Some((id, grab)) = self.canvas.dragging {
            if response.dragged_by(egui::PointerButton::Primary)
                && let Some(p) = pointer {
                    let world = view.to_world(p - grab);
                    let target = [world[0].round(), world[1].round()];
                    let group = self.selection.comp_ids();
                    let old = self.project.circuit.components.get(&id).map(|c| c.pos);
                    if let Some(old) = old {
                        let delta = [target[0] - old[0], target[1] - old[1]];
                        if delta != [0.0, 0.0] {
                            let ids =
                                if group.contains(&id) { group } else { vec![id] };
                            for cid in ids {
                                if let Some(c) = self.project.circuit.components.get_mut(&cid) {
                                    c.pos = [c.pos[0] + delta[0], c.pos[1] + delta[1]];
                                }
                            }
                        }
                    }
                }
            if response.drag_stopped() {
                self.canvas.dragging = None;
                self.topo_rev += 1;
            }
        }
        if let Some(start) = self.canvas.select_start {
            if let Some(p) = pointer {
                let rect_sel = Rect::from_two_pos(start, p);
                painter.rect_filled(
                    rect_sel,
                    CornerRadius::ZERO,
                    Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 18),
                );
                painter.rect_stroke(
                    rect_sel,
                    CornerRadius::ZERO,
                    egui::Stroke::new(1.0, accent),
                    egui::StrokeKind::Inside,
                );
                if response.drag_stopped() {
                    let hits: Vec<CompId> = self
                        .project
                        .circuit
                        .components
                        .values()
                        .filter(|c| rect_sel.contains(view.to_screen(c.pos)))
                        .map(|c| c.id)
                        .collect();
                    self.selection = match hits.len() {
                        0 => Selection::None,
                        1 => Selection::Comp(hits[0]),
                        _ => Selection::Comps(hits),
                    };
                    self.canvas.select_start = None;
                }
            }
            if !ui.input(|i| i.pointer.primary_down()) {
                self.canvas.select_start = None;
            }
        }

        // Keyboard: delete / rotate / escape.
        let typing = ui.ctx().egui_wants_keyboard_input();
        if !typing {
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.canvas.placing = None;
                self.canvas.wire_from = None;
            }
            if ui.input(|i| i.key_pressed(egui::Key::R)) {
                let ids = self.selection.comp_ids();
                for id in &ids {
                    if let Some(c) = self.project.circuit.components.get_mut(id) {
                        c.rotation = (c.rotation + 90) % 360;
                    }
                }
                if !ids.is_empty() {
                    self.topo_rev += 1;
                }
            }
            if ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
            {
                let ids = self.selection.comp_ids();
                if !ids.is_empty() {
                    for id in ids {
                        self.project.circuit.remove_component(id);
                    }
                    self.selection = Selection::None;
                    self.topo_rev += 1;
                } else if let Selection::Wire(id) = self.selection {
                    self.project.circuit.remove_wire(id);
                    self.selection = Selection::None;
                    self.topo_rev += 1;
                }
            }
        }

        // Status hints; a hovered short explains itself.
        let placing_over_wire = self.canvas.placing.is_some()
            && !ui.input(|i| i.modifiers.ctrl)
            && pointer
                .zip(self.canvas.placing.as_ref().and_then(|id| self.lib.component(id)).cloned())
                .is_some_and(|(p, def)| {
                    let world = view.to_world(p);
                    self.splice_target(&view, p, &def, world).is_some()
                });
        let mut hint = if placing_over_wire {
            "click to splice into this wire · hold Ctrl to place without wiring".to_string()
        } else if self.canvas.placing.is_some() {
            "click to place (shift = multiple, Esc = cancel) · drop on a wire to splice it in"
                .to_string()
        } else if self.canvas.wire_from.is_some() {
            "glowing pins are valid targets — click one to finish the wire (Esc = cancel)".into()
        } else if matches!(&self.selection, Selection::Comps(v) if v.len() >= 2) {
            format!(
                "{} selected — right-click or use ⚡ Auto wire in the toolbar · Del = delete all",
                self.selection.comp_ids().len()
            )
        } else {
            "click a pin to start a wire · drag empty space = select · right-click = menu · scroll = zoom · right-drag = pan".into()
        };
        let mut hint_fg = hint_color;
        if let Some(from) = &self.canvas.wire_from
            && let Some(to) = hover.as_ref().and_then(|h| self.hit_endpoint(h))
                && to != *from {
                    match self.draft_verdict(from, &to) {
                        WireVerdict::Blocked(why) => {
                            hint = format!("✖ {why}");
                            hint_fg = ui.visuals().error_fg_color;
                        }
                        WireVerdict::Redundant => {
                            hint = "already connected — this wire would do nothing".into();
                        }
                        WireVerdict::Ok => {}
                    }
                }
        painter.text(
            rect.left_bottom() + Vec2::new(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            hint,
            egui::FontId::proportional(12.0),
            hint_fg,
        );
    }

    fn terminal_endpoint(&self, comp: CompId, index: usize) -> Option<Endpoint> {
        let c = self.project.circuit.components.get(&comp)?;
        let def = self.lib.component(&c.def_id)?;
        let t = def.terminals.get(index)?;
        Some(Endpoint::Terminal { comp, terminal: t.id.clone() })
    }

    fn canvas_context_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(190.0);
        let world = self.canvas.ctx_world;

        if let Some(Hit::Comp(id)) = self.canvas.ctx_hit.clone() {
            let has_script = self
                .project
                .circuit
                .components
                .get(&id)
                .is_some_and(|c| c.script.is_some());
            if ui
                .button(if has_script { "📜 Edit script" } else { "📜 Attach script" })
                .clicked()
            {
                self.selection = Selection::Comp(id);
                if !has_script {
                    self.attach_template_script(id);
                }
                self.open_script_tab(id);
                ui.close();
            }
            if ui.button("⟳ Rotate").clicked() {
                if let Some(c) = self.project.circuit.components.get_mut(&id) {
                    c.rotation = (c.rotation + 90) % 360;
                    self.topo_rev += 1;
                }
                ui.close();
            }
            if ui.button("🗑 Delete").clicked() {
                self.project.circuit.remove_component(id);
                self.selection = Selection::None;
                self.topo_rev += 1;
                ui.close();
            }
            ui.separator();
        }

        let ids = self.selection.comp_ids();
        let autowire_label = match ids.len() {
            0 | 1 => match &self.canvas.ctx_hit {
                Some(Hit::Comp(_)) => Some("⚡ Auto wire to board".to_string()),
                _ => None,
            },
            n => Some(format!("⚡ Auto wire {n} selected")),
        };
        if let Some(label) = autowire_label
            && ui.button(label).clicked() {
                if ids.is_empty()
                    && let Some(Hit::Comp(id)) = self.canvas.ctx_hit {
                        self.selection = Selection::Comp(id);
                    }
                self.apply_auto_wire();
                ui.close();
            }

        ui.menu_button("➕ Add component", |ui| {
            let mut categories: Vec<String> =
                self.lib.components.values().map(|c| c.category.clone()).collect();
            categories.sort();
            categories.dedup();
            let mut place: Option<String> = None;
            for cat in categories {
                ui.menu_button(&cat, |ui| {
                    for def in self.lib.components.values().filter(|c| c.category == cat) {
                        if ui.button(&def.name).clicked() {
                            place = Some(def.id.clone());
                            ui.close();
                        }
                    }
                });
            }
            if let Some(def_id) = place {
                self.place_at(&def_id, world);
            }
        });
        if ui
            .button(format!("{} Add routing dot", egui_phosphor::regular::DOT_OUTLINE))
            .clicked()
        {
            self.place_at("junction-dot", world);
            ui.close();
        }
    }

    /// Drop a library component at a world position.
    fn place_at(&mut self, def_id: &str, world: [f32; 2]) {
        let Some(def) = self.lib.component(def_id) else {
            self.console.push(format!("unknown component '{def_id}'"));
            return;
        };
        let sim = def.sim.clone();
        self.project.circuit.add_component(PlacedComponent {
            id: CompId(0),
            def_id: def_id.to_string(),
            pos: [world[0].round(), world[1].round()],
            rotation: 0,
            label: String::new(),
            props: Default::default(),
            state: CompState::initial(&sim),
            script: None,
        });
        self.topo_rev += 1;
    }

    /// Wire the current selection to the board automatically.
    pub fn apply_auto_wire(&mut self) {
        let ids = self.selection.comp_ids();
        if ids.is_empty() {
            return;
        }
        let Some(board) = self.lib.board(&self.project.circuit.board_id).cloned() else {
            return;
        };
        let plan =
            wirelab_core::autowire::auto_wire(&self.project.circuit, &board, &self.lib, &ids);
        let n = plan.wires.len();
        for (a, b) in plan.wires {
            let color = self.wire_color_for(&a, &b);
            self.project.circuit.add_wire(a, b, color);
        }
        for note in plan.notes {
            self.console.push(format!("auto-wire: {note}"));
        }
        if n > 0 {
            self.topo_rev += 1;
            self.console.push(format!("auto-wire: {n} wire(s) added"));
        } else {
            self.console.push("auto-wire: nothing to do".into());
        }
    }

    /// Perform a one-click lint remedy.
    pub fn apply_lint_fix(&mut self, fix: &wirelab_core::validate::LintFix) {
        match fix {
            wirelab_core::validate::LintFix::SpliceResistor { wire, ohms, .. } => {
                let Some(w) = self.project.circuit.wires.get(wire).cloned() else {
                    self.console.push("that wire changed — re-check the warning".into());
                    return;
                };
                // Nearest stock resistor to the computed value.
                let Some(def) = self
                    .lib
                    .components
                    .values()
                    .filter_map(|d| match d.sim {
                        SimModel::Resistor { ohms: o } => Some((d, (o - ohms).abs())),
                        _ => None,
                    })
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(d, _)| d.clone())
                else {
                    self.console.push("no resistor in the library".into());
                    return;
                };
                let (Some(a), Some(b)) =
                    (self.endpoint_world(&w.a), self.endpoint_world(&w.b))
                else {
                    return;
                };
                let mid = [((a[0] + b[0]) / 2.0).round(), ((a[1] + b[1]) / 2.0).round()];
                let id = self.project.circuit.add_component(PlacedComponent {
                    id: CompId(0),
                    def_id: def.id.clone(),
                    pos: mid,
                    rotation: 0,
                    label: String::new(),
                    props: Default::default(),
                    state: CompState::None,
                    script: None,
                });
                let (ta, tb) = (def.terminals[0].id.clone(), def.terminals[1].id.clone());
                if self.project.circuit.splice_component(*wire, id, &ta, &tb) {
                    let val = match def.sim {
                        SimModel::Resistor { ohms } => ohms,
                        _ => *ohms,
                    };
                    self.console.push(format!(
                        "spliced a {val:.0} Ω resistor in series (computed ≈{ohms:.0} Ω)"
                    ));
                    self.topo_rev += 1;
                }
            }
        }
    }

    /// Generate and attach the starter script for a component.
    pub fn attach_template_script(&mut self, id: CompId) {
        let names = wirelab_core::script::component_names(&self.project.circuit, &self.lib);
        let own = names.get(&id).cloned().unwrap_or_default();
        let peers: Vec<String> =
            names.iter().filter(|(c, _)| **c != id).map(|(_, n)| n.clone()).collect();
        let Some(def) = self
            .project
            .circuit
            .components
            .get(&id)
            .and_then(|c| self.lib.component(&c.def_id))
        else {
            return;
        };
        let template = wirelab_core::script::script_template(def, &own, &peers);
        if let Some(c) = self.project.circuit.components.get_mut(&id) {
            c.script = Some(template.clone());
        }
        self.script_ed.stash.remove(&id);
        if self.script_ed.comp == Some(id) {
            self.script_ed.buffer = template;
        }
        self.script_rev += 1;
        if !self.live.connected() {
            self.live.scripts.sync(&self.project.circuit, &self.lib);
        }
    }
}
