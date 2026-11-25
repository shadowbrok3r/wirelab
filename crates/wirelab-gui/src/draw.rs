//! Pure drawing helpers: board, components, wires, terminal geometry.

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use egui::epaint::{EllipseShape, Mesh, RectShape};
use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Stroke, StrokeKind, Vec2};
use wirelab_core::board::{BoardPin, BoardProfile, PinKind, Side};
use wirelab_core::circuit::PlacedComponent;
use wirelab_core::component::{
    ArcPulse, BoostMode, CompState, ComponentDef, Shape, SimModel, TerminalRole, VisualState,
};
use wirelab_proto::PinMode;

/// Short pad label for a terminal, e.g. "+", "NO", "SIG".
pub fn terminal_short_label(role: TerminalRole) -> &'static str {
    match role {
        TerminalRole::Anode => "+",
        TerminalRole::Cathode => "−",
        TerminalRole::A => "A",
        TerminalRole::B => "B",
        TerminalRole::Common => "COM",
        TerminalRole::NormallyOpen => "NO",
        TerminalRole::NormallyClosed => "NC",
        TerminalRole::EndA => "A",
        TerminalRole::EndB => "B",
        TerminalRole::Wiper => "W",
        TerminalRole::Vcc => "V+",
        TerminalRole::Gnd => "G",
        TerminalRole::Signal => "SIG",
        TerminalRole::Positive => "+",
        TerminalRole::Negative => "−",
        TerminalRole::InPos => "IN+",
        TerminalRole::InNeg => "IN−",
        TerminalRole::OutPos => "OUT+",
        TerminalRole::OutNeg => "OUT−",
    }
}

pub const PIN_HIT_RADIUS_PX: f32 = 8.0;

/// World (mm) -> screen transform.
#[derive(Clone, Copy)]
pub struct View {
    pub origin: Pos2,
    pub scale: f32,
}

impl View {
    pub fn to_screen(self, world: [f32; 2]) -> Pos2 {
        Pos2::new(self.origin.x + world[0] * self.scale, self.origin.y + world[1] * self.scale)
    }

    pub fn to_world(self, screen: Pos2) -> [f32; 2] {
        [(screen.x - self.origin.x) / self.scale, (screen.y - self.origin.y) / self.scale]
    }

    pub fn px(self, mm: f32) -> f32 {
        mm * self.scale
    }
}

/// World-space terminal position for a placed component.
pub fn terminal_world_pos(comp: &PlacedComponent, def: &ComponentDef, index: usize) -> [f32; 2] {
    wirelab_core::geometry::terminal_world_pos(comp, def, index)
}

/// Header pin position in world mm for a board turned `rotation` degrees about its centre.
pub fn board_pin_world_pos(board: &BoardProfile, pin: &BoardPin, board_pos: [f32; 2], rotation: u16) -> [f32; 2] {
    wirelab_core::geometry::board_pin_world_pos(board, pin, board_pos, rotation)
}

/// Screen rect of a placed board; quarter turns keep it axis-aligned.
pub fn board_screen_rect(view: &View, board: &BoardProfile, board_pos: [f32; 2], rotation: u16) -> Rect {
    let [a, b] = wirelab_core::geometry::board_world_bounds(board, board_pos, rotation);
    Rect::from_min_max(view.to_screen(a), view.to_screen(b))
}

pub fn pin_state_color(mode: PinMode, high: bool) -> Color32 {
    match mode {
        PinMode::Disabled => Color32::from_gray(120),
        PinMode::Analog => Color32::from_rgb(80, 160, 255),
        PinMode::Pwm => Color32::from_rgb(240, 180, 40),
        m if m.is_input() => {
            if high {
                Color32::from_rgb(90, 220, 120)
            } else {
                Color32::from_rgb(50, 90, 60)
            }
        }
        _ => {
            if high {
                Color32::from_rgb(255, 90, 80)
            } else {
                Color32::from_rgb(110, 60, 55)
            }
        }
    }
}

pub struct PinLive {
    pub mode: PinMode,
    pub high: bool,
    pub millivolts: Option<u16>,
}

/// Clickable / hoverable on-board extras.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardControl {
    Reset,
    Boot,
    RgbLed,
}

/// World-space (mm) rectangles of the board's feature widgets.
pub fn board_feature_rects(
    board: &BoardProfile,
    board_pos: [f32; 2],
    rotation: u16,
) -> Vec<(BoardControl, Rect)> {
    let at = |x_frac: f32, y_from_bottom: f32, w: f32, h: f32| {
        let x0 = board.width_mm * x_frac - w / 2.0;
        let y0 = board.height_mm - y_from_bottom;
        let world = |p: [f32; 2]| {
            let q = wirelab_core::geometry::board_local_to_world(board, board_pos, rotation, p);
            Pos2::new(q[0], q[1])
        };
        Rect::from_two_pos(world([x0, y0]), world([x0 + w, y0 + h]))
    };
    let mut out = Vec::new();
    if board.features.reset_button {
        out.push((BoardControl::Reset, at(0.30, 11.5, 7.0, 4.6)));
    }
    if board.features.boot_button_gpio.is_some() {
        out.push((BoardControl::Boot, at(0.70, 11.5, 7.0, 4.6)));
    }
    if board.features.rgb_led_gpio.is_some() {
        out.push((BoardControl::RgbLed, at(0.22, 18.5, 4.2, 4.2)));
    }
    out
}

/// Draw the board; returns nothing, geometry comes from `board_pin_world_pos`.
#[allow(clippy::too_many_arguments)]
pub fn draw_board(
    painter: &egui::Painter,
    view: &View,
    board: &BoardProfile,
    board_pos: [f32; 2],
    rotation: u16,
    hovered_pin: Option<&str>,
    live: Option<&dyn Fn(u8) -> PinLive>,
    group_keys: &[String],
    group_color: Color32,
    hovered_feature: Option<BoardControl>,
    rgb: Option<[u8; 3]>,
) {
    let rect = board_screen_rect(view, board, board_pos, rotation);
    let (w, h) = (board.width_mm, board.height_mm);
    let l = Local::board(painter, view, board_pos, [w, h], rotation);
    let mask = Color32::from_rgb(18, 74, 48);
    l.shadow([0.0, 0.0, w, h], 1.1, 1.4);
    l.shaded([0.0, 0.0, w, h], 1.1, shade(mask, 1.25), shade(mask, 0.8));
    painter.rect_stroke(
        rect,
        l.corner(1.1),
        Stroke::new(1.5, Color32::from_rgb(10, 40, 26)),
        StrokeKind::Outside,
    );
    painter.rect_stroke(
        rect,
        l.corner(1.1),
        Stroke::new(1.0, Color32::from_white_alpha(16)),
        StrokeKind::Inside,
    );
    board_headers(&l, board);
    board_module(&l, board, w, h);
    board_usb(&l, w, h);
    if view.scale >= 3.0 {
        board_parts(&l, w, h);
    }

    // Boards with a display carry their name above the glass.
    let (name_at, name_align) = if board.features.display.is_some() {
        (Pos2::new(rect.center().x, rect.min.y - 4.0), Align2::CENTER_BOTTOM)
    } else {
        (Pos2::new(rect.center().x, rect.min.y + view.px(board.height_mm * 0.55)), Align2::CENTER_CENTER)
    };
    painter.text(
        name_at,
        name_align,
        &board.name,
        FontId::proportional((view.px(2.2)).clamp(8.0, 16.0)),
        Color32::from_gray(220),
    );

    for pin in &board.pins {
        let pos = view.to_screen(board_pin_world_pos(board, pin, board_pos, rotation));
        let r = view.px(1.1).clamp(2.5, 7.0);
        let (fill, ring) = match pin.kind {
            PinKind::Gnd => (Color32::from_gray(30), Color32::from_gray(140)),
            PinKind::V3_3 => (Color32::from_rgb(180, 40, 40), Color32::from_rgb(255, 140, 120)),
            PinKind::V5 => (Color32::from_rgb(150, 30, 90), Color32::from_rgb(255, 120, 190)),
            PinKind::En | PinKind::Other | PinKind::NotConnected => {
                (Color32::from_gray(90), Color32::from_gray(150))
            }
            PinKind::Gpio(g) => {
                let state = live.map(|f| f(g));
                match state {
                    Some(s) if s.mode != PinMode::Disabled => {
                        (pin_state_color(s.mode, s.high), Color32::from_gray(220))
                    }
                    _ => (Color32::from_rgb(184, 148, 60), Color32::from_rgb(230, 200, 120)),
                }
            }
        };
        painter.circle_filled(pos, r, fill);
        painter.circle_stroke(pos, r, Stroke::new(1.0, ring));
        // Function-group siblings of the hovered pin.
        if group_keys.iter().any(|k| k == &pin.key) {
            painter.circle_stroke(pos, r + 2.5, Stroke::new(1.6, group_color));
        }
        if hovered_pin == Some(pin.key.as_str()) {
            painter.circle_stroke(pos, r + 4.0, Stroke::new(1.5, Color32::WHITE));
        }
        // Millivolt readout next to analog pins.
        if let (PinKind::Gpio(g), Some(f)) = (pin.kind, live) {
            let s = f(g);
            if s.mode == PinMode::Analog
                && let Some(mv) = s.millivolts {
                    let dx = if pin.side == Side::Right { 10.0 } else { -10.0 };
                    let anchor = if pin.side == Side::Right {
                        Align2::LEFT_CENTER
                    } else {
                        Align2::RIGHT_CENTER
                    };
                    painter.text(
                        pos + Vec2::new(dx, 0.0),
                        anchor,
                        format!("{mv} mV"),
                        FontId::monospace(10.0),
                        Color32::from_rgb(120, 190, 255),
                    );
                }
        }
    }

    // On-board extras: RGB LED, RESET / BOOT buttons.
    for (kind, world_rect) in board_feature_rects(board, board_pos, rotation) {
        let r = Rect::from_min_max(
            view.to_screen([world_rect.min.x, world_rect.min.y]),
            view.to_screen([world_rect.max.x, world_rect.max.y]),
        );
        let hovered = hovered_feature == Some(kind);
        match kind {
            BoardControl::Reset | BoardControl::Boot => {
                tact_switch(&l, r, hovered);
                if view.scale > 2.0 {
                    let label = if kind == BoardControl::Reset { "RST" } else { "BOOT" };
                    painter.text(
                        Pos2::new(r.center().x, r.min.y - 2.0),
                        Align2::CENTER_BOTTOM,
                        label,
                        FontId::monospace((view.px(1.4)).clamp(6.0, 10.0)),
                        Color32::from_gray(200),
                    );
                }
            }
            BoardControl::RgbLed => {
                let color = rgb
                    .filter(|c| *c != [0, 0, 0])
                    .map(|c| Color32::from_rgb(c[0], c[1], c[2]));
                rgb_led(&l, r, color);
                if hovered {
                    painter.rect_stroke(
                        r.expand(2.0),
                        CornerRadius::same(2),
                        Stroke::new(1.2, Color32::WHITE),
                        StrokeKind::Outside,
                    );
                }
            }
        }
    }
}

/// Header pin labels, drawn after the display glass so it never hides them.
pub fn draw_board_pin_labels(painter: &egui::Painter, view: &View, board: &BoardProfile, board_pos: [f32; 2], rotation: u16) {
    if view.scale <= 2.0 {
        return;
    }
    let font = FontId::monospace((view.px(1.9)).clamp(7.0, 13.0));
    // Clears both the pin pad and a breadboard solder fillet.
    let r = view.px(1.1).max(2.5);
    for pin in &board.pins {
        let pos = view.to_screen(board_pin_world_pos(board, pin, board_pos, rotation));
        let inward = match pin.side {
            Side::Left => [1.0, 0.0],
            Side::Right => [-1.0, 0.0],
            Side::Top => [0.0, 1.0],
            Side::Bottom => [0.0, -1.0],
        };
        let [ix, iy] = wirelab_core::geometry::rotate(inward, rotation);
        let anchor = match (ix.round() as i32, iy.round() as i32) {
            (1, _) => Align2::LEFT_CENTER,
            (-1, _) => Align2::RIGHT_CENTER,
            (_, 1) => Align2::CENTER_TOP,
            _ => Align2::CENTER_BOTTOM,
        };
        let galley = painter.layout_no_wrap(pin.label.clone(), font.clone(), Color32::from_gray(210));
        let rect = anchor.anchor_size(pos + Vec2::new(ix, iy) * (r + 3.0), galley.size());
        painter.rect_filled(rect.expand(1.0), CornerRadius::same(2), Color32::from_black_alpha(110));
        painter.galley(rect.min, galley, Color32::from_gray(210));
    }
}

/// Black header strips under each row of board pins.
fn board_headers(l: &Local, board: &BoardProfile) {
    for side in [Side::Left, Side::Right, Side::Top, Side::Bottom] {
        let mut pts: Vec<[f32; 2]> = board
            .pins
            .iter()
            .filter(|p| p.side == side)
            .map(|p| wirelab_core::geometry::board_pin_local(board, p))
            .collect();
        if pts.is_empty() {
            continue;
        }
        let vertical = matches!(side, Side::Left | Side::Right);
        let axis = usize::from(vertical);
        pts.sort_by(|a, b| a[axis].total_cmp(&b[axis]));
        let (lo, hi) = (pts[0], pts[pts.len() - 1]);
        let strip = [
            lo[0].min(hi[0]) - 1.27,
            lo[1].min(hi[1]) - 1.27,
            lo[0].max(hi[0]) + 1.27,
            lo[1].max(hi[1]) + 1.27,
        ];
        l.shaded(strip, 0.35, col(46, 46, 50), col(20, 20, 23));
        l.frame(strip, 0.35, 1.0, Color32::from_white_alpha(14));
        if l.scale >= 3.0 {
            for pair in pts.windows(2) {
                let m = (pair[0][axis] + pair[1][axis]) * 0.5;
                let (a, b) = if vertical {
                    ([strip[0] + 0.25, m], [strip[2] - 0.25, m])
                } else {
                    ([m, strip[1] + 0.25], [m, strip[3] - 0.25])
                };
                l.seg(a, b, 1.0, Color32::from_black_alpha(120));
            }
        }
    }
}

/// The ESP module: dark module PCB, meandered antenna, shield can with the chip name.
fn board_module(l: &Local, board: &BoardProfile, w: f32, h: f32) {
    let m = [w * 0.22, 3.0, w * 0.78, 3.0 + h * 0.38];
    l.shadow(m, 0.5, 1.0);
    l.shaded(m, 0.5, col(42, 44, 52), col(24, 25, 30));
    let ant = (m[3] - m[1]) * 0.27;
    if l.scale >= 2.0 {
        let (x0, x1) = (m[0] + 1.0, m[2] - 1.0);
        let (y0, y1) = (m[1] + 0.8, m[1] + ant - 0.7);
        let n = 10;
        let mut pts = Vec::with_capacity(n * 2);
        for i in 0..n {
            let x = x0 + (x1 - x0) * i as f32 / (n - 1) as f32;
            let (a, b) = if i % 2 == 0 { (y1, y0) } else { (y0, y1) };
            pts.push([x, a]);
            pts.push([x, b]);
        }
        l.path(&pts, l.px(0.3).max(1.0), col(196, 156, 76));
    }
    let can = [m[0] + 0.45, m[1] + ant, m[2] - 0.45, m[3] - 0.45];
    l.shaded(can, 0.3, col(228, 230, 234), col(152, 155, 162));
    l.frame(can, 0.3, 1.0, col(110, 113, 120));
    if l.scale >= 5.0 {
        let mut y = can[1] + 0.6;
        while y < can[3] - 0.4 {
            l.seg([can[0] + 0.3, y], [can[2] - 0.3, y], 1.0, Color32::from_white_alpha(12));
            y += 0.5;
        }
    }
    let cy = (can[1] + can[3]) * 0.5;
    l.text([can[0] + 0.6, cy - 1.4, can[2] - 0.6, cy + 1.4], board.chip.name(), 1.7, col(74, 76, 84));
}

