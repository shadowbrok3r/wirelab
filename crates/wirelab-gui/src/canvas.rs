//! Interactive circuit canvas: pan/zoom, placement, wiring, live pokes.

use egui::{Color32, CornerRadius, Pos2, Rect, Sense, Stroke, Vec2};
use wirelab_core::breadboard::{self, HoleRef};
use wirelab_core::circuit::{BbId, CompId, Endpoint, PlacedBreadboard, PlacedComponent, WireId};
use wirelab_core::component::{CompState, SimModel};
use wirelab_core::netlist::{WireVerdict, wire_verdict};
use wirelab_proto::PinMode;

use egui_theme_kit::controls::{self, BarKind};

use crate::app::{Selection, WireLabApp};
use crate::dock::Tab;
use crate::ui_kit::{self, ph};
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
    /// Breadboard being dragged, grab offset, and the parts seated in it.
    pub dragging_bb: Option<(BbId, Vec2, Vec<CompId>)>,
    pub pressed_button: Option<CompId>,
    /// Screen-space anchor of an in-progress rubber-band selection.
    pub select_start: Option<Pos2>,
    /// Hit and world position captured when the context menu opened.
    ctx_hit: Option<Hit>,
    ctx_world: [f32; 2],
    /// Screen rect the canvas filled last frame, for toolbar zoom and fit.
    pub view_rect: Rect,
    /// Fit the circuit into view on the next frame.
    pub fit_pending: bool,
}

impl Default for CanvasState {
    fn default() -> Self {
        CanvasState {
            offset: Vec2::new(60.0, 40.0),
            zoom: 1.4,
            placing: None,
            wire_from: None,
            dragging: None,
            dragging_bb: None,
            pressed_button: None,
            select_start: None,
            ctx_hit: None,
            ctx_world: [0.0, 0.0],
            view_rect: Rect::NOTHING,
            fit_pending: true,
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
    Hole(BbId, HoleRef),
    Breadboard(BbId),
}

/// Leads snap to a hole within this distance while dragging or placing.
const SNAP_MM: f32 = 1.4;

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
                Some(board_pin_world_pos(board, pin, self.project.circuit.board_pos, self.project.circuit.board_rotation))
            }
            Endpoint::Terminal { comp, terminal } => {
                let c = self.project.circuit.components.get(comp)?;
                let def = self.lib.component(&c.def_id)?;
                let idx = def.terminals.iter().position(|t| &t.id == terminal)?;
                Some(terminal_world_pos(c, def, idx))
            }
            Endpoint::Hole { bb, hole } => {
                breadboard::hole_endpoint_world(&self.project.circuit, &self.lib, *bb, hole)
            }
        }
    }

    /// Which way a wire should leave this endpoint: away from the board
    /// edge for pins, away from the body for component terminals.
    fn endpoint_exit_dir(&self, ep: &Endpoint) -> Vec2 {
        let Some(board) = self.lib.board(&self.project.circuit.board_id) else {
            return Vec2::ZERO;
        };
        let d = wirelab_core::geometry::endpoint_exit_dir(
            &self.project.circuit,
            board,
            &self.lib,
            ep,
        );
        Vec2::new(d[0], d[1])
    }

    /// Pixels a doubling-back wire must travel to clear this endpoint's body.
    fn endpoint_clearance(&self, view: &View, ep: &Endpoint) -> f32 {
        match ep {
            Endpoint::BoardPin { .. } | Endpoint::Hole { .. } => 0.0,
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
        if !wire.waypoints.is_empty() {
            let at = |p: [f32; 2]| {
                let s = view.to_screen(p);
                [s.x, s.y]
            };
            let (sa, sb) = (at(a), at(b));
            let wps: Vec<[f32; 2]> = wire.waypoints.iter().map(|&p| at(p)).collect();
            return Some(
                wirelab_core::geometry::waypoint_path(sa, &wps, sb, 9.0)
                    .into_iter()
                    .map(|p| Pos2::new(p[0], p[1]))
                    .collect(),
            );
        }
        let ea = self.endpoint_exit_dir(&wire.a);
        let eb = self.endpoint_exit_dir(&wire.b);
        let route = draw::Route {
            exit_a: [ea.x, ea.y],
            exit_b: [eb.x, eb.y],
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
                    self.project.circuit.board_rotation,
                ));
                if p.distance(pointer) < PIN_HIT_RADIUS_PX {
                    return Some(Hit::Pin(pin.key.clone()));
                }
            }
            for (kind, world_rect) in board_feature_rects(board, self.project.circuit.board_pos, self.project.circuit.board_rotation)
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
        let world = view.to_world(pointer);
        let tol = (PIN_HIT_RADIUS_PX / view.scale).min(breadboard::PITCH_MM * 0.45);
        if let Some((bb, h)) = breadboard::hole_at(&self.project.circuit, &self.lib, world, tol) {
            return Some(Hit::Hole(bb, h));
        }
        for wire in self.project.circuit.wires.values() {
            let Some(pts) = self.wire_screen_path(view, wire) else { continue };
            if draw::wire_hit_distance(&pts, pointer) < 6.0 {
                return Some(Hit::Wire(wire.id));
            }
        }
        self.breadboard_at(world).map(Hit::Breadboard)
    }

    fn breadboard_at(&self, world: [f32; 2]) -> Option<BbId> {
        self.project.circuit.breadboards.values().rev().find_map(|bb| {
            let p = self.lib.breadboard(&bb.profile)?;
            let l = breadboard::world_to_local(bb, p, world);
            (l[0] >= 0.0 && l[1] >= 0.0 && l[0] <= p.width_mm && l[1] <= p.height_mm).then_some(bb.id)
        })
    }

    fn hole_endpoint(&self, bb: BbId, h: HoleRef) -> Option<Endpoint> {
        let p = self.lib.breadboard(&self.project.circuit.breadboards.get(&bb)?.profile)?;
        Some(Endpoint::Hole { bb, hole: p.hole_name(h) })
    }

    /// Hole a breadboard endpoint or seated lead sits in.
    fn endpoint_hole(&self, ep: &Endpoint) -> Option<(BbId, HoleRef)> {
        match ep {
            Endpoint::Hole { bb, hole } => {
                let p = self.lib.breadboard(&self.project.circuit.breadboards.get(bb)?.profile)?;
                Some((*bb, p.parse_hole(hole)?))
            }
            other => self.cache.seating.seat_of(other).map(|s| (s.bb, s.hole)),
        }
    }

    /// Part position adjusted so its closest lead drops into a nearby hole.
    fn snap_to_holes(&self, comp: &PlacedComponent, pos: [f32; 2]) -> [f32; 2] {
        let rounded = [pos[0].round(), pos[1].round()];
        let Some(def) = self.lib.component(&comp.def_id) else { return rounded };
        if def.leads.is_empty() || self.project.circuit.breadboards.is_empty() {
            return rounded;
        }
        let mut probe = comp.clone();
        probe.pos = pos;
        let mut best: Option<([f32; 2], f32)> = None;
        for i in 0..def.terminals.len() {
            let t = terminal_world_pos(&probe, def, i);
            let Some((bb, h)) = breadboard::hole_at(&self.project.circuit, &self.lib, t, SNAP_MM) else {
                continue;
            };
            let Some(hw) = self.hole_endpoint(bb, h).and_then(|ep| self.endpoint_world(&ep)) else { continue };
            let d = (hw[0] - t[0]).hypot(hw[1] - t[1]);
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some(([hw[0] - t[0], hw[1] - t[1]], d));
            }
        }
        match best {
            Some((d, _)) => [pos[0] + d[0], pos[1] + d[1]],
            None => rounded,
        }
    }

    /// Parts whose every lead sits in this breadboard; they ride along with it.
    pub(crate) fn parts_seated_in(&self, bb: BbId) -> Vec<CompId> {
        let board = self.lib.board(&self.project.circuit.board_id);
        let seating = breadboard::seating(&self.project.circuit, board, &self.lib);
        self.project
            .circuit
            .components
            .values()
            .filter(|c| {
                let Some(def) = self.lib.component(&c.def_id) else { return false };
                !def.leads.is_empty()
                    && def.terminals.iter().all(|t| {
                        let ep = Endpoint::Terminal { comp: c.id, terminal: t.id.clone() };
                        seating.seat_of(&ep).is_some_and(|s| s.bb == bb)
                    })
            })
            .map(|c| c.id)
            .collect()
    }

    /// Quarter-turn a breadboard about its centre, carrying its seated parts.
    pub fn rotate_breadboard(&mut self, id: BbId) {
        let carried = self.parts_seated_in(id);
        let Some(bb) = self.project.circuit.breadboards.get_mut(&id) else { return };
        bb.rotation = (bb.rotation + 90) % 360;
        let center = bb.pos;
        for cid in carried {
            if let Some(c) = self.project.circuit.components.get_mut(&cid) {
                let r = wirelab_core::geometry::rotate([c.pos[0] - center[0], c.pos[1] - center[1]], 90);
                c.pos = [center[0] + r[0], center[1] + r[1]];
                c.rotation = (c.rotation + 90) % 360;
            }
        }
        self.topo_rev += 1;
    }

    /// Add a breadboard from the library, centred at a world position.
    pub fn add_breadboard_at(&mut self, profile: &str, world: [f32; 2]) -> Option<BbId> {
        if self.lib.breadboard(profile).is_none() {
            self.console.push(format!("unknown breadboard '{profile}'"));
            return None;
        }
        let id = self.project.circuit.add_breadboard(PlacedBreadboard {
            id: BbId(0),
            profile: profile.to_string(),
            pos: [world[0].round(), world[1].round()],
            rotation: 0,
            label: String::new(),
        });
        self.topo_rev += 1;
        Some(id)
    }

    fn wire_color_for(&self, a: &Endpoint, b: &Endpoint) -> [u8; 3] {
        let rail = |ep: &Endpoint| -> Option<[u8; 3]> {
            if let Endpoint::Hole { bb, hole } = ep {
                let p = self.lib.breadboard(&self.project.circuit.breadboards.get(bb)?.profile)?;
                return match p.lines.get(p.parse_hole(hole)?.line)?.polarity.as_str() {
                    "+" => Some([220, 60, 50]),
                    "-" => Some([40, 40, 40]),
                    _ => None,
                };
            }
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
            Hit::Hole(bb, h) => self.hole_endpoint(*bb, *h),
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
        self.canvas.view_rect = rect;
        if self.canvas.fit_pending && rect.is_positive() {
            self.canvas.fit_pending = false;
            self.fit_view();
        }
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

        // Breadboards, under everything else; the copper under the cursor lights up.
        let hover_hole = match &hover {
            Some(Hit::Hole(bb, h)) => Some((*bb, *h)),
            Some(Hit::Terminal(c, i)) => {
                self.terminal_endpoint(*c, *i).and_then(|ep| self.endpoint_hole(&ep))
            }
            _ => None,
        }
        .or_else(|| self.canvas.wire_from.as_ref().and_then(|ep| self.endpoint_hole(ep)));
        let bus_color = ui.visuals().warn_fg_color;
        for bb in self.project.circuit.breadboards.values() {
            let Some(p) = self.lib.breadboard(&bb.profile) else { continue };
            let (bus, hovered) = match hover_hole {
                Some((id, h)) if id == bb.id => (p.bus_holes(h), Some(h)),
                _ => (Vec::new(), None),
            };
            let paint = crate::breadboard_view::BbPaint {
                bus: &bus,
                bus_color,
                hovered,
                selected: self.selection == Selection::Breadboard(bb.id),
                accent,
            };
            crate::breadboard_view::draw(&painter, &view, bb, p, &paint);
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
                    self.project.circuit.board_rotation,
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
                    self.project.circuit.board_rotation,
                    hovered_pin,
                    None,
                    &group_keys,
                    group_color,
                    hovered_feature,
                    live_out.as_ref().and_then(|o| o.rgb),
                ),
            }
        }

        // On-board display glass, showing the simulated panel when no display part claims it.
        if let Some([dx, dy, dw, dh]) =
            self.lib.board(&self.project.circuit.board_id).and_then(|b| b.features.display)
        {
            let (bp, rot) = (self.project.circuit.board_pos, self.project.circuit.board_rotation);
            let board = self.lib.board(&self.project.circuit.board_id).expect("board with a display");
            let corner = |p: [f32; 2]| view.to_screen(wirelab_core::geometry::board_local_to_world(board, bp, rot, p));
            let glass = egui::Rect::from_two_pos(corner([dx, dy]), corner([dx + dw, dy + dh]));
            painter.rect_filled(glass.expand(view.px(0.6)), CornerRadius::same(3), Color32::from_gray(22));
            painter.rect_filled(glass, CornerRadius::same(1), Color32::from_rgb(4, 5, 8));
            let claimed = self.project.circuit.components.values().any(|c| c.def_id.starts_with("st7735"));
            if !claimed
                && let Some(ops) = live_out.as_ref().and_then(|o| o.lcd.as_ref())
            {
                let side = glass.width().min(glass.height());
                let square = egui::Rect::from_center_size(glass.center(), egui::Vec2::splat(side));
                let texture = self.lcd_texture.texture(ui.ctx(), ops);
                crate::lcd::paint(&painter, square, self.project.circuit.screen_rotation(), texture);
            }
        }
        if let Some(board) = self.lib.board(&self.project.circuit.board_id) {
            draw::draw_board_pin_labels(&painter, &view, board, self.project.circuit.board_pos, self.project.circuit.board_rotation);
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
                && let Some(ops) = live_out.as_ref().and_then(|o| o.lcd.as_ref())
            {
                let texture = self.lcd_texture.texture(ui.ctx(), ops);
                crate::lcd::paint(&painter, crate::lcd::screen_rect(&view, &comp, &def), comp.rotation, texture);
            }
            self.paint_script_badge(ui, &view, &comp, &def, time);
        }

        // Inserted leads get a solder fillet; leads that miss a hole are crossed out.
        for seat in &self.cache.seating.seats {
            if let Some(w) = self.endpoint_world(&seat.ep) {
                crate::breadboard_view::draw_seat(&painter, &view, w);
            }
        }
        let miss_color = ui.visuals().error_fg_color;
        for miss in &self.cache.seating.misses {
            crate::breadboard_view::draw_miss(&painter, &view, miss.world, miss_color);
        }

        // Spotlight the components a hovered warning refers to.
        if !self.hover_highlight.is_empty() {
            let hl = ui.visuals().warn_fg_color;
            let pulse = 3.0 + (((time * 6.0).sin() as f32) + 1.0) * 2.0;
            for id in &self.hover_highlight {
                let Some(comp) = self.project.circuit.components.get(id) else { continue };
                let Some(def) = self.lib.component(&comp.def_id) else { continue };
                let body = Self::body_rect(&view, comp, def);
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
                        self.project.circuit.board_rotation,
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
                let ea = self.endpoint_exit_dir(from);
                let route = draw::Route {
                    exit_a: [ea.x, ea.y],
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
                let mut ghost = PlacedComponent {
                    id: CompId(0),
                    def_id,
                    pos: [world[0].round(), world[1].round()],
                    rotation: 0,
                    label: String::new(),
                    props: Default::default(),
                    state: CompState::initial(&def.sim),
                    script: None,
                };
                ghost.pos = self.snap_to_holes(&ghost, world);
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

        if let Some(Hit::Hole(bb, h)) = &hover {
            response.clone().on_hover_text(self.hole_tooltip(*bb, *h));
        }
        if let Some(Hit::Comp(id)) = &hover
            && self.canvas.dragging.is_none()
        {
            let id = *id;
            response.clone().on_hover_ui_at_pointer(|ui| self.comp_hover_card(ui, id));
        }
        if response.double_clicked()
            && let Some(Hit::Comp(id)) = &hover
        {
            self.selection = Selection::Comp(*id);
            self.open_tab(Tab::Script(*id));
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
                    .is_some_and(|d| matches!(d.sim, SimModel::PushButton | SimModel::ArcContact { .. }));
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
                    let mut placed = PlacedComponent {
                        id: CompId(0),
                        def_id,
                        pos: [world[0].round(), world[1].round()],
                        rotation: 0,
                        label: String::new(),
                        props: Default::default(),
                        state: CompState::initial(&def.sim),
                        script: None,
                    };
                    placed.pos = self.snap_to_holes(&placed, world);
                    let id = self.project.circuit.add_component(placed);
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
                    Some(Hit::Hole(bb, h)) => {
                        self.canvas.wire_from = self.hole_endpoint(*bb, *h);
                    }
                    Some(Hit::Breadboard(id)) => self.selection = Selection::Breadboard(*id),
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
                Some(Hit::Breadboard(bb)) | Some(Hit::Hole(bb, _)) => {
                    if let Some(b) = self.project.circuit.breadboards.get(&bb) {
                        let start = view.to_screen(b.pos);
                        let grab = start_pos.unwrap_or(start) - start;
                        self.canvas.dragging_bb = Some((bb, grab, self.parts_seated_in(bb)));
                        self.selection = Selection::Breadboard(bb);
                    }
                    None
                }
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
                    let target = match self.project.circuit.components.get(&id) {
                        Some(c) => self.snap_to_holes(c, world),
                        None => [world[0].round(), world[1].round()],
                    };
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
        if let Some((bb, grab, carried)) = self.canvas.dragging_bb.clone() {
            if response.dragged_by(egui::PointerButton::Primary)
                && let Some(p) = pointer
            {
                let world = view.to_world(p - grab);
                let target = [(world[0] * 10.0).round() / 10.0, (world[1] * 10.0).round() / 10.0];
                if let Some(b) = self.project.circuit.breadboards.get_mut(&bb) {
                    let delta = [target[0] - b.pos[0], target[1] - b.pos[1]];
                    b.pos = target;
                    for cid in &carried {
                        if let Some(c) = self.project.circuit.components.get_mut(cid) {
                            c.pos = [c.pos[0] + delta[0], c.pos[1] + delta[1]];
                        }
                    }
                }
            }
            if response.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
                self.canvas.dragging_bb = None;
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
                if let Selection::Breadboard(id) = self.selection {
                    self.rotate_breadboard(id);
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
                } else if let Selection::Breadboard(id) = self.selection {
                    self.project.circuit.remove_breadboard(id);
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

    /// Hole name, its copper, the net it joins and what sits on that copper.
    fn hole_tooltip(&self, bb: BbId, h: HoleRef) -> String {
        let Some(p) = self.project.circuit.breadboards.get(&bb).and_then(|b| self.lib.breadboard(&b.profile))
        else {
            return String::new();
        };
        let mut text = format!("{} · {}", p.hole_name(h), p.node_label(h));
        let node = p.node_of(h);
        let mut on_copper: Vec<String> = self
            .cache
            .seating
            .seats
            .iter()
            .filter(|s| s.bb == bb && p.node_of(s.hole) == node)
            .map(|s| format!("{} in {}", self.endpoint_label(&s.ep), s.hole_name))
            .collect();
        for w in self.project.circuit.wires.values() {
            for (here, there) in [(&w.a, &w.b), (&w.b, &w.a)] {
                if let Endpoint::Hole { bb: b, hole } = here
                    && *b == bb
                    && p.parse_hole(hole).is_some_and(|o| p.node_of(o) == node)
                {
                    on_copper.push(format!("wire {hole} -> {}", self.endpoint_label(there)));
                }
            }
        }
        if let Some(net) = self.hole_endpoint(bb, h).and_then(|ep| self.cache.netlist.net_of(&ep)) {
            let name = self
                .project
                .circuit
                .net_names
                .iter()
                .find(|(_, ep)| self.cache.netlist.net_of(ep) == Some(net))
                .map(|(n, _)| n.clone());
            if let Some(name) = name {
                text += &format!("\nnet {name}");
            }
        }
        for line in on_copper {
            text += &format!("\n  {line}");
        }
        if self.canvas.wire_from.is_none() {
            text += "\nclick to start a wire from this hole";
        }
        text
    }

    /// Short readable name for an endpoint, e.g. "R1.a", "GPIO4", "E12".
    pub fn endpoint_label(&self, ep: &Endpoint) -> String {
        match ep {
            Endpoint::BoardPin { key } => key.clone(),
            Endpoint::Terminal { comp, terminal } => format!("{}.{terminal}", self.comp_name(*comp)),
            Endpoint::Hole { bb, hole } => {
                if self.project.circuit.breadboards.len() > 1 {
                    format!("BB{}:{hole}", bb.0)
                } else {
                    hole.clone()
                }
            }
        }
    }

    fn terminal_endpoint(&self, comp: CompId, index: usize) -> Option<Endpoint> {
        let c = self.project.circuit.components.get(&comp)?;
        let def = self.lib.component(&c.def_id)?;
        let t = def.terminals.get(index)?;
        Some(Endpoint::Terminal { comp, terminal: t.id.clone() })
    }

    fn canvas_context_menu(&mut self, ui: &mut egui::Ui) {
        let world = self.canvas.ctx_world;
        controls::menu_body(ui, |ui| {
            if let Some(Hit::Comp(id)) = self.canvas.ctx_hit.clone() {
                let has_script = self.project.circuit.components.get(&id).is_some_and(|c| c.script.is_some());
                let (icon, label) = if has_script { (ph::FILE_CODE, "Edit script") } else { (ph::FILE_PLUS, "Attach script") };
                if ui_kit::menu_item(ui, icon, label).clicked() {
                    self.selection = Selection::Comp(id);
                    if !has_script {
                        self.attach_template_script(id);
                    }
                    self.open_script_tab(id);
                    ui.close();
                }
                if ui_kit::menu_item(ui, ph::ARROW_CLOCKWISE, "Rotate").clicked() {
                    if let Some(c) = self.project.circuit.components.get_mut(&id) {
                        c.rotation = (c.rotation + 90) % 360;
                        self.topo_rev += 1;
                    }
                    ui.close();
                }
                if controls::menu_danger_item(ui, ui_kit::kit(ph::TRASH), "Delete").clicked() {
                    self.project.circuit.remove_component(id);
                    self.selection = Selection::None;
                    self.topo_rev += 1;
                    ui.close();
                }
                controls::menu_separator(ui);
            }

            let bb_hit = match self.canvas.ctx_hit {
                Some(Hit::Breadboard(id)) | Some(Hit::Hole(id, _)) => Some(id),
                _ => None,
            };
            if let Some(id) = bb_hit {
                if ui_kit::menu_item(ui, ph::ARROW_CLOCKWISE, "Rotate breadboard").clicked() {
                    self.rotate_breadboard(id);
                    ui.close();
                }
                if controls::menu_danger_item(ui, ui_kit::kit(ph::TRASH), "Delete breadboard").clicked() {
                    self.project.circuit.remove_breadboard(id);
                    self.selection = Selection::None;
                    self.topo_rev += 1;
                    ui.close();
                }
                controls::menu_separator(ui);
            }

            let ids = self.selection.comp_ids();
            let autowire_label = match ids.len() {
                0 | 1 => matches!(self.canvas.ctx_hit, Some(Hit::Comp(_))).then(|| "Auto wire to board".to_string()),
                n => Some(format!("Auto wire {n} selected")),
            };
            if let Some(label) = autowire_label
                && ui_kit::menu_item(ui, ph::MAGIC_WAND, &label).clicked()
            {
                if ids.is_empty()
                    && let Some(Hit::Comp(id)) = self.canvas.ctx_hit
                {
                    self.selection = Selection::Comp(id);
                }
                self.apply_auto_wire();
                ui.close();
            }

            let mut categories: Vec<String> = self.lib.components.values().map(|c| c.category.clone()).collect();
            categories.sort();
            categories.dedup();
            let mut place: Option<String> = None;
            let _ = controls::submenu(ui, ui_kit::kit(ph::PLUS), "Add part", |ui| {
                for cat in &categories {
                    let _ = controls::submenu(ui, ui_kit::kit(crate::icons::category(cat)), cat, |ui| {
                        for def in self.lib.components.values().filter(|c| &c.category == cat) {
                            if controls::menu_item(ui, ui_kit::kit(crate::icons::part(def)), &def.name).clicked() {
                                place = Some(def.id.clone());
                                ui.close();
                            }
                        }
                    });
                }
            });
            if let Some(def_id) = place {
                self.place_at(&def_id, world);
            }
            if controls::menu_item(ui, ui_kit::kit(crate::icons::PART_JUNCTION), "Add routing dot").clicked() {
                self.place_at("junction-dot", world);
                ui.close();
            }
            let profiles: Vec<(String, String)> =
                self.lib.breadboards.values().map(|p| (p.id.clone(), p.name.clone())).collect();
            if !profiles.is_empty() {
                let mut add: Option<String> = None;
                if ui_kit::menu_item(ui, ph::ARROW_CLOCKWISE, "Rotate dev board").clicked() {
                    let c = &mut self.project.circuit;
                    c.board_rotation = (c.board_rotation + 90) % 360;
                    self.topo_rev += 1;
                    ui.close();
                }
                let has_display =
                    self.lib.board(&self.project.circuit.board_id).is_some_and(|b| b.features.display.is_some());
                if has_display && ui_kit::menu_item(ui, ph::ARROWS_CLOCKWISE, "Rotate display 180°").clicked() {
                    let c = &mut self.project.circuit;
                    c.display_rotation = (c.display_rotation + 180) % 360;
                    ui.close();
                }
                let _ = controls::submenu(ui, ui_kit::kit(ph::GRID_FOUR), "Add breadboard", |ui| {
                    for (id, name) in &profiles {
                        if controls::menu_item(ui, ui_kit::kit(ph::GRID_FOUR), name).clicked() {
                            add = Some(id.clone());
                            ui.close();
                        }
                    }
                });
                if let Some(id) = add
                    && let Some(bb) = self.add_breadboard_at(&id, world)
                {
                    self.selection = Selection::Breadboard(bb);
                }
            }
        });
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
        self.docs.set_buffer(id, &self.project.circuit, template);
        self.script_rev += 1;
        if !self.live.connected() {
            self.live.scripts.sync(&self.project.circuit, &self.lib);
        }
        self.open_tab(Tab::Script(id));
    }

    /// The schematic tab: toolbar over the canvas.
    pub fn show_schematic(&mut self, ui: &mut egui::Ui) {
        let m = egui_theme_kit::metrics(ui.ctx());
        egui::Panel::top("schematic-toolbar")
            .exact_size(m.bar_h)
            .frame(egui::Frame::NONE.fill(ui.visuals().panel_fill))
            .show_separator_line(false)
            .show(ui, |ui| controls::bar(ui, BarKind::Tool, |ui| self.schematic_toolbar(ui)));
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| self.show_canvas(ui));
    }

    fn schematic_toolbar(&mut self, ui: &mut egui::Ui) {
        let control_h = egui_theme_kit::metrics(ui.ctx()).control_h;
        if ui_kit::tool(ui, ph::MAGNIFYING_GLASS_MINUS, "Zoom out").clicked() {
            self.zoom_by(1.0 / 1.25);
        }
        ui.label(egui::RichText::new(format!("{:.0}%", self.canvas.zoom * 100.0)).monospace().weak());
        if ui_kit::tool(ui, ph::MAGNIFYING_GLASS_PLUS, "Zoom in").clicked() {
            self.zoom_by(1.25);
        }
        if ui_kit::tool(ui, ph::CORNERS_OUT, "Fit the circuit in view").clicked() {
            self.fit_view();
        }
        controls::vline(ui, control_h);
        let single = match self.selection {
            Selection::Comp(id) => Some(id),
            _ => None,
        };
        let any = !self.selection.comp_ids().is_empty();
        if ui_kit::tool_enabled(ui, single.is_some(), ph::ARROW_CLOCKWISE, "Rotate the selected part").clicked()
            && let Some(id) = single
            && let Some(c) = self.project.circuit.components.get_mut(&id)
        {
            c.rotation = (c.rotation + 90) % 360;
            self.topo_rev += 1;
        }
        if ui_kit::tool_enabled(ui, any, ph::MAGIC_WAND, "Auto wire the selection to free board pins").clicked() {
            self.apply_auto_wire();
        }
        if ui_kit::tool_enabled(ui, any, ph::TRASH, "Delete the selection").clicked() {
            for id in self.selection.comp_ids() {
                self.project.circuit.remove_component(id);
            }
            self.selection = Selection::None;
            self.topo_rev += 1;
        }
        if let Some(id) = single {
            controls::vline(ui, control_h);
            let has_script = self.project.circuit.components.get(&id).is_some_and(|c| c.script.is_some());
            if has_script {
                if ui_kit::button(ui, ph::FILE_CODE, "Edit script").clicked() {
                    self.open_tab(Tab::Script(id));
                }
            } else if ui_kit::button(ui, ph::FILE_PLUS, "Attach script").clicked() {
                self.attach_template_script(id);
            }
        }
        if let Some(def_id) = self.canvas.placing.clone() {
            let name = self.lib.component(&def_id).map(|d| d.name.clone()).unwrap_or(def_id);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui_kit::tool(ui, ph::X, "Stop placing (Esc)").clicked() {
                    self.canvas.placing = None;
                }
                ui_kit::status(ui, ph::CURSOR_CLICK, &format!("Placing {name}"), egui_theme_kit::primary(ui));
            });
        }
    }

    /// Zoom around the centre of the canvas.
    fn zoom_by(&mut self, factor: f32) {
        let rect = self.canvas.view_rect;
        if !rect.is_positive() {
            return;
        }
        let new_zoom = (self.canvas.zoom * factor).clamp(0.25, 8.0);
        let factor = new_zoom / self.canvas.zoom;
        let rel = rect.center() - rect.min - self.canvas.offset;
        self.canvas.offset += rel - rel * factor;
        self.canvas.zoom = new_zoom;
    }

    /// Zoom and pan so the board and every part fit the canvas.
    fn fit_view(&mut self) {
        let rect = self.canvas.view_rect;
        if !rect.is_positive() {
            return;
        }
        let mut bounds = Rect::NOTHING;
        if let Some(board) = self.lib.board(&self.project.circuit.board_id) {
            let [a, b] = wirelab_core::geometry::board_world_bounds(board, self.project.circuit.board_pos, self.project.circuit.board_rotation);
            bounds = bounds.union(Rect::from_min_max(Pos2::new(a[0], a[1]), Pos2::new(b[0], b[1])));
        }
        for bb in self.project.circuit.breadboards.values() {
            let Some(p) = self.lib.breadboard(&bb.profile) else { continue };
            let size = if bb.rotation % 180 == 90 { Vec2::new(p.height_mm, p.width_mm) } else { Vec2::new(p.width_mm, p.height_mm) };
            bounds = bounds.union(Rect::from_center_size(Pos2::new(bb.pos[0], bb.pos[1]), size));
        }
        for c in self.project.circuit.components.values() {
            let Some(def) = self.lib.component(&c.def_id) else { continue };
            let half = Vec2::splat(def.visual.width_mm.max(def.visual.height_mm) / 2.0 + 4.0);
            bounds = bounds.union(Rect::from_center_size(Pos2::new(c.pos[0], c.pos[1]), half * 2.0));
        }
        if !bounds.is_positive() {
            return;
        }
        let bounds = bounds.expand(6.0);
        let zoom = ((rect.width() / bounds.width()).min(rect.height() / bounds.height()) / 4.0).clamp(0.25, 8.0);
        self.canvas.zoom = zoom;
        self.canvas.offset = rect.center() - rect.min - bounds.center().to_vec2() * zoom * 4.0;
    }

    /// Body rect of a placed part on screen, rotation included.
    fn body_rect(view: &View, comp: &PlacedComponent, def: &wirelab_core::component::ComponentDef) -> Rect {
        let (w, h) = (view.px(def.visual.width_mm), view.px(def.visual.height_mm));
        let size = if comp.rotation % 180 == 90 { Vec2::new(h, w) } else { Vec2::new(w, h) };
        Rect::from_center_size(view.to_screen(comp.pos), size)
    }

    /// Script badge on a part: state colour, pulsing right after a handler ran.
    fn paint_script_badge(&self, ui: &egui::Ui, view: &View, comp: &PlacedComponent, def: &wirelab_core::component::ComponentDef, time: f64) {
        let scripted = comp.script.is_some() || self.docs.is_dirty(comp.id, &self.project.circuit);
        if !scripted || view.scale < 1.2 || def.visual.shape == wirelab_core::component::Shape::Dot {
            return;
        }
        let errors = self.docs.diags(comp.id).iter().any(|d| d.severity == crate::rhai_lint::Severity::Error);
        let (_, _, state_tone) = self.script_state(ui, comp.id);
        let tone = if errors { ui.visuals().error_fg_color } else { state_tone };
        let tone = if tone == ui.visuals().weak_text_color() { egui_theme_kit::primary(ui) } else { tone };
        let body = Self::body_rect(view, comp, def);
        let center = Pos2::new(body.right() + 2.0, body.top() - 2.0);
        let r = 8.5;
        let painter = ui.painter();
        if let Some((_, at)) = self.live.scripts.last_fired.get(&comp.id) {
            let age = self.live.now_ms().saturating_sub(*at) as f32 / 600.0;
            if age < 1.0 {
                painter.circle_stroke(center, r + 10.0 * age, Stroke::new(2.0, tone.gamma_multiply(1.0 - age)));
                ui.ctx().request_repaint();
            }
        }
        let _ = time;
        painter.circle_filled(center, r, egui_theme_kit::tokens(ui.ctx()).raised);
        painter.circle_stroke(center, r, Stroke::new(1.5, tone));
        ui_kit::kit(ph::FILE_CODE).paint(ui, Rect::from_center_size(center, Vec2::splat(11.0)), 11.0, tone);
    }

    /// Hover card for a part: binding, script state, handlers and last activity.
    fn comp_hover_card(&self, ui: &mut egui::Ui, id: CompId) {
        let Some(comp) = self.project.circuit.components.get(&id) else { return };
        let Some(def) = self.lib.component(&comp.def_id) else { return };
        ui.set_max_width(320.0);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
            crate::icons::paint_part(ui, rect, def);
            ui.label(egui::RichText::new(self.comp_name(id)).strong());
            ui.label(egui::RichText::new(&def.name).weak());
        });
        let weak = ui.visuals().weak_text_color();
        match self.cache.bindings.gpio_of(id) {
            Some(g) => ui_kit::status(ui, ph::PLUGS_CONNECTED, &format!("GPIO{g}"), weak),
            None => ui_kit::status(ui, ph::LINK_BREAK, "Not bound to a GPIO", weak),
        };
        if comp.script.is_none() {
            ui_kit::status(ui, ph::FILE_PLUS, "No script · double-click to add one", weak);
            return;
        }
        let (icon, text, tone) = self.script_state(ui, id);
        ui_kit::status(ui, icon, &format!("Script {}", text.to_lowercase()), tone);
        let handlers: Vec<String> = self.live.scripts.handlers(id).into_iter().filter(|h| h.starts_with("on_")).collect();
        if !handlers.is_empty() {
            ui_kit::status(ui, ph::LIGHTNING, &handlers.join(" · "), weak);
        }
        if let Some((f, at)) = self.live.scripts.last_fired.get(&id) {
            let ago = self.live.now_ms().saturating_sub(*at) as f32 / 1000.0;
            ui_kit::status(ui, ph::CLOCK, &format!("{f} ran {ago:.1} s ago"), weak);
        }
        if let Some(err) = self.live.scripts.errors.get(&id) {
            controls::note(ui, ui_kit::kit(ph::WARNING_CIRCLE), err, ui.visuals().error_fg_color);
        }
        ui.label(egui::RichText::new("Double-click to open the script").small().weak());
    }
}