/// USB-C receptacle at the bottom edge.
fn board_usb(l: &Local, w: f32, h: f32) {
    let usb = [w * 0.5 - 4.5, h - 4.5, w * 0.5 + 4.5, h + 1.0];
    l.fill([usb[0] - 0.7, h - 3.9, usb[2] + 0.7, h - 2.6], 0.2, TIN);
    l.shadow(usb, 1.2, 0.9);
    l.shaded(usb, 1.2, col(232, 234, 238), col(148, 151, 158));
    l.frame(usb, 1.2, 1.0, col(104, 107, 114));
    if l.scale >= 2.5 {
        l.seg([usb[0] + 0.8, h - 3.2], [usb[2] - 0.8, h - 3.2], 1.0, Color32::from_black_alpha(55));
        l.fill([w * 0.5 - 3.4, h + 0.1, w * 0.5 + 3.4, h + 0.7], 0.3, col(24, 24, 28));
    }
}

/// USB bridge, regulator and passives between the name and the buttons.
fn board_parts(l: &Local, w: f32, h: f32) {
    let y = h * 0.55;
    let x = w * 0.5;
    ic(l, [x, y + 4.6], 3.4, 3.4, 0);
    l.fill(bx([x, y + 8.0], 2.4, 0.9), 0.1, TIN);
    l.shaded(bx([x, y + 9.6], 3.4, 2.2), 0.15, gray(56), gray(24));
    for i in [-1.0f32, 0.0, 1.0] {
        l.fill(bx([x + i * 1.1, y + 11.0], 0.45, 0.8), 0.05, TIN);
    }
    for (dx, dy) in [(-3.0, 3.8), (-3.0, 5.4), (3.0, 3.8), (3.0, 5.4), (-3.0, 9.6), (3.0, 9.6)] {
        smd(l, [x + dx, y + dy], 1.2, 0.65, CAP_TAN);
    }
}

/// Board RESET / BOOT tact switch: metal body, dark round actuator.
fn tact_switch(l: &Local, r: Rect, hovered: bool) {
    let p = l.painter;
    let lift = if hovered { 1.12 } else { 1.0 };
    p.add(
        RectShape::filled(r.translate(l.shadow_offset()), CornerRadius::same(2), Color32::from_black_alpha(90))
            .with_blur_width(l.px(0.8).clamp(2.0, 6.0)),
    );
    let (top, bottom) = (shade(col(206, 208, 213), lift), shade(col(136, 139, 146), lift));
    fill_convex(p, &rrect_outline(r, 2.0), r.center(), &[], l.feather, |q, _| vgrad(r, top, bottom, q));
    p.rect_stroke(
        r,
        CornerRadius::same(2),
        Stroke::new(1.0, Color32::from_gray(if hovered { 230 } else { 92 })),
        StrokeKind::Inside,
    );
    let cap = if hovered { 70 } else { 44 };
    let cr = (r.height() * 0.3).clamp(1.5, 6.0);
    dome_px(p, l.feather, r.center(), cr, gray(cap + 40), gray(cap), gray(cap / 2));
}

/// Board RGB LED: white 5050 package with a round lens that takes the live colour.
fn rgb_led(l: &Local, r: Rect, color: Option<Color32>) {
    let p = l.painter;
    // Full-brightness hue for the lens; the raw intensity scales the glow.
    let color = color.map(|c| {
        let peak = c.r().max(c.g()).max(c.b()).max(1);
        let s = |v: u8| (u16::from(v) * 255 / u16::from(peak)) as u8;
        (Color32::from_rgb(s(c.r()), s(c.g()), s(c.b())), (f32::from(peak) / 255.0).sqrt())
    });
    if let Some((c, k)) = color {
        glow_px(p, r.center(), r.width() * (1.1 + 0.8 * k), c, 0.5 + 0.5 * k);
    }
    let color = color.map(|(c, _)| c);
    p.rect_filled(r, CornerRadius::same(1), Color32::from_gray(232));
    p.rect_stroke(r, CornerRadius::same(1), Stroke::new(1.0, Color32::from_gray(150)), StrokeKind::Inside);
    if r.width() > 10.0 {
        let k = r.width() * 0.22;
        p.add(egui::Shape::convex_polygon(
            vec![r.left_top(), r.left_top() + Vec2::new(k, 0.0), r.left_top() + Vec2::new(0.0, k)],
            Color32::from_gray(140),
            Stroke::NONE,
        ));
    }
    let lens = r.width() * 0.32;
    match color {
        Some(c) => dome_px(p, l.feather, r.center(), lens, mix(c, Color32::WHITE, 0.75), c, shade(c, 0.7)),
        None => dome_px(p, l.feather, r.center(), lens, gray(214), gray(178), gray(146)),
    }
}

/// Draw one placed component; `vis` animates it when live and `active`
/// outlines it while it is doing something in the simulated circuit.
#[allow(clippy::too_many_arguments)]
pub fn draw_component(
    painter: &egui::Painter,
    view: &View,
    comp: &PlacedComponent,
    def: &ComponentDef,
    vis: Option<VisualState>,
    selected: bool,
    accent: Color32,
    active: Option<Color32>,
    time: f64,
) {
    let center = view.to_screen(comp.pos);
    let (w, h) = (def.visual.width_mm, def.visual.height_mm);
    let size = if comp.rotation % 180 == 90 {
        Vec2::new(view.px(h), view.px(w))
    } else {
        Vec2::new(view.px(w), view.px(h))
    };
    let body = Rect::from_center_size(center, size);
    if !painter.clip_rect().intersects(body.expand(view.px(9.0).max(140.0))) {
        return;
    }
    let base = Color32::from_rgb(def.visual.color[0], def.visual.color[1], def.visual.color[2]);
    let on = match vis {
        Some(VisualState::LedBrightness(b)) => b > 0.01,
        Some(VisualState::RelayClosed(c)) => c,
        Some(VisualState::BuzzerOn { .. }) => true,
        Some(VisualState::Capacitor { volts, .. }) => volts.abs() > 36.0,
        Some(VisualState::Boost { mode, .. }) => mode != BoostMode::Off,
        Some(VisualState::Supply { amps, .. }) => amps > 0.01,
        Some(VisualState::Diode { conducting, .. }) => conducting,
        Some(VisualState::Arc { amps, since_pulse_s, .. }) => amps > 0.5 || since_pulse_s < 0.3,
        _ => false,
    } || matches!(
        comp.state,
        CompState::Button { pressed: true } | CompState::Toggle { on: true }
    );

    // Junction dots are just a node: no body, pads or name.
    if def.visual.shape == Shape::Dot {
        outlines(painter, body, selected, accent, active.filter(|_| on));
        let r = (size.x.min(size.y) * 0.5).clamp(3.0, 7.0);
        painter.circle_filled(center, r, base);
        painter.circle_stroke(center, r, Stroke::new(1.2, base.linear_multiply(1.6)));
        painter.circle_filled(center + Vec2::new(-0.3, -0.35) * r, r * 0.32, Color32::from_white_alpha(70));
        if !comp.label.is_empty() && view.scale > 1.6 {
            painter.text(
                Pos2::new(center.x, body.min.y - 5.0),
                Align2::CENTER_BOTTOM,
                &comp.label,
                FontId::proportional((view.px(1.8)).clamp(8.0, 13.0)),
                Color32::from_gray(200),
            );
        }
        return;
    }

    let pads = wirelab_core::geometry::instance_offsets(comp, def);
    // Bent two-lead parts draw their leads to the chosen span.
    let w = if comp.props.contains_key(wirelab_core::geometry::SPAN_PROP) && pads.len() == 2 {
        (pads[1][0] - pads[0][0]).abs().max(w.min(h))
    } else {
        w
    };
    let part = Part { l: Local::new(painter, view, comp.pos, comp.rotation), w, h, base, pads };
    let toggled = matches!(comp.state, CompState::Toggle { on: true });
    let pressed = matches!(comp.state, CompState::Button { pressed: true });
    let level = match comp.state {
        CompState::Fraction { value } => value.clamp(0.0, 1.0),
        _ => 0.5,
    };
    let mut art_top = f32::INFINITY;
    match art_of(def) {
        Art::Led => {
            let lit = match vis {
                Some(VisualState::LedBrightness(b)) => b.clamp(0.0, 1.0),
                _ => 0.0,
            };
            led(&part, lit);
        }
        Art::Resistor => {
            let ohms = comp.props.get("ohms").copied().unwrap_or(match def.sim {
                SimModel::Resistor { ohms } => f64::from(ohms),
                _ => 1000.0,
            }) as f32;
            resistor(&part, ohms);
        }
        Art::Button => button(&part, pressed),
        Art::Toggle => toggle(&part, toggled),
        Art::Slide => slide(&part, toggled),
        Art::Reed => reed(&part, pressed),
        Art::Tilt => tilt(&part, pressed),
        Art::Ldr => ldr(&part, level),
        Art::Pot => pot(&part, level),
        Art::Buzzer { active: drive } => {
            buzzer(&part, drive, matches!(vis, Some(VisualState::BuzzerOn { .. })), time);
        }
        Art::Servo => {
            let angle = match vis {
                Some(VisualState::ServoAngle(a)) => a,
                _ => 90.0,
            };
            art_top = servo(&part, angle);
        }
        Art::Relay => relay(&part, matches!(vis, Some(VisualState::RelayClosed(true)))),
        Art::Dht => dht(&part, toggled),
        Art::Ultrasonic => ultrasonic(&part, toggled, time),
        Art::Pir => pir(&part, toggled),
        Art::Soil => soil(&part, level),
        Art::Touch => touch(&part, toggled),
        Art::Rc522 => rc522(&part),
        Art::Rdm6300 => rdm6300(&part, toggled),
        Art::Tft => tft(&part),
        Art::AnalogSensor => analog_sensor(&part, level),
        Art::Chip => chip(&part),
        Art::Module => module(&part, toggled),
        Art::Capacitor => {
            let (volts, rated) = match vis {
                Some(VisualState::Capacitor { volts, rated }) => (volts, rated),
                _ => (0.0, match def.sim {
                    SimModel::Capacitor { v_rated, .. } => v_rated,
                    _ => 1.0,
                }),
            };
            capacitor(&part, volts, rated, time);
        }
        Art::Diode => diode(&part, matches!(vis, Some(VisualState::Diode { conducting: true, .. }))),
        Art::Supply => {
            let (v, a) = match vis {
                Some(VisualState::Supply { volts, amps }) => (Some(volts), amps),
                _ => (None, 0.0),
            };
            supply(&part, v, a);
        }
        Art::Boost => {
            let st = match vis {
                Some(VisualState::Boost { mode, volts, amps }) => Some((mode, volts, amps)),
                _ => None,
            };
            boost(&part, st, time);
        }
        Art::Pen => {
            let st = match vis {
                Some(VisualState::Arc { amps, pulse, since_pulse_s }) => Some((amps, pulse, since_pulse_s)),
                _ => None,
            };
            pen(&part, pressed, st, time);
        }
        Art::Clamp => clamp(&part),
    }
    outlines(painter, body, selected, accent, active.filter(|_| on));

    // Terminal pads with role labels placed outward from the body.
    let label_font = FontId::monospace((view.px(1.6)).clamp(7.0, 11.0));
    let mut name_y = (body.min.y - 5.0).min(art_top - 4.0);
    for (i, t) in def.terminals.iter().enumerate() {
        let p = view.to_screen(terminal_world_pos(comp, def, i));
        let r = view.px(0.9).clamp(2.5, 6.0);
        pad(painter, p, r);
        // Outward normal of the body side the pad sits on, in screen space.
        let [ox, oy] = part.pads.get(i).copied().unwrap_or([0.0, 0.0]);
        let side = if w * 0.5 - ox.abs() <= h * 0.5 - oy.abs() {
            [ox.signum(), 0.0]
        } else {
            [0.0, oy.signum()]
        };
        let out = part.l.dir(side);
        let on_top = out.y < -0.5;
        if on_top {
            name_y = name_y.min(p.y - r - 4.0);
        }
        // Non-polar leads of through-hole parts need no label.
        let unlabelled = !def.leads.is_empty() && matches!(t.role, TerminalRole::A | TerminalRole::B);
        if view.scale > 2.2 && !unlabelled {
            let gap = r + view.px(0.5).clamp(2.0, 6.0);
            let anchor = if out.x > 0.5 {
                Align2::LEFT_CENTER
            } else if out.x < -0.5 {
                Align2::RIGHT_CENTER
            } else if out.y > 0.5 {
                Align2::CENTER_TOP
            } else {
                Align2::CENTER_BOTTOM
            };
            let off = out * gap;
            // Bus-heavy modules (SPI displays, readers) label by pin id.
            let label = if def.terminals.len() > 3 {
                t.id.to_uppercase()
            } else {
                terminal_short_label(t.role).to_string()
            };
            let rect = painter.text(p + off, anchor, label, label_font.clone(), Color32::from_gray(185));
            if on_top {
                name_y = name_y.min(rect.min.y - 4.0);
            }
        }
    }

    // Name label above the body, clear of terminal pads and their labels.
    let label = if comp.label.is_empty() { &def.name } else { &comp.label };
    if view.scale > 1.6 {
        painter.text(
            Pos2::new(center.x, name_y),
            Align2::CENTER_BOTTOM,
            label,
            FontId::proportional((view.px(1.8)).clamp(8.0, 13.0)),
            Color32::from_gray(200),
        );
    }
}

/// Selection accent and live-activity glow around the (rotated) body.
fn outlines(painter: &egui::Painter, body: Rect, selected: bool, accent: Color32, glow: Option<Color32>) {
    if selected {
        painter.rect_stroke(body.expand(4.0), CornerRadius::same(4), Stroke::new(1.5, accent), StrokeKind::Outside);
    }
    if let Some(glow) = glow {
        painter.rect_stroke(body.expand(2.0), CornerRadius::same(3), Stroke::new(1.5, glow), StrokeKind::Outside);
    }
}

/// Tinned terminal pad with a dark pin hole.
fn pad(painter: &egui::Painter, p: Pos2, r: f32) {
    painter.circle_filled(p, r, Color32::from_rgb(208, 210, 214));
    painter.circle_filled(p, r * 0.42, Color32::from_rgb(92, 94, 100));
    painter.circle_stroke(p, r, Stroke::new(1.0, Color32::from_gray(70)));
}

// ------------------------------------------------------------------ part art --

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Art {
    Led,
    Resistor,
    Button,
    Toggle,
    Slide,
    Reed,
    Tilt,
    Ldr,
    Pot,
    Buzzer { active: bool },
    Servo,
    Relay,
    Dht,
    Ultrasonic,
    Pir,
    Soil,
    Touch,
    Rc522,
    Rdm6300,
    Tft,
    AnalogSensor,
    Chip,
    Module,
    Capacitor,
    Diode,
    Supply,
    Boost,
    Pen,
    Clamp,
}

/// Id keywords that pick the art before the drawn shape does (same table as the palette icons).
const ART_BY_ID: [(&str, Art); 22] = [
    ("capacitor", Art::Capacitor),
    ("diode", Art::Diode),
    ("dc-supply", Art::Supply),
    ("hv-boost", Art::Boost),
    ("weld-pen", Art::Pen),
    ("weld-clamp", Art::Clamp),
    ("dht", Art::Dht),
    ("hc-sr04", Art::Ultrasonic),
    ("ultrasonic", Art::Ultrasonic),
    ("pir", Art::Pir),
    ("soil", Art::Soil),
    ("touch", Art::Touch),
    ("rc522", Art::Rc522),
    ("rdm6300", Art::Rdm6300),
    ("st7735", Art::Tft),
    ("tft", Art::Tft),
    ("oled", Art::Tft),
    ("lcd", Art::Tft),
    ("reed", Art::Reed),
    ("tilt", Art::Tilt),
    ("analog-sensor", Art::AnalogSensor),
    ("relay", Art::Relay),
];

fn art_of(def: &ComponentDef) -> Art {
    if let Some((_, art)) = ART_BY_ID.iter().find(|(key, _)| def.id.contains(key)) {
        return *art;
    }
    match def.visual.shape {
        Shape::Led => Art::Led,
        Shape::PushButton => Art::Button,
        Shape::ToggleSwitch => Art::Toggle,
        Shape::SlideSwitch => Art::Slide,
        Shape::Resistor => Art::Resistor,
        Shape::Potentiometer => Art::Pot,
        Shape::Photoresistor => Art::Ldr,
        Shape::Buzzer => Art::Buzzer { active: !matches!(def.sim, SimModel::Buzzer { active: false }) },
        Shape::Servo => Art::Servo,
        Shape::Relay => Art::Relay,
        Shape::Chip => Art::Chip,
        Shape::SensorModule | Shape::Generic | Shape::Dot => Art::Module,
    }
}

/// A part being drawn: its local painter, body size in mm, body colour and local pad offsets.
struct Part<'a> {
    l: Local<'a>,
    w: f32,
    h: f32,
    base: Color32,
    pads: Vec<[f32; 2]>,
}

impl Part<'_> {
    fn detail(&self) -> f32 {
        self.l.scale
    }

    fn headers(&self, keep: impl Fn([f32; 2]) -> bool) {
        for &at in self.pads.iter().filter(|p| keep(**p)) {
            header(&self.l, at);
        }
    }
}

/// 5 mm LED: leads, cathode-flat flange, domed lens with die cup and highlight; glows when lit.
fn led(p: &Part, lit: f32) {
    let l = &p.l;
    let (w, h, base) = (p.w, p.h, p.base);
    let rd = w.min(h) * 0.4;
    let rf = rd * 1.17;
    let flat = rf * 0.84;
    l.lead([-w * 0.5, 0.0], [-rf * 0.8, 0.0], 0.5);
    l.lead([w * 0.5, 0.0], [flat * 0.9, 0.0], 0.5);
    l.disc_shadow([0.0, 0.0], rf, 0.9);
    if lit > 0.01 {
        l.glow([0.0, 0.0], rf * (2.3 + 1.9 * lit), base, 0.8 * lit);
    }
    let flange: Vec<[f32; 2]> = (0..36)
        .map(|i| {
            let a = i as f32 / 36.0 * TAU;
            [(rf * a.cos()).min(flat), rf * a.sin()]
        })
        .collect();
    l.poly_shaded(
        &flange,
        mix(shade(base, 0.58), shade(base, 0.95), lit),
        mix(shade(base, 0.3), shade(base, 0.62), lit),
    );
    l.poly_outline(&flange, 1.0, Color32::from_black_alpha(70));
    let hub = mix(mix(shade(base, 0.9), Color32::WHITE, 0.24), mix(base, Color32::WHITE, 0.88), lit);
    let mid = mix(shade(base, 0.68), mix(base, Color32::WHITE, 0.32), lit);
    let rim = mix(shade(base, 0.38), shade(base, 0.86), lit);
    l.dome([0.0, 0.0], rd, hub, mid, rim);
    if p.detail() >= 3.0 {
        let cup = mix(shade(base, 0.3), mix(base, Color32::WHITE, 0.92), lit).gamma_multiply(0.75);
        l.fill(bx([rd * 0.12, 0.0], rd * 0.42, rd * 0.42), 0.12, cup);
        l.seg([-rd * 0.5, 0.0], [0.0, 0.0], l.px(0.08).max(1.0), Color32::from_black_alpha(60));
    }
    if lit > 0.05 {
        l.glow([0.0, 0.0], rd * 0.95, Color32::WHITE, 0.55 * lit);
    }
    l.specular([0.0, 0.0], rd, 150);
}

/// Axial resistor: leads, dog-bone body with bulged end caps, 4-band colour code, cylinder shading.
fn resistor(p: &Part, ohms: f32) {
    let l = &p.l;
    let (w, h, base) = (p.w, p.h, p.base);
    let half = w * 0.5;
    let body = (w * 0.35).min(half - 0.4);
    let re = h * 0.42;
    let rm = h * 0.32;
    let bulb = body * 0.34;
    let rr = re * 0.55;
    l.lead([-half, 0.0], [half, 0.0], 0.5);
    l.shadow([-body, -re, body, re], rr, 0.8);
    l.fill([-body + bulb * 0.5, -rm, body - bulb * 0.5, rm], 0.0, base);
    let ends = [(-body, -body + bulb), (body - bulb, body)];
    for (x0, x1) in ends {
        l.fill([x0, -re, x1, re], rr, base);
    }
    if let Some(bands) = resistor_bands(ohms) {
        let bw = (w * 0.06).max(0.3);
        let profile = |d: f32| {
            if d >= rr {
                re
            } else {
                (re - rr) + (rr * rr - (rr - d).powi(2)).max(0.0).sqrt()
            }
        };
        let xs = [-body + bulb * 0.5, -body * 0.46, -body * 0.16, body - bulb * 0.5];
        for (i, (x, c)) in xs.into_iter().zip(bands).enumerate() {
            let hh = if i == 0 || i == 3 { profile(body - x.abs() - bw * 0.5) - 0.03 } else { rm };
            l.fill([x - bw * 0.5, -hh, x + bw * 0.5, hh], 0.0, c);
        }
    }
    let s = l.lit_y();
    let hl = Color32::from_white_alpha(64);
    let sh = Color32::from_black_alpha(64);
    l.fill([-body + bulb, s * rm * 0.78, body - bulb, s * rm * 0.3], 0.0, hl);
    l.fill([-body + bulb, -s * rm * 0.38, body - bulb, -s * rm], 0.0, sh);
    for (x0, x1) in ends {
        l.fill([x0 + 0.08, s * re * 0.82, x1 - 0.08, s * re * 0.34], rr * 0.5, hl);
        l.fill([x0 + 0.04, -s * re * 0.42, x1 - 0.04, -s * (re - 0.04)], rr * 0.8, sh);
    }
}

/// 6 mm tactile switch: legs, black base, metal plate with clips, round cap that sinks when pressed.
fn button(p: &Part, pressed: bool) {
    let l = &p.l;
    let s = p.w.min(p.h) * 0.38;
    for sx in [-1.0f32, 1.0] {
        l.fill([sx * (s - 0.3), -0.6, sx * p.w * 0.5, 0.6], 0.15, TIN);
    }
    l.shadow([-s, -s, s, s], 0.5, 1.0);
    l.shaded([-s, -s, s, s], 0.55, gray(56), gray(24));
    let m = s * 0.86;
    l.shaded([-m, -m, m, m], 0.35, col(224, 226, 230), col(144, 148, 155));
    l.frame([-m, -m, m, m], 0.35, 1.0, Color32::from_black_alpha(70));
    if p.detail() >= 3.0 {
        for (dx, dy) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            l.disc([dx * (m - 0.62), dy * (m - 0.62)], 0.26, col(116, 119, 126));
        }
        for sy in [-1.0f32, 1.0] {
            l.fill([-0.9, sy * s, 0.9, sy * (m - 0.25)], 0.0, col(176, 179, 185));
        }
    }
    let rc = s * 0.56;
    l.disc([0.0, 0.0], rc + 0.34, gray(34));
    let cap = p.base;
    if pressed {
        l.dome([0.0, 0.0], rc * 0.9, shade(cap, 0.5), shade(cap, 0.72), shade(cap, 0.98));
    } else {
        l.dome([0.0, 0.0], rc, mix(cap, Color32::WHITE, 0.38), cap, shade(cap, 0.5));
        l.specular([0.0, 0.0], rc, 60);
    }
}

/// Bat-handle toggle: housing, hex nut, threaded bushing and a chrome lever thrown up (on) or down.
fn toggle(p: &Part, on: bool) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let bw = w * 0.5 - 0.4;
    let bh = h * 0.5 - 0.3;
    for sx in [-1.0f32, 1.0] {
        l.fill([sx * (bw - 0.3), -0.7, sx * w * 0.5, 0.7], 0.2, TIN);
    }
    l.shadow([-bw, -bh, bw, bh], 0.8, 1.0);
    l.shaded([-bw, -bh, bw, bh], 0.8, shade(p.base, 1.2), shade(p.base, 0.7));
    l.frame([-bw, -bh, bw, bh], 0.8, 1.0, Color32::from_white_alpha(24));
    let rn = bw * 0.86;
    let hex: Vec<[f32; 2]> = (0..6)
        .map(|i| {
            let a = (i as f32 * 60.0 + 30.0).to_radians();
            [rn * a.cos(), rn * a.sin()]
        })
        .collect();
    l.poly_shadow(&hex, 0.8);
    l.poly_shaded(&hex, CHROME_HI, CHROME_LO);
    l.poly_outline(&hex, 1.0, Color32::from_black_alpha(60));
    let rb = rn * 0.7;
    l.dome([0.0, 0.0], rb, CHROME_HI, col(186, 190, 196), col(118, 122, 128));
    if p.detail() >= 4.0 {
        for k in 1..=3 {
            l.ring([0.0, 0.0], rb * (0.6 + 0.11 * k as f32), 1.0, Color32::from_black_alpha(40));
        }
    }
    l.disc([0.0, 0.0], rb * 0.5, gray(22));
    let d = if on { -1.0 } else { 1.0 };
    let len = bh - 0.35;
    let lever = [[-0.62, 0.0], [0.62, 0.0], [0.95, d * len], [-0.95, d * len]];
    l.poly_shadow(&lever, 0.9);
    l.poly_shaded(&lever, CHROME_HI, col(146, 150, 156));
    l.dome([0.0, d * len], 0.95, CHROME_HI, col(196, 200, 206), col(126, 130, 136));
    let sx = l.lit_x();
    l.seg([sx * 0.32, d * 0.5], [sx * 0.52, d * (len - 0.5)], l.px(0.2).max(1.0), Color32::from_white_alpha(130));
    l.dome([0.0, 0.0], 0.62, col(150, 154, 160), col(104, 108, 114), col(70, 72, 78));
}

/// SPDT slide switch: three pins, tin housing, dark slot and a coloured ribbed actuator.
fn slide(p: &Part, on: bool) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let hx = w * 0.5 - 0.7;
    let hy = h * 0.5 - 0.55;
    for sx in [-1.0f32, 1.0] {
        l.fill([sx * (hx - 0.3), -0.42, sx * w * 0.5, 0.42], 0.12, TIN);
    }
    l.fill([-0.42, hy - 0.3, 0.42, h * 0.5], 0.12, TIN);
    l.shadow([-hx, -hy, hx, hy], 0.3, 1.0);
    l.shaded([-hx, -hy, hx, hy], 0.3, col(216, 218, 222), col(136, 139, 146));
    l.frame([-hx + 0.25, -hy + 0.25, hx - 0.25, hy - 0.25], 0.2, 1.0, Color32::from_black_alpha(45));
    let sl = hx * 0.6;
    l.fill([-sl, -0.72, sl, 0.72], 0.25, gray(16));
    let kw = 1.25;
    let cx = if on { sl - kw } else { -sl + kw };
    let knob = [cx - kw, -1.2, cx + kw, 1.2];
    l.shadow(knob, 0.35, 0.8);
    l.shaded(knob, 0.35, shade(p.base, 1.22), shade(p.base, 0.68));
    l.frame(knob, 0.35, 1.0, Color32::from_black_alpha(50));
    if p.detail() >= 4.0 {
        for k in -1..=1 {
            let x = cx + k as f32 * 0.6;
            l.seg([x, -0.85], [x, 0.85], l.px(0.16).max(1.0), Color32::from_black_alpha(70));
        }
    }
}

/// Reed switch: glass capsule with two overlapping blades that touch when closed.
fn reed(p: &Part, closed: bool) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let gx = w * 0.5 - 2.0;
    let gy = h * 0.5 - 0.35;
    l.lead([-w * 0.5, 0.0], [-gx + 0.4, 0.0], 0.45);
    l.lead([w * 0.5, 0.0], [gx - 0.4, 0.0], 0.45);
    l.shadow([-gx, -gy, gx, gy], gy, 0.45);
    let blade = col(150, 152, 146);
    let (ya, yb) = if closed { (-0.04, 0.04) } else { (-0.21, 0.21) };
    let t = 0.17;
    l.fill([-gx + 0.4, ya - t, 1.0, ya + t], 0.05, blade);
    l.fill([-1.0, yb - t, gx - 0.4, yb + t], 0.05, blade);
    if p.detail() >= 4.0 {
        let tip = col(206, 176, 96);
        l.fill([0.2, ya - t, 1.0, ya + t], 0.05, tip);
        l.fill([-1.0, yb - t, -0.2, yb + t], 0.05, tip);
    }
    let glass = mix(p.base, Color32::WHITE, 0.25);
    l.fill([-gx, -gy, gx, gy], gy, glass.gamma_multiply(0.28));
    for sx in [-1.0f32, 1.0] {
        l.fill([sx * gx, -gy * 0.8, sx * (gx - 1.1), gy * 0.8], gy * 0.8, glass.gamma_multiply(0.22));
    }
    l.frame([-gx, -gy, gx, gy], gy, 1.0, Color32::from_white_alpha(70));
    let s = l.lit_y();
    l.fill([-gx + gy, s * gy * 0.72, gx - gy, s * gy * 0.38], 0.2, Color32::from_white_alpha(90));
    if closed {
        l.glow([0.0, 0.0], 0.9, col(255, 230, 150), 0.6);
    }
}

/// Ball tilt switch: brass can with rolled rim and epoxy base; the ball sits on the contacts when closed.
fn tilt(p: &Part, closed: bool) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let cx = w * 0.5 - 0.5;
    let top = -h * 0.5 + 0.3;
    let bot = h * 0.5 - 1.5;
    for sx in [-1.0f32, 1.0] {
        l.fill([sx * (cx - 0.2), -0.28, sx * w * 0.5, 0.28], 0.1, TIN);
    }
    l.fill([-cx * 0.85, bot - 0.3, cx * 0.85, h * 0.5 - 0.2], 0.3, gray(30));
    l.shadow([-cx, top, cx, bot], 0.7, 1.0);
    l.fill([-cx, top, cx, bot], 0.7, p.base);
    let s = l.lit_x();
    l.fill([s * cx * 0.78, top + 0.25, s * cx * 0.26, bot - 0.25], 0.3, Color32::from_white_alpha(80));
    l.fill([-s * cx * 0.35, top + 0.2, -s * (cx - 0.08), bot - 0.2], 0.4, Color32::from_black_alpha(70));
    l.fill([-cx, top, cx, top + 0.9], 0.6, shade(p.base, 0.8));
    l.seg([-cx, top + 1.7], [cx, top + 1.7], l.px(0.18).max(1.0), Color32::from_black_alpha(80));
    if p.detail() >= 4.0 {
        let y = if closed { bot - 1.5 } else { top + 2.9 };
        l.ring([0.0, y], 1.15, 1.0, Color32::from_white_alpha(60));
    }
}

/// LDR: ceramic disc with flats, serpentine CdS track and glossy coating; brightens with light.
fn ldr(p: &Part, level: f32) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let r = (w * 0.47).min(h * 0.62);
    let flat = h * 0.5 - 0.08;
    l.lead([-w * 0.5, 0.0], [-r + 0.3, 0.0], 0.45);
    l.lead([w * 0.5, 0.0], [r - 0.3, 0.0], 0.45);
    let disc: Vec<[f32; 2]> = (0..40)
        .map(|i| {
            let a = i as f32 / 40.0 * TAU;
            [r * a.cos(), (r * a.sin()).clamp(-flat, flat)]
        })
        .collect();
    l.disc_shadow([0.0, 0.0], r, 0.8);
    let face = mix(p.base, Color32::WHITE, 0.62);
    let k = 0.8 + 0.25 * level;
    l.poly_shaded(&disc, shade(face, k * 1.04), shade(face, k * 0.84));
    l.poly_outline(&disc, 1.0, shade(p.base, 0.5));
    let track = shade(p.base, 0.6);
    if p.detail() >= 2.5 {
        let n = 6;
        let span = r * 0.68;
        let mut pts = Vec::with_capacity(n * 2);
        for i in 0..n {
            let x = -span + 2.0 * span * i as f32 / (n - 1) as f32;
            let y = ((r - 0.45).powi(2) - x * x).max(0.0).sqrt().min(flat - 0.35);
            let (a, b) = if i % 2 == 0 { (-y, y) } else { (y, -y) };
            pts.push([x, a]);
            pts.push([x, b]);
        }
        l.path(&pts, l.px(0.3).max(1.0), track);
    } else {
        l.seg([-r * 0.6, 0.0], [r * 0.6, 0.0], 1.0, track);
    }
    l.specular([0.0, 0.0], r, 110);
}

/// Rotary pot: solder lugs, steel housing with scale ticks, knurled knob and pointer.
fn pot(p: &Part, frac: f32) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let rh = w.min(h) * 0.5 - 1.0;
    let lugs = [
        ([-w * 0.5, -0.8, -rh + 0.4, 0.8], [-w * 0.5 + 0.9, 0.0]),
        ([rh - 0.4, -0.8, w * 0.5, 0.8], [w * 0.5 - 0.9, 0.0]),
        ([-0.8, rh - 0.4, 0.8, h * 0.5], [0.0, h * 0.5 - 0.9]),
    ];
    for (b, eye) in lugs {
        l.fill(b, 0.3, TIN);
        if p.detail() >= 3.0 {
            l.disc(eye, 0.32, gray(64));
        }
    }
    l.fill([-0.7, -rh - 0.55, 0.7, -rh + 0.5], 0.2, col(120, 124, 130));
    l.disc_shadow([0.0, 0.0], rh, 1.0);
    l.dome([0.0, 0.0], rh, col(112, 116, 123), col(70, 73, 79), col(40, 42, 46));
    l.ring([0.0, 0.0], rh, 1.0, Color32::from_white_alpha(25));
    let rk = rh * 0.74;
    if p.detail() >= 3.5 {
        for i in 0..=10 {
            let a = (-135.0 + 27.0 * i as f32).to_radians();
            let d = [a.sin(), -a.cos()];
            let alpha = if i % 5 == 0 { 170 } else { 90 };
            l.seg(
                [d[0] * rk * 1.1, d[1] * rk * 1.1],
                [d[0] * rh * 0.94, d[1] * rh * 0.94],
                l.px(0.18).max(1.0),
                Color32::from_white_alpha(alpha),
            );
        }
    }
    l.disc_shadow([0.0, 0.0], rk, 0.7);
    l.dome([0.0, 0.0], rk, mix(p.base, Color32::WHITE, 0.4), p.base, shade(p.base, 0.55));
    if p.detail() >= 3.0 {
        let n = 32;
        for i in 0..n {
            let a = i as f32 / n as f32 * TAU;
            let d = [a.cos(), a.sin()];
            l.seg(
                [d[0] * rk * 0.84, d[1] * rk * 0.84],
                [d[0] * rk * 0.99, d[1] * rk * 0.99],
                l.px(0.2).max(1.0),
                Color32::from_black_alpha(70),
            );
        }
    }
    l.dome([0.0, 0.0], rk * 0.7, mix(p.base, Color32::WHITE, 0.22), shade(p.base, 0.95), shade(p.base, 0.82));
    l.ring([0.0, 0.0], rk * 0.7, 1.0, Color32::from_black_alpha(50));
    let a = (-135.0 + 270.0 * frac.clamp(0.0, 1.0)).to_radians();
    let d = [a.sin(), -a.cos()];
    l.seg(
        [d[0] * rk * 0.12, d[1] * rk * 0.12],
        [d[0] * rk * 0.8, d[1] * rk * 0.8],
        l.px(0.5).max(2.0),
        Color32::WHITE,
    );
    l.specular([0.0, 0.0], rk, 50);
}

/// 12 mm buzzer: black can with sound port; passive shows its green PCB rim and brass piezo.
fn buzzer(p: &Part, active: bool, on: bool, time: f64) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let r = w.min(h) * 0.5 - 0.35;
    for sx in [-1.0f32, 1.0] {
        l.fill([sx * (r - 0.4), -0.35, sx * w * 0.5, 0.35], 0.1, TIN);
    }
    l.disc_shadow([0.0, 0.0], r, 1.0);
    let body_r = if active { r } else { r - 0.45 };
    if !active {
        l.dome([0.0, 0.0], r, col(52, 150, 92), col(30, 112, 64), col(18, 78, 44));
    }
    l.dome([0.0, 0.0], body_r, mix(p.base, Color32::WHITE, 0.2), p.base, shade(p.base, 0.55));
    l.ring([0.0, 0.0], body_r, 1.0, Color32::from_white_alpha(30));
    l.ring([0.0, 0.0], body_r * 0.8, l.px(0.16).max(1.0), Color32::from_white_alpha(16));
    l.ring([0.0, 0.0], body_r * 0.77, l.px(0.16).max(1.0), Color32::from_black_alpha(90));
    if active {
        l.disc([0.0, 0.0], body_r * 0.17, gray(6));
        l.ring([0.0, 0.0], body_r * 0.17, 1.0, Color32::from_white_alpha(30));
        l.text(bx([-body_r * 0.5, -body_r * 0.5], 2.4, 2.4), "+", 2.2, gray(150));
    } else {
        l.disc([0.0, 0.0], body_r * 0.27, gray(8));
        l.dome([0.0, 0.0], body_r * 0.14, col(214, 186, 112), col(170, 140, 74), col(120, 96, 46));
    }
    l.specular([0.0, 0.0], body_r, 30);
    if on {
        let wave = col(255, 214, 90);
        for k in 0..3 {
            let ph = ((time * 1.8).fract() as f32 + k as f32 / 3.0).fract();
            l.ring(
                [0.0, 0.0],
                r * (1.08 + 0.75 * ph),
                l.px(0.3).clamp(1.2, 3.0),
                wave.gamma_multiply(0.8 * (1.0 - ph)),
            );
        }
    }
}

/// SG90 servo with a white single-arm horn at `angle`; returns the horn's top edge in screen y.
fn servo(p: &Part, angle: f32) -> f32 {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let bxh = w * 0.5 - 3.7;
    let byh = h * 0.5 - 1.1;
    let tx = w * 0.5 - 0.45;
    let ty = byh * 0.6;
    let wire = l.px(0.42).max(1.4);
    let (red, brown, orange) = (col(214, 52, 40), col(120, 72, 44), col(240, 150, 40));
    l.seg([-tx + 0.3, 0.0], [-w * 0.5, 0.0], wire, red);
    l.seg([tx - 0.3, 0.0], [w * 0.5, 0.0], wire, brown);
    for (dx, c) in [(-0.5f32, red), (0.0, orange), (0.5, brown)] {
        l.seg([dx, byh - 0.4], [dx * 0.3, h * 0.5], wire, c);
    }
    for sx in [-1.0f32, 1.0] {
        let tab = [sx * (bxh - 0.4), -ty, sx * tx, ty];
        l.shadow(tab, 0.5, 0.7);
        l.shaded(tab, 0.5, shade(p.base, 1.05), shade(p.base, 0.76));
        l.disc([sx * (bxh + (tx - bxh) * 0.45), 0.0], 0.72, HOLE);
    }
    let body = [-bxh, -byh, bxh, byh];
    l.shadow(body, 0.9, 1.0);
    l.shaded(body, 0.9, shade(p.base, 1.18), shade(p.base, 0.78));
    l.frame(body, 0.9, 1.0, Color32::from_white_alpha(30));
    let hub = [-bxh * 0.5, 0.0];
    l.dome(hub, byh * 0.92, shade(p.base, 1.28), shade(p.base, 1.06), shade(p.base, 0.84));
    l.dome([bxh * 0.12, -byh * 0.45], byh * 0.32, shade(p.base, 1.25), shade(p.base, 1.05), shade(p.base, 0.85));
    l.text([bxh * 0.2, byh * 0.05, bxh * 0.95, byh * 0.85], "SG90", 1.8, Color32::from_white_alpha(150));
    let rad = (angle - 90.0).to_radians();
    let d = [rad.sin(), -rad.cos()];
    let n = [-d[1], d[0]];
    let len = 7.0f32.min(w * 0.3);
    let (r0, r1) = (1.25, 0.72);
    let tip = [hub[0] + d[0] * len, hub[1] + d[1] * len];
    let arm = [
        [hub[0] + n[0] * r0, hub[1] + n[1] * r0],
        [tip[0] + n[0] * r1, tip[1] + n[1] * r1],
        [tip[0] - n[0] * r1, tip[1] - n[1] * r1],
        [hub[0] - n[0] * r0, hub[1] - n[1] * r0],
    ];
    let horn = col(240, 240, 234);
    l.poly_shadow(&arm, 1.0);
    l.disc_shadow(hub, 2.0, 0.8);
    l.poly_shaded(&arm, horn, col(204, 204, 198));
    l.dome(tip, r1, horn, col(224, 224, 218), col(196, 196, 190));
    l.dome(hub, 2.0, horn, col(228, 228, 222), col(186, 186, 180));
    if p.detail() >= 4.0 {
        for k in 0..3 {
            let t = 3.0 + 1.3 * k as f32;
            l.disc([hub[0] + d[0] * t, hub[1] + d[1] * t], 0.22, gray(120));
        }
    }
    l.dome(hub, 0.62, col(206, 208, 214), col(160, 163, 170), col(116, 119, 126));
    if p.detail() >= 4.0 {
        let k = 0.38;
        l.seg([hub[0] - k, hub[1]], [hub[0] + k, hub[1]], 1.0, gray(70));
        l.seg([hub[0], hub[1] - k], [hub[0], hub[1] + k], 1.0, gray(70));
    }
    (l.pt(tip).y - l.px(r1)).min(l.pt(hub).y - l.px(2.0))
}

/// 1-channel relay module: blue PCB, relay cube with its contact diagram, screw terminals, status LED.
fn relay(p: &Part, closed: bool) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let (px, py) = (w * 0.5 - 0.4, h * 0.5 - 0.4);
    pcb(l, [-px, -py, px, py], 1.0, p.base);
    hole(l, [-px + 2.4, -py + 1.8], 0.85);
    hole(l, [-px + 2.4, py - 1.8], 0.85);
    p.headers(|q| q[0] < 0.0);
    for &q in p.pads.iter().filter(|q| q[0] > 0.0) {
        screw_terminal(l, q[1], px);
    }
    let cube = [-px + 3.6, -py + 4.8, px - 7.0, py - 8.4];
    l.shadow(cube, 0.6, 1.2);
    l.shaded(cube, 0.6, col(64, 136, 240), col(26, 82, 184));
    l.frame(cube, 0.6, 1.0, Color32::from_white_alpha(46));
    let cx = (cube[0] + cube[2]) * 0.5;
    let ink = Color32::from_white_alpha(185);
    l.text([cube[0] + 0.6, cube[1] + 0.8, cube[2] - 0.6, cube[1] + 2.4], "SRD-05VDC-SL-C", 1.0, ink);
    l.text([cube[0] + 0.6, cube[1] + 2.5, cube[2] - 0.6, cube[1] + 3.9], "10A 250VAC", 0.85, ink);
    l.text([cube[0] + 0.6, cube[1] + 3.9, cube[2] - 0.6, cube[1] + 5.3], "10A 30VDC", 0.85, ink);
    if p.detail() >= 3.0 {
        relay_symbol(l, [cx, cube[3] - 6.2], closed);
    }
    let row = py - 3.4;
    status_led(l, [-px + 4.8, row], col(70, 230, 110), if closed { 1.0 } else { 0.0 });
    ic(l, [-px + 9.2, row], 1.5, 1.2, 0);
    if p.detail() >= 4.0 {
        for k in [-0.5f32, 0.5] {
            l.fill(bx([-px + 9.2 + k, row + 0.85], 0.3, 0.5), 0.0, TIN);
        }
        l.fill(bx([-px + 9.2, row - 0.85], 0.3, 0.5), 0.0, TIN);
    }
    let diode = bx([-px + 13.4, row], 2.6, 1.2);
    l.shaded(diode, 0.2, gray(52), gray(22));
    l.fill([diode[0] + 0.3, diode[1], diode[0] + 0.65, diode[3]], 0.0, gray(150));
    smd(l, [-px + 17.0, row], 1.6, 0.85, gray(30));
}

/// Relay contact diagram: coil, and a blade thrown to NO when closed or NC when open.
fn relay_symbol(l: &Local, at: [f32; 2], closed: bool) {
    let ink = Color32::from_white_alpha(200);
    let wd = l.px(0.16).max(1.0);
    let [x, y] = at;
    l.frame([x - 5.4, y - 1.4, x - 3.0, y + 2.6], 0.0, wd, ink);
    l.seg([x - 4.2, y - 1.4], [x - 4.2, y - 2.6], wd, ink);
    l.seg([x - 4.2, y + 2.6], [x - 4.2, y + 3.6], wd, ink);
    let pivot = [x + 1.4, y + 3.2];
    let nc = [x - 0.2, y - 1.8];
    let no = [x + 3.0, y - 1.8];
    l.seg([x + 1.4, y + 3.2], [x + 1.4, y + 4.2], wd, ink);
    l.seg(nc, [nc[0], nc[1] - 1.2], wd, ink);
    l.seg(no, [no[0], no[1] - 1.2], wd, ink);
    l.seg(pivot, if closed { no } else { nc }, l.px(0.22).max(1.2), ink);
    for c in [pivot, nc, no] {
        l.disc(c, 0.28, ink);
    }
    let mut xd = x - 2.8;
    while xd < x + 0.8 {
        l.seg([xd, y + 0.6], [xd + 0.4, y + 0.6], wd, ink.gamma_multiply(0.7));
        xd += 0.8;
    }
}

/// Green 1-way screw terminal at the module's right edge, centred on `y`.
fn screw_terminal(l: &Local, y: f32, edge: f32) {
    let b = [edge - 6.0, y - 2.55, edge, y + 2.55];
    l.shadow(b, 0.4, 0.8);
    l.shaded(b, 0.4, col(54, 176, 108), col(22, 108, 64));
    l.frame(b, 0.4, 1.0, Color32::from_black_alpha(60));
    let sc = [edge - 3.4, y];
    l.dome(sc, 1.5, CHROME_HI, col(176, 180, 186), col(108, 112, 118));
    l.seg([sc[0] - 0.9, sc[1] - 0.9], [sc[0] + 0.9, sc[1] + 0.9], l.px(0.32).max(1.0), gray(64));
    l.fill([edge - 1.1, y - 1.15, edge, y + 1.15], 0.15, gray(14));
}

/// ST7735 module: red PCB, headers, black bezel, unpowered glass on the shared screen square, FPC, SD socket.
fn tft(p: &Part) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let (px, py) = (w * 0.5 - 0.4, h * 0.5 - 0.4);
    pcb(l, [-px, -py, px, py], 1.0, p.base);
    let side = 0.82 * w.min(h);
    let cy = -0.06 * h;
    let g = [-side * 0.5, cy - side * 0.5, side * 0.5, cy + side * 0.5];
    for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        hole(l, [sx * (px - 1.8), sy * (py - 1.5)], 0.9);
    }
    p.headers(|_| true);
    let bezel = [g[0] - 0.55, g[1] - 0.6, g[2] + 0.55, g[3] + 2.3];
    let sd = [-6.4, (bezel[3] + 2.2).min(py - 2.0), 6.4, py];
    l.shaded(sd, 0.3, col(214, 216, 220), col(150, 153, 160));
    l.frame(sd, 0.3, 1.0, col(108, 111, 118));
    if p.detail() >= 3.0 {
        l.fill([sd[0] + 0.8, sd[3] - 0.6, sd[2] - 0.8, sd[3] - 0.15], 0.1, gray(30));
        for k in 0..4 {
            let x = sd[0] + 2.2 + k as f32 * 2.6;
            l.seg([x, sd[1] + 0.6], [x, sd[1] + 2.0], 1.0, Color32::from_black_alpha(60));
        }
    }
    l.fill([-4.6, bezel[3] - 0.2, 4.6, bezel[3] + 1.3], 0.15, col(198, 134, 42));
    l.shadow(bezel, 0.3, 0.9);
    l.fill(bezel, 0.3, gray(16));
    l.shaded(g, 0.0, col(16, 18, 24), col(5, 6, 9));
    if p.detail() >= 2.0 {
        let s = side;
        let band = |a: f32, b: f32| {
            [[g[0] + s * a, g[1]], [g[0] + s * b, g[1]], [g[0], g[1] + s * b], [g[0], g[1] + s * a]]
        };
        l.poly(&band(0.2, 0.52), Color32::from_white_alpha(9));
        l.poly(&band(0.6, 0.66), Color32::from_white_alpha(6));
        l.seg([g[0], g[1]], [g[2], g[1]], 1.0, Color32::from_white_alpha(26));
    }
    l.text([-px + 3.0, -py + 0.2, px - 3.0, bezel[1] - 0.2], "1.44 TFT 128x128", 1.1, SILK);
}

/// TTP223 touch module: PCB with a large round touch pad, IC and an LED that lights on touch.
fn touch(p: &Part, on: bool) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let (px, py) = (w * 0.5 - 0.4, h * 0.5 - 0.4);
    pcb(l, [-px, -py, px, py], 1.2, p.base);
    for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        hole(l, [sx * (px - 1.8), sy * (py - 1.8)], 0.8);
    }
    p.headers(|_| true);
    let c = [0.0, -0.06 * h];
    let rp = w.min(h) * 0.29;
    l.disc(c, rp + 0.6, shade(p.base, 0.7));
    l.dome(c, rp, shade(p.base, 1.5), shade(p.base, 1.34), shade(p.base, 1.16));
    if p.detail() >= 2.5 {
        for k in 1..=2 {
            l.ring(c, rp * 0.34 * k as f32, l.px(0.25).max(1.0), shade(p.base, 1.12));
        }
        l.disc(c, rp * 0.1, shade(p.base, 1.12));
    }
    if on {
        l.glow(c, rp * 1.3, Color32::WHITE, 0.35);
    }
    let row = py - 3.6;
    ic(l, [-w * 0.2, row], 2.9, 1.6, 3);
    status_led(l, [w * 0.2, row], col(255, 64, 48), if on { 1.0 } else { 0.0 });
    l.text([-px + 3.0, -py + 0.2, px - 3.0, c[1] - rp - 0.8], "TTP223", 1.4, SILK);
}

/// Generic analog sensor breakout: sensing element, blue trimmer, comparator and a level LED.
fn analog_sensor(p: &Part, level: f32) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let (px, py) = (w * 0.5 - 0.4, h * 0.5 - 0.4);
    pcb(l, [-px, -py, px, py], 1.2, p.base);
    hole(l, [-px + 2.0, -py + 2.0], 0.9);
    hole(l, [px - 2.0, -py + 2.0], 0.9);
    p.headers(|_| true);
    let e = [0.0, -h * 0.22];
    let pkg = bx(e, 8.0, 8.0);
    l.shadow(pkg, 0.6, 1.0);
    l.shaded(pkg, 0.6, gray(50), gray(22));
    l.frame(pkg, 0.6, 1.0, Color32::from_white_alpha(18));
    l.disc(e, 2.6, gray(12));
    l.dome(e, 2.1, col(244, 218, 146), col(206, 168, 88), col(146, 112, 50));
    if p.detail() >= 4.0 {
        let mut y = e[1] - 1.5;
        while y <= e[1] + 1.5 {
            let half = (2.1f32.powi(2) - (y - e[1]).powi(2)).max(0.0).sqrt() * 0.9;
            l.seg([e[0] - half, y], [e[0] + half, y], 1.0, Color32::from_black_alpha(50));
            y += 0.5;
        }
    }
    let t = [-w * 0.16, h * 0.06];
    let trim = bx(t, 5.0, 5.0);
    l.shadow(trim, 0.4, 0.9);
    l.shaded(trim, 0.4, col(76, 128, 236), col(34, 74, 172));
    l.dome([t[0] + 1.5, t[1] - 1.5], 0.95, col(236, 210, 140), col(196, 160, 84), col(140, 108, 50));
    l.seg([t[0] + 0.85, t[1] - 1.5], [t[0] + 2.15, t[1] - 1.5], l.px(0.22).max(1.0), gray(70));
    ic(l, [-w * 0.14, h * 0.3], 3.9, 3.0, 4);
    status_led(l, [w * 0.24, h * 0.2], col(255, 70, 50), 0.0);
    status_led(l, [w * 0.24, h * 0.3], col(80, 230, 110), level);
    for (dx, dy) in [(0.24, 0.06), (0.24, 0.12)] {
        smd(l, [w * dx, h * dy], 1.6, 0.85, gray(30));
    }
    l.text([-px + 3.0, -py + 0.3, px - 3.0, -py + 3.2], "SENSOR", 1.4, SILK);
}

/// DHT22: white housing with mounting ear, grille of vent holes, label and pins.
fn dht(p: &Part, on: bool) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let hw = w * 0.5 - 0.3;
    let top = -h * 0.5 + 0.1;
    let bot = h * 0.5 - 1.7;
    let ear = top + 2.4;
    for sx in [-1.0f32, 1.0] {
        l.fill([sx * (hw - 0.2), -0.3, sx * w * 0.5, 0.3], 0.1, TIN);
    }
    l.fill([-0.3, bot - 0.2, 0.3, h * 0.5], 0.1, TIN);
    l.shadow([-hw, top, hw, bot], 0.7, 1.0);
    let span = l.rect([-hw, top, hw, bot]);
    let (lite, dark) = (shade(p.base, 1.05), shade(p.base, 0.8));
    l.shaded_in([-hw * 0.62, top, hw * 0.62, ear + 0.6], 0.8, lite, dark, span);
    l.shaded_in([-hw, ear, hw, bot], 0.7, lite, dark, span);
    l.disc([0.0, top + 1.2], 0.8, HOLE);
    let grille = [-hw + 0.9, ear + 0.9, hw - 0.9, bot - 2.6];
    if p.detail() >= 3.0 {
        let (cols, rows) = (4, 6);
        let cw = (grille[2] - grille[0]) / cols as f32;
        let rh = (grille[3] - grille[1]) / rows as f32;
        for r in 0..rows {
            let shift = if r % 2 == 1 { cw * 0.5 } else { 0.0 };
            for c in 0..cols {
                let x = grille[0] + cw * (c as f32 + 0.5) + shift;
                if x > grille[2] - cw * 0.3 {
                    continue;
                }
                let y = grille[1] + rh * (r as f32 + 0.5);
                l.fill(bx([x, y], cw * 0.62, rh * 0.62), 0.1, gray(84));
            }
        }
    } else {
        l.fill(grille, 0.3, gray(150));
    }
    l.text([-hw, bot - 2.4, hw, bot - 0.4], "DHT22", 1.3, gray(120));
    if on {
        status_led(l, [hw - 1.1, bot - 1.0], col(80, 230, 110), 1.0);
    }
}

/// HC-SR04: blue PCB, two silver transducers with mesh fronts, crystal; pings while the echo is high.
fn ultrasonic(p: &Part, on: bool, time: f64) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let (px, py) = (w * 0.5 - 0.3, h * 0.5 - 0.3);
    pcb(l, [-px, -py, px, py], 0.5, p.base);
    p.headers(|_| true);
    let rt = (h * 0.5 - 1.05).min(w * 0.22);
    let cx = w * 0.247;
    l.shaded(bx([0.0, -py + 1.2], 2.6, 1.1), 0.5, CHROME_HI, CHROME_LO);
    l.text([-2.6, py - 1.9, 2.6, py - 0.1], "HC-SR04", 0.95, SILK);
    for s in [-1.0f32, 1.0] {
        let c = [s * cx, 0.1];
        l.disc_shadow(c, rt, 1.0);
        l.dome(c, rt, col(238, 240, 244), col(192, 196, 202), col(118, 122, 128));
        l.ring(c, rt, 1.0, Color32::from_black_alpha(80));
        let rm = rt * 0.8;
        l.disc(c, rm, gray(56));
        if p.detail() >= 4.5 {
            mesh_grille(l, c, rm, 0.42, gray(98));
        } else {
            l.ring(c, rm * 0.55, 1.0, gray(90));
        }
        l.ring(c, rm, l.px(0.25).max(1.0), gray(150));
        l.specular(c, rt, 50);
        if on {
            let wave = col(120, 220, 255);
            for k in 0..2 {
                let ph = ((time * 1.4).fract() as f32 + k as f32 * 0.5).fract();
                let rr = rt + 0.6 + 3.2 * ph;
                let color = wave.gamma_multiply(0.85 * (1.0 - ph));
                l.arc(c, rr, -FRAC_PI_2 - 0.7, -FRAC_PI_2 + 0.7, l.px(0.3).clamp(1.2, 3.0), color);
            }
        }
    }
}

/// HC-SR501 PIR: green PCB, white Fresnel dome on a square base, trim pots, status LED.
fn pir(p: &Part, on: bool) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let (px, py) = (w * 0.5 - 0.3, h * 0.5 - 0.3);
    pcb(l, [-px, -py, px, py], 1.0, col(34, 112, 64));
    hole(l, [-px + 1.9, py - 1.9], 0.9);
    hole(l, [px - 1.9, py - 1.9], 0.9);
    p.headers(|_| true);
    let c = [0.0, -h * 0.1];
    let half = px - 1.3;
    let lens = bx(c, half * 2.0, half * 2.0);
    l.shadow(lens, 2.2, 1.0);
    l.shaded(lens, 2.2, shade(p.base, 0.98), shade(p.base, 0.78));
    let rd = half * 0.93;
    l.disc_shadow(c, rd, 0.5);
    l.dome(c, rd, mix(p.base, Color32::WHITE, 0.6), p.base, shade(p.base, 0.74));
    if p.detail() >= 2.5 {
        fresnel(l, c, rd, Color32::from_black_alpha(34));
    }
    l.specular(c, rd, 80);
    let row = (lens[3] + py) * 0.5 - 0.6;
    for sx in [-1.0f32, 1.0] {
        trim_pot(l, [sx * (px - 4.6), row], 3.4);
    }
    status_led(l, [0.0, row - 0.6], col(255, 64, 48), if on { 1.0 } else { 0.0 });
    l.text([-3.4, row + 0.4, 3.4, row + 2.0], "HC-SR501", 1.0, SILK);
}

/// Soil probe: two pointed prongs with gold electrodes, castellated side pads and a header; wet to `level`.
fn soil(p: &Part, level: f32) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let px = w * 0.5 - 0.4;
    let py = h * 0.5 - 0.4;
    let gap = w * 0.15;
    let tip = -h * 0.5 + 0.2;
    let neck = h * 0.19;
    let prong = |s: f32| -> Vec<[f32; 2]> {
        let (xi, xo) = (s * gap, s * px);
        vec![[xi, neck + 0.8], [xi, tip + 4.2], [(xi + xo) * 0.5, tip], [xo, tip + 4.2], [xo, neck + 0.8]]
    };
    let span = l.rect([-px, tip, px, py]);
    let (lite, dark) = (shade(p.base, 1.16), shade(p.base, 0.82));
    for s in [-1.0f32, 1.0] {
        l.poly_shadow(&prong(s), 0.9);
    }
    l.shadow([-px, neck, px, py], 0.5, 1.0);
    for s in [-1.0f32, 1.0] {
        l.poly_grad(&prong(s), lite, dark, span);
    }
    l.shaded_in([-px, neck, px, py], 0.5, lite, dark, span);
    let outline = [
        [-px, py],
        [-px, tip + 4.2],
        [-(gap + px) * 0.5, tip],
        [-gap, tip + 4.2],
        [-gap, neck],
        [gap, neck],
        [gap, tip + 4.2],
        [(gap + px) * 0.5, tip],
        [px, tip + 4.2],
        [px, py],
    ];
    l.poly_outline(&outline, 1.0, mix(p.base, Color32::WHITE, 0.25).gamma_multiply(0.55));
    for s in [-1.0f32, 1.0] {
        let xm = s * (gap + px) * 0.5;
        let ew = (px - gap) * 0.27;
        l.fill([xm, -0.35, s * px, 0.35], 0.0, PLATING);
        l.disc([s * px, 0.0], 0.95, PLATING);
        l.shaded([xm - ew, tip + 5.0, xm + ew, neck - 1.2], ew, col(240, 206, 120), col(176, 136, 58));
        if p.detail() >= 3.5 {
            l.seg([xm - ew * 0.4, tip + 6.0], [xm - ew * 0.4, neck - 2.2], 1.0, Color32::from_white_alpha(70));
        }
    }
    let wet = tip + level * (neck - tip - 1.0);
    if level > 0.02 {
        let water = col(80, 170, 255);
        for s in [-1.0f32, 1.0] {
            let pts = clip_below(&prong(s), wet);
            if pts.len() >= 3 {
                l.poly(&pts, water.gamma_multiply(0.14));
            }
            let (x0, x1) = if s < 0.0 { (-px, -gap) } else { (gap, px) };
            if wet > tip + 4.2 {
                l.seg([x0, wet], [x1, wet], 1.0, water.gamma_multiply(0.55));
            }
        }
    }
    p.headers(|q| q[1] > h * 0.25);
    hole(l, [0.0, py - 6.0], 1.4);
    l.text([-px + 1.5, neck + 1.5, px - 1.5, neck + 5.5], "SOIL", 2.2, SILK);
    l.text([-px + 1.5, neck + 5.4, px - 1.5, neck + 7.6], "+   S   -", 1.4, SILK);
}

/// RFID-RC522: blue PCB with a rectangular antenna coil, reader IC, crystal and headers.
fn rc522(p: &Part) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let (px, py) = (w * 0.5 - 0.3, h * 0.5 - 0.3);
    pcb(l, [-px, -py, px, py], 0.6, p.base);
    let ant = [-px + 1.6, -py + 1.3, px * 0.2, py - 1.3];
    let trace = mix(p.base, col(150, 190, 255), 0.45);
    if p.detail() >= 2.5 {
        for k in 0..4 {
            let d = k as f32 * 0.85;
            let turn = [ant[0] + d, ant[1] + d, ant[2] - d, ant[3] - d];
            l.frame(turn, (1.6 - d * 0.4).max(0.3), l.px(0.38).max(1.0), trace);
        }
        for y in [-0.9f32, 0.9] {
            l.seg([ant[2], y], [px * 0.4, y], l.px(0.38).max(1.0), trace);
        }
    } else {
        l.frame(ant, 1.6, 1.0, trace);
    }
    let chip_at = [px * 0.55, -py * 0.3];
    ic(l, chip_at, 3.6, 3.6, 0);
    if p.detail() >= 5.0 {
        let at = |dx: f32, dy: f32| [chip_at[0] + dx, chip_at[1] + dy];
        for k in 0..6 {
            let t = -1.5 + 0.6 * k as f32;
            l.seg(at(-2.0, t), at(-1.8, t), 1.0, TIN);
            l.seg(at(1.8, t), at(2.0, t), 1.0, TIN);
            l.seg(at(t, -2.0), at(t, -1.8), 1.0, TIN);
            l.seg(at(t, 1.8), at(t, 2.0), 1.0, TIN);
        }
    }
    l.shaded(bx([px * 0.55, py * 0.42], 3.6, 1.5), 0.75, CHROME_HI, CHROME_LO);
    for (dx, dy) in [(-2.7, -4.6), (2.6, -4.6), (-2.7, 1.2), (2.6, 1.2)] {
        smd(l, [chip_at[0] + dx * 0.8, chip_at[1] + dy * 0.6], 1.2, 0.65, CAP_TAN);
    }
    p.headers(|_| true);
    l.text([px * 0.2 + 0.6, py - 2.2, px - 1.6, py - 0.4], "RFID-RC522", 0.9, SILK);
}

/// RDM6300: small reader PCB with headers plus its round copper 125 kHz coil wired to ANT1/ANT2.
fn rdm6300(p: &Part, on: bool) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let py = h * 0.5 - 0.3;
    let mx = w * 0.12;
    pcb(l, [-w * 0.5 + 0.3, -py, mx, py], 0.6, p.base);
    p.headers(|q| q[0] < 0.0);
    ic(l, [-w * 0.15, 0.0], 3.0, 4.4, 4);
    for (dx, dy) in [(-0.02, -5.0), (-0.02, 5.0), (0.06, -2.6), (0.06, 2.6)] {
        smd(l, [w * dx, dy], 1.2, 0.65, CAP_TAN);
    }
    status_led(l, [mx - 1.3, -py + 1.4], col(80, 230, 110), if on { 1.0 } else { 0.0 });
    l.text([-w * 0.5 + 3.0, py - 2.0, mx - 0.4, py - 0.3], "RDM6300", 0.95, SILK);
    let c = [w * 0.31, 0.0];
    let ro = w * 0.5 - c[0] - 0.15;
    let ri = ro * 0.64;
    let copper = col(196, 116, 52);
    for &q in p.pads.iter().filter(|q| q[0] > 0.0) {
        let s = q[1].signum();
        let a = s * 0.75;
        l.path(
            &[[c[0] + ro * a.cos() * 0.98, s * ro * a.sin().abs()], [q[0] - 1.0, q[1] * 0.85], [q[0], q[1]]],
            l.px(0.28).max(1.0),
            copper,
        );
    }
    l.disc_shadow(c, ro, 0.9);
    let mid = (ro + ri) * 0.5;
    l.ring(c, mid, l.px(ro - ri), copper);
    if p.detail() >= 4.0 {
        let n = 5;
        for k in 1..n {
            let r = ri + (ro - ri) * k as f32 / n as f32;
            l.ring(c, r, 1.0, Color32::from_black_alpha(55));
        }
    }
    l.ring(c, ro, 1.0, shade(copper, 0.6));
    l.ring(c, ri, 1.0, shade(copper, 0.6));
    let (a0, a1) = (l.screen_angle(-2.4), l.screen_angle(-1.2));
    l.arc(c, mid, a0, a1, l.px((ro - ri) * 0.3).max(1.0), Color32::from_white_alpha(60));
    l.fill([c[0] - 0.7, -ro - 0.1, c[0] + 0.7, -ri + 0.1], 0.1, col(232, 196, 60));
}

/// Generic chip: black DIP body with legs out to every pad.
fn chip(p: &Part) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let bw = (w * 0.5 - 1.2).max(w * 0.3);
    let bh = h * 0.5 - 0.3;
    for &q in &p.pads {
        let inner = [q[0].signum() * (bw - 0.2), q[1]];
        l.seg(inner, q, l.px(0.6).max(1.2), TIN);
    }
    l.shadow([-bw, -bh, bw, bh], 0.4, 1.0);
    l.shaded([-bw, -bh, bw, bh], 0.4, gray(52), gray(22));
    l.frame([-bw, -bh, bw, bh], 0.4, 1.0, Color32::from_white_alpha(18));
    l.disc([0.0, -bh], (bw * 0.22).min(1.0), gray(14));
    if p.detail() >= 4.0 {
        l.disc([-bw + 1.0, -bh + 1.0], 0.35, gray(70));
    }
}

/// Generic sensor module: PCB, headers on every pad, an IC and a status LED.
/// Photoflash electrolytic can: sleeve, polarity stripe, charge level and a volts readout.
fn capacitor(p: &Part, volts: f32, rated: f32, time: f64) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let (ex, ey) = (w * 0.5 - 1.6, h * 0.5 - 0.2);
    // Each lead runs from the nearer end of the can to its pad.
    for pad in &p.pads {
        let x = if pad[0] < 0.0 { -ex } else { ex };
        l.lead([x, pad[1].clamp(-ey * 0.8, ey * 0.8)], *pad, 0.6);
    }
    let radial = p.pads.len() == 2 && (p.pads[0][0] < 0.0) == (p.pads[1][0] < 0.0);
    let body = [-ex, -ey, ex, ey];
    let frac = (volts / rated.max(1.0)).clamp(0.0, 1.2);
    let charged = col(255, 170, 40);
    if frac > 0.1 {
        l.glow([0.0, 0.0], ey * 2.2, charged, 0.3 * frac.min(1.0));
    }
    l.shadow(body, 1.4, 1.0);
    l.shaded(body, 1.4, col(64, 68, 80), col(20, 22, 28));
    // Charge level rises from the - end toward +.
    let over = frac > 1.0;
    let level = if over {
        if (time * 6.0).sin() > 0.0 { col(255, 40, 40) } else { col(120, 0, 0) }
    } else {
        mix(col(40, 150, 90), charged, frac.min(1.0))
    };
    if frac > 0.004 {
        let top = ex - 0.6 - (2.0 * ex - 1.2) * frac.min(1.0);
        l.fill([top, -ey + 0.6, ex - 0.6, ey - 0.6], 1.0, level.gamma_multiply(0.75));
    }
    // Negative stripe on the side of the - lead, + mark at the other end.
    let stripe = col(160, 168, 186).gamma_multiply(0.9);
    if radial {
        let below = p.pads[1][1] >= p.pads[0][1];
        let (y0, y1) = if below { (ey - 2.6, ey - 1.0) } else { (-ey + 1.0, -ey + 2.6) };
        l.fill([-ex, y0, ex, y1], 0.0, stripe);
        for k in [-0.55f32, 0.0, 0.55] {
            l.fill(bx([k * ex, (y0 + y1) * 0.5], 1.2, 0.35), 0.0, col(40, 42, 48));
        }
    } else {
        l.fill([ex - 2.6, -ey, ex - 1.0, ey], 0.0, stripe);
        for k in [-0.55f32, 0.0, 0.55] {
            l.fill(bx([ex - 1.8, k * ey], 0.35, 1.2), 0.0, col(40, 42, 48));
        }
    }
    l.text([-ex, -ey, -ex + 3.0, -ey + 3.0], "+", 2.2, col(255, 220, 150));
    l.frame(body, 1.4, 1.0, Color32::from_white_alpha(36));
    let label = if volts.abs() >= 10.0 { format!("{volts:.0}V") } else { format!("{volts:.1}V") };
    l.text(body, &label, 3.4, if over { col(255, 120, 120) } else { Color32::WHITE });
}

/// Glass signal diode with its cathode band; glows faintly while conducting.
fn diode(p: &Part, conducting: bool) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let half = w * 0.25;
    l.lead([-w * 0.5, 0.0], [-half, 0.0], 0.45);
    l.lead([w * 0.5, 0.0], [half, 0.0], 0.45);
    let body = [-half, -h * 0.42, half, h * 0.42];
    if conducting {
        l.glow([0.0, 0.0], h * 1.6, col(255, 120, 60), 0.6);
    }
    l.shaded(body, h * 0.4, col(240, 150, 80), col(180, 80, 30));
    l.fill([half - 1.0, body[1], half - 0.45, body[3]], 0.0, col(20, 20, 20));
    l.frame(body, h * 0.4, 1.0, Color32::from_white_alpha(70));
}

/// Wall brick with output readout.
fn supply(p: &Part, volts: Option<f32>, amps: f32) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let b = [-w * 0.5 + 0.6, -h * 0.5 + 0.6, w * 0.5 - 0.6, h * 0.5 - 0.6];
    l.shadow(b, 2.0, 1.2);
    l.shaded(b, 2.0, col(58, 58, 64), col(18, 18, 22));
    for i in 0..5 {
        let x = b[0] + 4.0 + i as f32 * 1.6;
        l.fill([x, b[1] + 3.0, x + 0.7, b[3] - 3.0], 0.3, col(10, 10, 12));
    }
    l.frame(b, 2.0, 1.0, Color32::from_white_alpha(28));
    let lit = if volts.is_some_and(|v| v > 1.0) { 1.0 } else { 0.0 };
    status_led(l, [b[2] - 3.0, b[1] + 3.0], col(80, 230, 110), lit);
    l.text([b[0] + 13.0, b[1] + 2.0, b[2] - 6.0, b[1] + 8.0], "12V DC", 3.0, col(225, 225, 230));
    l.text([b[0] + 13.0, b[1] + 8.0, b[2] - 6.0, b[1] + 12.0], "FLOATING", 1.8, col(160, 160, 170));
    if let Some(v) = volts {
        l.text([b[0] + 10.0, b[3] - 7.0, b[2] - 4.0, b[3] - 2.5], &format!("{v:.1} V  {amps:.2} A"), 2.2, col(140, 230, 170));
    }
}

/// ZVS capacitor-charger board: toroid, heatsink, trimpot, terminal blocks and a mode readout.
fn boost(p: &Part, st: Option<(BoostMode, f32, f32)>, time: f64) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let (px, py) = (w * 0.5 - 0.4, h * 0.5 - 0.4);
    pcb(l, [-px, -py, px, py], 1.0, p.base);
    for c in [[-px + 1.8, -py + 1.8], [px - 1.8, -py + 1.8], [-px + 1.8, py - 1.8], [px - 1.8, py - 1.8]] {
        hole(l, c, 0.9);
    }
    for &q in &p.pads {
        let edge = if q[0] < 0.0 { -px + 6.0 } else { px };
        screw_terminal(l, q[1], edge);
    }
    let mode = st.map(|s| s.0).unwrap_or(BoostMode::Off);
    let active = !matches!(mode, BoostMode::Off);
    // Toroid transformer.
    let t = [-3.0, -1.0];
    if active && !matches!(mode, BoostMode::Regulating) {
        let flicker = 0.5 + 0.2 * ((time * 31.0).sin() as f32);
        l.glow(t, 9.0, col(255, 180, 60), flicker * 0.5);
    }
    l.disc_shadow(t, 7.0, 1.0);
    l.dome(t, 7.0, col(200, 120, 70), col(160, 80, 40), col(96, 46, 20));
    l.disc(t, 2.6, shade(p.base, 0.6));
    for k in 0..18 {
        let a = k as f32 / 18.0 * TAU;
        l.seg([t[0] + 2.8 * a.cos(), t[1] + 2.8 * a.sin()], [t[0] + 6.8 * a.cos(), t[1] + 6.8 * a.sin()], 1.0, col(214, 140, 80));
    }
    // Heatsink.
    let hs = [6.0, -py + 3.0, 12.0, py - 8.0];
    l.shaded(hs, 0.3, col(170, 176, 186), col(110, 116, 126));
    for i in 0..4 {
        let y = hs[1] + 1.0 + i as f32 * (hs[3] - hs[1] - 2.0) / 3.0;
        l.seg([hs[0], y], [hs[2], y], 1.0, col(90, 94, 104));
    }
    trim_pot(l, [-11.0, py - 4.0], 2.8);
    let (color, label) = match st {
        Some((BoostMode::ConstantCurrent, _, a)) => (col(255, 170, 40), format!("CC {:.0} mA", a * 1000.0)),
        Some((BoostMode::ConstantPower, v, a)) => (col(255, 140, 40), format!("CP {:.0} W", v * a)),
        Some((BoostMode::Regulating, v, _)) => (col(80, 230, 110), format!("REG {v:.0} V")),
        _ => (col(120, 120, 130), "OFF".to_string()),
    };
    status_led(l, [-px + 9.0, -py + 3.0], color, if active { 1.0 } else { 0.0 });
    l.text([-px + 7.0, py - 6.5, px - 7.0, py - 1.5], &label, 2.8, color);
}

/// Pin vise with tungsten electrode; flashes when a pulse fires and prints the last pulse.
fn pen(p: &Part, touching: bool, st: Option<(f32, Option<ArcPulse>, f32)>, time: f64) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let x0 = -w * 0.5;
    let grip = [x0 + 3.0, -h * 0.42, x0 + w * 0.58, h * 0.42];
    l.lead([x0, 0.0], [grip[0], 0.0], 0.9);
    l.shadow(grip, h * 0.4, 1.0);
    l.shaded(grip, h * 0.4, col(70, 30, 30), col(30, 10, 10));
    for i in 0..10 {
        let x = grip[0] + 2.0 + i as f32 * (grip[2] - grip[0] - 4.0) / 9.0;
        l.seg([x, grip[1] + 0.6], [x, grip[3] - 0.6], 1.0, Color32::from_black_alpha(80));
    }
    let collet = [grip[2], -h * 0.26, grip[2] + 6.0, h * 0.26];
    l.shaded(collet, 0.8, CHROME_HI, CHROME_LO);
    let reach = if touching { 0.6 } else { 0.0 };
    let tip = [w * 0.5 - 0.2 + reach, 0.0];
    l.seg([collet[2], 0.0], tip, l.px(0.55).max(1.5), col(150, 150, 160));
    l.text([grip[0] + 1.0, grip[1], grip[2] - 1.0, grip[3]], "INSULATED", 1.6, Color32::from_white_alpha(110));
    let Some((amps, pulse, since)) = st else { return };
    let flash = if amps > 0.5 { 1.0 } else { (1.0 - since / 0.35).clamp(0.0, 1.0) };
    if flash > 0.0 {
        let joules = pulse.map(|p| p.joules).unwrap_or(1.0);
        let r = 4.0 + joules.sqrt() * 4.0;
        l.glow(tip, r * 2.2, col(120, 170, 255), 0.9 * flash);
        l.glow(tip, r, Color32::WHITE, flash);
        for k in 0..7 {
            let a = k as f32 / 7.0 * TAU + (time * 13.0) as f32;
            let len = r * (0.6 + 0.4 * ((time * 37.0 + k as f64).sin() as f32).abs());
            l.seg(tip, [tip[0] + len * a.cos(), tip[1] + len * a.sin()], 1.5, col(200, 220, 255).gamma_multiply(flash));
        }
    }
    if let Some(p) = pulse
        && since < 6.0
    {
        let alpha = (1.0 - (since - 3.0).max(0.0) / 3.0).clamp(0.0, 1.0);
        let txt = format!("{:.0} A  {:.2} J  {:.0} us", p.peak_a, p.joules, p.width_us);
        l.text([x0, h * 0.5 + 0.5, w * 0.5, h * 0.5 + 4.0], &txt, 2.4, col(160, 200, 255).gamma_multiply(alpha));
    }
}

/// Steel alligator clip; the jaw end bites the work.
fn clamp(p: &Part) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let x0 = -w * 0.5;
    l.lead([x0, 0.0], [x0 + 3.0, 0.0], 0.9);
    let upper = [[x0 + 3.0, -h * 0.15], [x0 + 12.0, -h * 0.45], [w * 0.5, -h * 0.08], [w * 0.5, 0.0], [x0 + 3.0, 0.0]];
    let lower = [[x0 + 3.0, 0.0], [w * 0.5, 0.0], [w * 0.5, h * 0.08], [x0 + 12.0, h * 0.45], [x0 + 3.0, h * 0.15]];
    l.poly_shadow(&upper, 0.8);
    l.poly_shaded(&upper, CHROME_HI, CHROME_LO);
    l.poly_shaded(&lower, shade(CHROME_HI, 0.9), shade(CHROME_LO, 0.8));
    for i in 0..6 {
        let x = x0 + 16.0 + i as f32 * 1.7;
        l.seg([x, -0.9], [x + 0.8, 0.0], 1.0, col(80, 84, 92));
        l.seg([x, 0.9], [x + 0.8, 0.0], 1.0, col(80, 84, 92));
    }
    l.disc([x0 + 12.0, 0.0], 1.2, col(90, 94, 100));
}

fn module(p: &Part, on: bool) {
    let l = &p.l;
    let (w, h) = (p.w, p.h);
    let (px, py) = (w * 0.5 - 0.4, h * 0.5 - 0.4);
    pcb(l, [-px, -py, px, py], 0.8, p.base);
    if w.min(h) > 12.0 {
        hole(l, [-px + 1.8, -py + 1.8], 0.8);
        hole(l, [px - 1.8, -py + 1.8], 0.8);
    }
    p.headers(|_| true);
    let s = (w.min(h) * 0.3).max(2.0);
    ic(l, [0.0, 0.0], s, s, 0);
    status_led(l, [px - 2.0, -py + 2.0], col(80, 230, 110), if on { 1.0 } else { 0.0 });
}

// ------------------------------------------------------------- shared bits --

/// Soldermask board with drop shadow, top-lit gradient and a light edge.
fn pcb(l: &Local, b: [f32; 4], r: f32, color: Color32) {
    l.shadow(b, r, 1.0);
    l.shaded(b, r, shade(color, 1.16), shade(color, 0.82));
    l.frame(b, r, 1.0, mix(color, Color32::WHITE, 0.35).gamma_multiply(0.6));
}

/// Plated mounting hole.
fn hole(l: &Local, at: [f32; 2], r: f32) {
    l.disc(at, r, PLATING);
    l.disc(at, r * 0.62, HOLE);
}

/// Header pin base under a pad.
fn header(l: &Local, at: [f32; 2]) {
    let b = bx(at, 2.44, 2.44);
    l.shaded(b, 0.3, gray(56), gray(22));
    l.frame(b, 0.3, 1.0, Color32::from_white_alpha(16));
}

/// Black IC body; `legs` per side along local x.
fn ic(l: &Local, at: [f32; 2], w: f32, h: f32, legs: usize) {
    if legs > 0 && l.scale >= 3.5 {
        for i in 0..legs {
            let y = at[1] - h * 0.5 + h * (i as f32 + 0.5) / legs as f32;
            l.fill([at[0] - w * 0.5 - 0.4, y - 0.17, at[0] + w * 0.5 + 0.4, y + 0.17], 0.0, TIN);
        }
    }
    let b = bx(at, w, h);
    l.shaded(b, 0.2, gray(50), gray(22));
    l.frame(b, 0.2, 1.0, Color32::from_white_alpha(14));
    if l.scale >= 5.0 {
        l.disc([b[0] + 0.45, b[1] + 0.45], 0.18, gray(76));
    }
}

/// SMD passive with tinned end caps along its long side.
fn smd(l: &Local, at: [f32; 2], w: f32, h: f32, body: Color32) {
    if l.scale < 3.5 {
        return;
    }
    let b = bx(at, w, h);
    l.fill(b, 0.05, TIN);
    let cap = w.max(h) * 0.22;
    let inner = if w >= h { [b[0] + cap, b[1], b[2] - cap, b[3]] } else { [b[0], b[1] + cap, b[2], b[3] - cap] };
    l.fill(inner, 0.0, body);
}

/// Small SMD LED: package, lens tinted by `color`, glowing with `lit` (0..=1).
fn status_led(l: &Local, at: [f32; 2], color: Color32, lit: f32) {
    if lit > 0.02 {
        l.glow(at, 2.8, color, 0.95 * lit);
    }
    l.fill(bx(at, 1.7, 1.0), 0.15, col(222, 222, 214));
    l.fill(bx(at, 0.9, 0.8), 0.15, mix(shade(color, 0.35), mix(color, Color32::WHITE, 0.55), lit));
}

/// Square trimmer pot: coloured body with a white cross-slotted rotor.
fn trim_pot(l: &Local, at: [f32; 2], s: f32) {
    let b = bx(at, s, s);
    l.shadow(b, 0.3, 0.8);
    l.shaded(b, 0.3, col(250, 156, 56), col(206, 104, 26));
    l.dome(at, s * 0.34, gray(250), gray(226), gray(186));
    let k = s * 0.22;
    l.seg([at[0] - k, at[1]], [at[0] + k, at[1]], l.px(0.22).max(1.0), gray(120));
    l.seg([at[0], at[1] - k], [at[0], at[1] + k], l.px(0.22).max(1.0), gray(120));
}

/// Wire-mesh front: horizontal and vertical chords of a circle.
fn mesh_grille(l: &Local, c: [f32; 2], r: f32, pitch: f32, color: Color32) {
    let mut t = -r + pitch * 0.5;
    while t < r {
        let half = (r * r - t * t).max(0.0).sqrt();
        l.seg([c[0] - half, c[1] + t], [c[0] + half, c[1] + t], 1.0, color);
        l.seg([c[0] + t, c[1] - half], [c[0] + t, c[1] + half], 1.0, color);
        t += pitch;
    }
}

/// Fresnel lens facets: two rings with staggered radial seams.
fn fresnel(l: &Local, c: [f32; 2], r: f32, color: Color32) {
    let wd = l.px(0.12).max(1.0);
    for k in [0.36f32, 0.68] {
        l.ring(c, r * k, wd, color);
    }
    for (count, r0, r1, phase) in [(6, 0.36, 0.68, 0.0), (12, 0.68, 0.97, 0.5)] {
        for i in 0..count {
            let a = (i as f32 + phase) / count as f32 * TAU;
            let at = |k: f32| [c[0] + a.cos() * r * k, c[1] + a.sin() * r * k];
            l.seg(at(r0), at(r1), wd, color);
        }
    }
}

/// Keeps the part of a convex local polygon with y <= `limit`.
fn clip_below(pts: &[[f32; 2]], limit: f32) -> Vec<[f32; 2]> {
    let mut out = Vec::with_capacity(pts.len() + 2);
    for (i, a) in pts.iter().enumerate() {
        let b = pts[(i + 1) % pts.len()];
        let (ina, inb) = (a[1] <= limit, b[1] <= limit);
        if ina {
            out.push(*a);
        }
        if ina != inb {
            let t = (limit - a[1]) / (b[1] - a[1]);
            out.push([a[0] + (b[0] - a[0]) * t, limit]);
        }
    }
    out
}

// ------------------------------------------------------------ local painter --

const TIN: Color32 = Color32::from_rgb(176, 178, 182);
const TIN_HI: Color32 = Color32::from_rgb(228, 230, 234);
const CHROME_HI: Color32 = Color32::from_rgb(238, 240, 243);
const CHROME_LO: Color32 = Color32::from_rgb(128, 132, 138);
const HOLE: Color32 = Color32::from_rgb(9, 9, 12);
const PLATING: Color32 = Color32::from_rgb(208, 178, 98);
const CAP_TAN: Color32 = Color32::from_rgb(176, 146, 102);
const SILK: Color32 = Color32::from_rgba_premultiplied(220, 220, 220, 220);
/// Direction toward the light in screen space (top, slightly left).
const LIGHT: Vec2 = Vec2::new(-0.55, -0.83);
/// Smallest silkscreen text worth drawing, in px.
const MIN_TEXT_PX: f32 = 6.5;

fn col(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

fn gray(v: u8) -> Color32 {
    Color32::from_gray(v)
}

/// Scales an opaque colour's RGB channels.
fn shade(c: Color32, f: f32) -> Color32 {
    let s = |v: u8| (f32::from(v) * f).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(s(c.r()), s(c.g()), s(c.b()))
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    a.lerp_to_gamma(b, t.clamp(0.0, 1.0))
}

/// Mostly additive light of colour `c` at strength `k`.
fn light(c: Color32, k: f32) -> Color32 {
    let k = k.clamp(0.0, 1.0);
    let s = |v: u8| (f32::from(v) * k) as u8;
    Color32::from_rgba_premultiplied(s(c.r()), s(c.g()), s(c.b()), (k * 90.0) as u8)
}

fn bx(at: [f32; 2], w: f32, h: f32) -> [f32; 4] {
    [at[0] - w * 0.5, at[1] - h * 0.5, at[0] + w * 0.5, at[1] + h * 0.5]
}

/// Colour at `p` of a top-to-bottom gradient over `rect`.
fn vgrad(rect: Rect, top: Color32, bottom: Color32, p: Pos2) -> Color32 {
    mix(top, bottom, (p.y - rect.min.y) / rect.height().max(1.0))
}

/// Paints in a part's local mm frame (unrotated, +y down); light, shadows and text stay screen-aligned.
struct Local<'a> {
    painter: &'a egui::Painter,
    origin: Pos2,
    scale: f32,
    rot: u16,
    feather: f32,
    /// Offset added to local points before rotation.
    pre: [f32; 2],
}

impl<'a> Local<'a> {
    fn new(painter: &'a egui::Painter, view: &View, origin: [f32; 2], rot: u16) -> Self {
        Local {
            painter,
            origin: view.to_screen(origin),
            scale: view.scale,
            rot,
            feather: 1.0 / painter.pixels_per_point().max(0.5),
            pre: [0.0, 0.0],
        }
    }

    /// Top-left-origin frame of a `size` board at `pos`, turned `rot` degrees about its centre.
    fn board(painter: &'a egui::Painter, view: &View, pos: [f32; 2], size: [f32; 2], rot: u16) -> Self {
        let mut l = Self::new(painter, view, [pos[0] + size[0] / 2.0, pos[1] + size[1] / 2.0], rot);
        l.pre = [-size[0] / 2.0, -size[1] / 2.0];
        l
    }

    fn pt(&self, p: [f32; 2]) -> Pos2 {
        let [x, y] = wirelab_core::geometry::rotate([p[0] + self.pre[0], p[1] + self.pre[1]], self.rot);
        Pos2::new(self.origin.x + x * self.scale, self.origin.y + y * self.scale)
    }

    fn dir(&self, v: [f32; 2]) -> Vec2 {
        let [x, y] = wirelab_core::geometry::rotate(v, self.rot);
        Vec2::new(x, y)
    }

    fn px(&self, mm: f32) -> f32 {
        mm * self.scale
    }

    fn rect(&self, b: [f32; 4]) -> Rect {
        Rect::from_two_pos(self.pt([b[0], b[1]]), self.pt([b[2], b[3]]))
    }

    fn corner(&self, r: f32) -> CornerRadius {
        CornerRadius::from(self.px(r).clamp(0.0, 255.0))
    }

    /// +1 when local +y faces the light, else -1.
    fn lit_y(&self) -> f32 {
        if self.dir([0.0, 1.0]).dot(LIGHT) > 0.0 { 1.0 } else { -1.0 }
    }

    /// +1 when local +x faces the light, else -1.
    fn lit_x(&self) -> f32 {
        if self.dir([1.0, 0.0]).dot(LIGHT) > 0.0 { 1.0 } else { -1.0 }
    }

    /// Local angle (radians) that points along screen angle `a`.
    fn screen_angle(&self, a: f32) -> f32 {
        a - f32::from(self.rot).to_radians()
    }

    fn shadow_offset(&self) -> Vec2 {
        Vec2::new(self.px(0.3).clamp(0.5, 3.0), self.px(0.6).clamp(1.0, 6.0))
    }

    fn fill(&self, b: [f32; 4], r: f32, color: Color32) {
        self.painter.rect_filled(self.rect(b), self.corner(r), color);
    }

    fn frame(&self, b: [f32; 4], r: f32, width: f32, color: Color32) {
        self.painter.rect_stroke(self.rect(b), self.corner(r), Stroke::new(width, color), StrokeKind::Inside);
    }

    /// Rounded box with a top-to-bottom screen gradient.
    fn shaded(&self, b: [f32; 4], r: f32, top: Color32, bottom: Color32) {
        self.shaded_in(b, r, top, bottom, self.rect(b));
    }

    /// Rounded box whose gradient runs over `span` instead of its own bounds.
    fn shaded_in(&self, b: [f32; 4], r: f32, top: Color32, bottom: Color32, span: Rect) {
        let rect = self.rect(b);
        let outline = rrect_outline(rect, self.px(r));
        fill_convex(self.painter, &outline, rect.center(), &[], self.feather, |p, _| vgrad(span, top, bottom, p));
    }

    /// Soft drop shadow under a rounded box.
    fn shadow(&self, b: [f32; 4], r: f32, strength: f32) {
        let rect = self.rect(b).translate(self.shadow_offset());
        let alpha = (110.0 * strength).min(255.0) as u8;
        self.painter.add(
            RectShape::filled(rect, self.corner(r), Color32::from_black_alpha(alpha))
                .with_blur_width(self.px(1.3).clamp(2.0, 14.0)),
        );
    }

    fn disc(&self, at: [f32; 2], r: f32, color: Color32) {
        let rr = self.px(r);
        if rr >= 0.4 {
            self.painter.circle_filled(self.pt(at), rr, color);
        }
    }

    fn ring(&self, at: [f32; 2], r: f32, width: f32, color: Color32) {
        self.painter.circle_stroke(self.pt(at), self.px(r), Stroke::new(width, color));
    }

    /// Soft round drop shadow.
    fn disc_shadow(&self, at: [f32; 2], r: f32, strength: f32) {
        let blur = self.px(1.2).clamp(2.0, 12.0);
        let c = self.pt(at) + self.shadow_offset();
        let rr = self.px(r) + blur * 0.5;
        let inner = (1.0 - blur / rr).clamp(0.0, 0.95);
        let alpha = (110.0 * strength).min(255.0) as u8;
        fill_convex(self.painter, &circle_outline(c, rr), c, &[inner], 0.0, |_, t| {
            if t <= inner { Color32::from_black_alpha(alpha) } else { Color32::TRANSPARENT }
        });
    }

    /// Sphere-shaded disc: `hub` toward the light, `mid` halfway, `rim` at the edge.
    fn dome(&self, at: [f32; 2], r: f32, hub: Color32, mid: Color32, rim: Color32) {
        dome_px(self.painter, self.feather, self.pt(at), self.px(r), hub, mid, rim);
    }

    /// Radial light falloff of colour `color`.
    fn glow(&self, at: [f32; 2], r: f32, color: Color32, k: f32) {
        glow_px(self.painter, self.pt(at), self.px(r), color, k);
    }

    /// Specular highlight on the light side of a dome of radius `r`.
    fn specular(&self, at: [f32; 2], r: f32, alpha: u8) {
        let rr = self.px(r);
        if rr < 3.0 {
            return;
        }
        let c = self.pt(at) + Vec2::new(-0.36, -0.42) * rr;
        self.painter.add(
            EllipseShape::filled(c, Vec2::new(0.34, 0.19) * rr, Color32::from_white_alpha(alpha))
                .with_angle(-0.7),
        );
    }

    fn poly(&self, pts: &[[f32; 2]], color: Color32) {
        let pts: Vec<Pos2> = pts.iter().map(|q| self.pt(*q)).collect();
        self.painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
    }

    /// Convex polygon with a top-to-bottom screen gradient.
    fn poly_shaded(&self, pts: &[[f32; 2]], top: Color32, bottom: Color32) {
        let screen: Vec<Pos2> = pts.iter().map(|q| self.pt(*q)).collect();
        let span = Rect::from_points(&screen);
        self.poly_grad(pts, top, bottom, span);
    }

    /// Convex polygon whose gradient spans `span`.
    fn poly_grad(&self, pts: &[[f32; 2]], top: Color32, bottom: Color32, span: Rect) {
        let screen: Vec<Pos2> = pts.iter().map(|q| self.pt(*q)).collect();
        let hub = screen.iter().fold(Vec2::ZERO, |a, p| a + p.to_vec2()) / screen.len().max(1) as f32;
        fill_convex(self.painter, &screen, hub.to_pos2(), &[], self.feather, |p, _| vgrad(span, top, bottom, p));
    }

    fn poly_shadow(&self, pts: &[[f32; 2]], strength: f32) {
        let off = self.shadow_offset();
        let pts: Vec<Pos2> = pts.iter().map(|q| self.pt(*q) + off).collect();
        let alpha = (90.0 * strength).min(255.0) as u8;
        self.painter.add(egui::Shape::convex_polygon(pts, Color32::from_black_alpha(alpha), Stroke::NONE));
    }

    fn poly_outline(&self, pts: &[[f32; 2]], width: f32, color: Color32) {
        let pts: Vec<Pos2> = pts.iter().map(|q| self.pt(*q)).collect();
        self.painter.add(egui::Shape::closed_line(pts, Stroke::new(width, color)));
    }

    fn path(&self, pts: &[[f32; 2]], width: f32, color: Color32) {
        let pts: Vec<Pos2> = pts.iter().map(|q| self.pt(*q)).collect();
        self.painter.add(egui::Shape::line(pts, Stroke::new(width, color)));
    }

    fn seg(&self, a: [f32; 2], b: [f32; 2], width: f32, color: Color32) {
        self.painter.line_segment([self.pt(a), self.pt(b)], Stroke::new(width, color));
    }

    /// Arc of radius `r` between angles `a0` and `a1` (radians, local frame).
    fn arc(&self, at: [f32; 2], r: f32, a0: f32, a1: f32, width: f32, color: Color32) {
        let n = 16;
        let pts: Vec<Pos2> = (0..=n)
            .map(|i| {
                let a = a0 + (a1 - a0) * i as f32 / n as f32;
                self.pt([at[0] + r * a.cos(), at[1] + r * a.sin()])
            })
            .collect();
        self.painter.add(egui::Shape::line(pts, Stroke::new(width, color)));
    }

    /// Tinned wire lead `d` mm thick.
    fn lead(&self, a: [f32; 2], b: [f32; 2], d: f32) {
        let w = self.px(d).max(1.2);
        self.seg(a, b, w, TIN);
        if w >= 2.5 {
            self.seg(a, b, w * 0.4, TIN_HI);
        }
    }

    /// Upright text centred in a local box and shrunk to its width; skipped when too small to read.
    fn text(&self, b: [f32; 4], text: &str, size: f32, color: Color32) {
        let px = self.px(size);
        if px < MIN_TEXT_PX {
            return;
        }
        let rect = self.rect(b);
        let mut galley = self.painter.layout_no_wrap(text.to_owned(), FontId::monospace(px), color);
        let width = galley.size().x;
        if width > rect.width() {
            let fit = px * rect.width() / width;
            if fit < MIN_TEXT_PX {
                return;
            }
            galley = self.painter.layout_no_wrap(text.to_owned(), FontId::monospace(fit), color);
        }
        self.painter.galley(rect.center() - galley.size() * 0.5, galley, color);
    }
}

/// Sphere-shaded disc in screen space.
fn dome_px(painter: &egui::Painter, feather: f32, c: Pos2, r: f32, hub: Color32, mid: Color32, rim: Color32) {
    if r < 0.4 {
        return;
    }
    let h = c + LIGHT * (r * 0.42);
    fill_convex(painter, &circle_outline(c, r), h, &[0.55], feather, |_, t| {
        if t <= 0.55 { mix(hub, mid, t / 0.55) } else { mix(mid, rim, (t - 0.55) / 0.45) }
    });
}

/// Radial light falloff in screen space.
fn glow_px(painter: &egui::Painter, c: Pos2, r: f32, color: Color32, k: f32) {
    fill_convex(painter, &circle_outline(c, r), c, &[0.25, 0.55], 0.0, |_, t| light(color, k * (1.0 - t).powi(2)));
}

fn circle_outline(c: Pos2, r: f32) -> Vec<Pos2> {
    let n = ((r * 0.9) as usize).clamp(12, 96);
    (0..n)
        .map(|i| {
            let a = i as f32 / n as f32 * TAU;
            c + Vec2::new(a.cos(), a.sin()) * r
        })
        .collect()
}

fn rrect_outline(rect: Rect, rad: f32) -> Vec<Pos2> {
    let rad = rad.min(rect.width() * 0.5).min(rect.height() * 0.5).max(0.0);
    if rad < 0.75 {
        return vec![rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()];
    }
    let k = ((rad * 0.45).ceil() as usize).clamp(2, 10);
    let corners = [
        (rect.left_top() + Vec2::splat(rad), PI),
        (rect.right_top() + Vec2::new(-rad, rad), 1.5 * PI),
        (rect.right_bottom() - Vec2::splat(rad), 0.0),
        (rect.left_bottom() + Vec2::new(rad, -rad), FRAC_PI_2),
    ];
    let mut pts = Vec::with_capacity(4 * (k + 1));
    for (c, a0) in corners {
        for i in 0..=k {
            let a = a0 + FRAC_PI_2 * i as f32 / k as f32;
            pts.push(c + Vec2::new(a.cos(), a.sin()) * rad);
        }
    }
    pts
}

/// Fills a convex outline with colour rings from `hub` (t = 0) to the edge (t = 1), feathered by `feather` px.
fn fill_convex(
    painter: &egui::Painter,
    outline: &[Pos2],
    hub: Pos2,
    rings: &[f32],
    feather: f32,
    color: impl Fn(Pos2, f32) -> Color32,
) {
    let n = outline.len();
    if n < 3 {
        return;
    }
    let area: f32 = outline
        .iter()
        .zip(outline.iter().cycle().skip(1))
        .map(|(a, b)| a.x * b.y - b.x * a.y)
        .sum();
    let sign = if area >= 0.0 { 1.0 } else { -1.0 };
    let normals: Vec<Vec2> = outline
        .iter()
        .zip(outline.iter().cycle().skip(1))
        .map(|(a, b)| {
            let d = *b - *a;
            let v = Vec2::new(d.y, -d.x) * sign;
            if v.length_sq() > 1e-9 { v.normalized() } else { Vec2::ZERO }
        })
        .collect();
    let miter = |i: usize| {
        let m = (normals[(i + n - 1) % n] + normals[i]) * 0.5;
        let l2 = m.length_sq();
        if l2 > 0.1 { m / l2 } else { m }
    };
    let mut mesh = Mesh::default();
    mesh.reserve_vertices(n * (rings.len() + 2) + 1);
    mesh.colored_vertex(hub, color(hub, 0.0));
    let mut starts: Vec<u32> = Vec::with_capacity(rings.len() + 1);
    for &t in rings {
        starts.push(mesh.vertices.len() as u32);
        for p in outline {
            let q = hub + (*p - hub) * t;
            mesh.colored_vertex(q, color(q, t));
        }
    }
    let rim = mesh.vertices.len() as u32;
    starts.push(rim);
    let half = feather * 0.5;
    for (i, p) in outline.iter().enumerate() {
        mesh.colored_vertex(*p - miter(i) * half, color(*p, 1.0));
    }
    let n32 = n as u32;
    for i in 0..n32 {
        mesh.add_triangle(0, starts[0] + i, starts[0] + (i + 1) % n32);
    }
    for pair in starts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        for i in 0..n32 {
            let j = (i + 1) % n32;
            mesh.add_triangle(a + i, a + j, b + j);
            mesh.add_triangle(a + i, b + j, b + i);
        }
    }
    if feather > 0.0 {
        let outer = mesh.vertices.len() as u32;
        for (i, p) in outline.iter().enumerate() {
            mesh.colored_vertex(*p + miter(i) * half, Color32::TRANSPARENT);
        }
        for i in 0..n32 {
            let j = (i + 1) % n32;
            mesh.add_triangle(rim + i, rim + j, outer + j);
            mesh.add_triangle(rim + i, outer + j, outer + i);
        }
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// Band colours by digit, matching the palette icons.
const BAND: [Color32; 10] = [
    Color32::from_rgb(24, 24, 24),
    Color32::from_rgb(130, 72, 30),
    Color32::from_rgb(220, 40, 40),
    Color32::from_rgb(240, 130, 20),
    Color32::from_rgb(240, 220, 40),
    Color32::from_rgb(40, 170, 70),
    Color32::from_rgb(40, 90, 220),
    Color32::from_rgb(150, 70, 200),
    Color32::from_rgb(128, 128, 128),
    Color32::from_rgb(250, 250, 250),
];
const BAND_GOLD: Color32 = Color32::from_rgb(212, 175, 55);
const BAND_SILVER: Color32 = Color32::from_rgb(192, 192, 192);

/// Four-band colour code: two digits, multiplier, gold (5 %) tolerance.
fn resistor_bands(ohms: f32) -> Option<[Color32; 4]> {
    let ohms = f64::from(ohms);
    if !ohms.is_finite() || ohms < 0.1 {
        return None;
    }
    let mut exp = ohms.log10().floor() as i32 - 1;
    let mut digits = (ohms / 10f64.powi(exp)).round() as u32;
    if digits >= 100 {
        digits /= 10;
        exp += 1;
    }
    let multiplier = match exp {
        -2 => BAND_SILVER,
        -1 => BAND_GOLD,
        0..=9 => BAND[exp as usize],
        _ => return None,
    };
    Some([BAND[(digits / 10) as usize], BAND[(digits % 10) as usize], multiplier, BAND_GOLD])
}

pub use wirelab_core::geometry::Route;

/// Orthogonal wire path between two screen points, honoring exit stubs,
/// with rounded corners. Delegates to the shared core geometry.
pub fn wire_path(a: Pos2, b: Pos2, route: Route) -> Vec<Pos2> {
    wirelab_core::geometry::wire_path([a.x, a.y], [b.x, b.y], &route)
        .into_iter()
        .map(|p| Pos2::new(p[0], p[1]))
        .collect()
}

/// Plain midpoint route, for previews with no endpoint context.
pub fn wire_points(a: Pos2, b: Pos2) -> Vec<Pos2> {
    wire_path(a, b, Route::default())
}

#[allow(clippy::too_many_arguments)]
pub fn draw_wire(
    painter: &egui::Painter,
    pts: &[Pos2],
    color: [u8; 3],
    selected: bool,
    accent: Color32,
    flow: Color32,
    live_mv: Option<f32>,
    time: f64,
) {
    let pts = pts.to_vec();
    let col = Color32::from_rgb(color[0], color[1], color[2]);
    if selected {
        painter.add(egui::Shape::line(pts.clone(), Stroke::new(6.0, accent)));
    }
    let energy = live_mv.map(|mv| (mv / 3300.0).clamp(0.0, 1.0)).unwrap_or(0.0);
    if energy > 0.08 {
        let glow = Color32::from_rgba_unmultiplied(
            flow.r(),
            flow.g(),
            flow.b(),
            (30.0 + energy * 60.0) as u8,
        );
        painter.add(egui::Shape::line(pts.clone(), Stroke::new(4.5, glow)));
    }
    painter.add(egui::Shape::line(pts.clone(), Stroke::new(2.5, col)));
    if energy > 0.08 {
        draw_tracers(painter, &pts, flow, energy, time);
    }
}

/// Marching dots along an energized wire; speed scales with voltage.
fn draw_tracers(painter: &egui::Painter, pts: &[Pos2], flow: Color32, energy: f32, time: f64) {
    let mut cum = Vec::with_capacity(pts.len());
    let mut total = 0.0f32;
    cum.push(0.0);
    for seg in pts.windows(2) {
        total += (seg[1] - seg[0]).length();
        cum.push(total);
    }
    if total < 8.0 {
        return;
    }
    let spacing = 26.0f32;
    let speed = 25.0 + energy * 70.0;
    let phase = (time as f32 * speed) % spacing;
    let halo = Color32::from_rgba_unmultiplied(flow.r(), flow.g(), flow.b(), 70);
    let mut d = phase;
    while d < total {
        let i = cum.partition_point(|&c| c <= d).min(pts.len() - 1);
        let (c0, c1) = (cum[i - 1], cum[i]);
        let t = if c1 > c0 { (d - c0) / (c1 - c0) } else { 0.0 };
        let p = pts[i - 1] + (pts[i] - pts[i - 1]) * t;
        painter.circle_filled(p, 3.4, halo);
        painter.circle_filled(p, 1.8, flow);
        d += spacing;
    }
}

/// Distance from a point to the wire's polyline.
pub fn wire_hit_distance(pts: &[Pos2], p: Pos2) -> f32 {
    let mut best = f32::MAX;
    for seg in pts.windows(2) {
        let (s, e) = (seg[0], seg[1]);
        let se = e - s;
        let len2 = se.length_sq().max(0.0001);
        let t = ((p - s).dot(se) / len2).clamp(0.0, 1.0);
        let proj = s + se * t;
        best = best.min((p - proj).length());
    }
    best
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use wirelab_core::library::Library;

    fn shipped() -> Library {
        let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        Library::load(&assets.join("boards"), &assets.join("components")).expect("shipped library loads")
    }

    #[test]
    fn every_shipped_part_has_dedicated_art() {
        for def in shipped().components.values() {
            if def.visual.shape == Shape::Dot {
                continue;
            }
            let art = art_of(def);
            assert!(!matches!(art, Art::Module | Art::Chip), "{} falls back to generic art", def.id);
        }
    }

    #[test]
    fn buzzers_split_by_drive() {
        let lib = shipped();
        assert_eq!(art_of(lib.component("buzzer-active").unwrap()), Art::Buzzer { active: true });
        assert_eq!(art_of(lib.component("buzzer-passive").unwrap()), Art::Buzzer { active: false });
    }

    #[test]
    fn resistor_band_colours() {
        let (red, violet, yellow, brown, black) = (BAND[2], BAND[7], BAND[4], BAND[1], BAND[0]);
        assert_eq!(resistor_bands(220.0), Some([red, red, brown, BAND_GOLD]));
        assert_eq!(resistor_bands(4_700.0), Some([yellow, violet, red, BAND_GOLD]));
        assert_eq!(resistor_bands(100.0), Some([brown, black, brown, BAND_GOLD]));
        assert_eq!(resistor_bands(4.7), Some([yellow, violet, BAND_GOLD, BAND_GOLD]));
        assert_eq!(resistor_bands(0.0), None);
    }

    #[test]
    fn local_frame_follows_core_rotation() {
        let ctx = egui::Context::default();
        let painter = egui::Painter::new(ctx, egui::LayerId::background(), Rect::EVERYTHING);
        let view = View { origin: Pos2::new(10.0, 20.0), scale: 4.0 };
        for rot in [0, 90, 180, 270] {
            let l = Local::new(&painter, &view, [5.0, 6.0], rot);
            let [x, y] = wirelab_core::geometry::rotate([3.0, -2.0], rot);
            let expect = view.to_screen([5.0 + x, 6.0 + y]);
            assert!((l.pt([3.0, -2.0]) - expect).length() < 1e-4, "rotation {rot}");
        }
    }
}
