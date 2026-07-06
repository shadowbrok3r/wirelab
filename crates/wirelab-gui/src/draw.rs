//! Pure drawing helpers: board, components, wires, terminal geometry.

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Stroke, StrokeKind, Vec2};
use wirelab_core::board::{BoardPin, BoardProfile, PinKind, Side};
use wirelab_core::component::{CompState, ComponentDef, Shape, SimModel, TerminalRole, VisualState};
use wirelab_core::circuit::PlacedComponent;
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

fn rotate(v: Vec2, deg: u16) -> Vec2 {
    match deg % 360 {
        90 => Vec2::new(-v.y, v.x),
        180 => Vec2::new(-v.x, -v.y),
        270 => Vec2::new(v.y, -v.x),
        _ => v,
    }
}

/// Local terminal offsets in mm, by terminal index, before rotation.
pub fn terminal_offsets(def: &ComponentDef) -> Vec<Vec2> {
    let w = def.visual.width_mm;
    let h = def.visual.height_mm;
    let n = def.terminals.len();
    match n {
        // Single-terminal parts (junction dots) attach at the centre.
        1 => vec![Vec2::ZERO],
        2 => vec![Vec2::new(-w / 2.0, 0.0), Vec2::new(w / 2.0, 0.0)],
        3 => vec![
            Vec2::new(-w / 2.0, 0.0),
            Vec2::new(w / 2.0, 0.0),
            Vec2::new(0.0, h / 2.0),
        ],
        _ => {
            let per_side = n.div_ceil(2);
            (0..n)
                .map(|i| {
                    let (side, j, m) = if i < per_side {
                        (-1.0, i, per_side)
                    } else {
                        (1.0, i - per_side, n - per_side)
                    };
                    let t = if m <= 1 { 0.5 } else { j as f32 / (m as f32 - 1.0) };
                    Vec2::new(side * w / 2.0, (t - 0.5) * h * 0.8)
                })
                .collect()
        }
    }
}

/// World-space terminal position for a placed component.
pub fn terminal_world_pos(comp: &PlacedComponent, def: &ComponentDef, index: usize) -> [f32; 2] {
    let off = terminal_offsets(def)
        .get(index)
        .copied()
        .unwrap_or(Vec2::ZERO);
    let r = rotate(off, comp.rotation);
    [comp.pos[0] + r.x, comp.pos[1] + r.y]
}

/// Header pin position in world mm; USB end is the bottom of the board.
pub fn board_pin_world_pos(board: &BoardProfile, pin: &BoardPin, board_pos: [f32; 2]) -> [f32; 2] {
    let count = |side: Side| {
        board
            .pins
            .iter()
            .filter(|p| p.side == side)
            .map(|p| p.index)
            .max()
            .unwrap_or(0)
            + 1
    };
    let margin = 6.0;
    match pin.side {
        Side::Left | Side::Right => {
            let n = count(pin.side) as f32;
            let usable = board.height_mm - 2.0 * margin;
            let pitch = if n > 1.0 { usable / (n - 1.0) } else { 0.0 };
            let y = board_pos[1] + board.height_mm - margin - pin.index as f32 * pitch;
            let x = if pin.side == Side::Left {
                board_pos[0] + 1.5
            } else {
                board_pos[0] + board.width_mm - 1.5
            };
            [x, y]
        }
        Side::Top | Side::Bottom => {
            let n = count(pin.side) as f32;
            let usable = board.width_mm - 2.0 * margin;
            let pitch = if n > 1.0 { usable / (n - 1.0) } else { 0.0 };
            let x = board_pos[0] + margin + pin.index as f32 * pitch;
            let y = if pin.side == Side::Top {
                board_pos[1] + 1.5
            } else {
                board_pos[1] + board.height_mm - 1.5
            };
            [x, y]
        }
    }
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
) -> Vec<(BoardControl, Rect)> {
    let at = |x_frac: f32, y_from_bottom: f32, w: f32, h: f32| {
        Rect::from_min_size(
            Pos2::new(
                board_pos[0] + board.width_mm * x_frac - w / 2.0,
                board_pos[1] + board.height_mm - y_from_bottom,
            ),
            Vec2::new(w, h),
        )
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
    hovered_pin: Option<&str>,
    live: Option<&dyn Fn(u8) -> PinLive>,
    group_keys: &[String],
    group_color: Color32,
    hovered_feature: Option<BoardControl>,
    rgb: Option<[u8; 3]>,
) {
    let tl = view.to_screen(board_pos);
    let br = view.to_screen([board_pos[0] + board.width_mm, board_pos[1] + board.height_mm]);
    let rect = Rect::from_min_max(tl, br);
    painter.rect_filled(rect, CornerRadius::same(6), Color32::from_rgb(18, 74, 48));
    painter.rect_stroke(
        rect,
        CornerRadius::same(6),
        Stroke::new(1.5, Color32::from_rgb(10, 40, 26)),
        StrokeKind::Outside,
    );

    // Module can + USB stub.
    let module = Rect::from_min_size(
        tl + Vec2::new(view.px(board.width_mm * 0.22), view.px(3.0)),
        Vec2::new(view.px(board.width_mm * 0.56), view.px(board.height_mm * 0.38)),
    );
    painter.rect_filled(module, CornerRadius::same(3), Color32::from_gray(160));
    painter.rect_filled(
        Rect::from_min_size(
            module.min + Vec2::new(view.px(1.0), view.px(1.0)),
            module.size() - Vec2::splat(view.px(2.0)),
        ),
        CornerRadius::same(2),
        Color32::from_gray(60),
    );
    let usb_w = view.px(9.0);
    let usb = Rect::from_min_size(
        Pos2::new(rect.center().x - usb_w / 2.0, rect.max.y - view.px(4.5)),
        Vec2::new(usb_w, view.px(5.5)),
    );
    painter.rect_filled(usb, CornerRadius::same(2), Color32::from_gray(190));

    painter.text(
        Pos2::new(rect.center().x, rect.min.y + view.px(board.height_mm * 0.55)),
        Align2::CENTER_CENTER,
        &board.name,
        FontId::proportional((view.px(2.2)).clamp(8.0, 16.0)),
        Color32::from_gray(220),
    );

    let font = FontId::monospace((view.px(1.9)).clamp(7.0, 13.0));
    for pin in &board.pins {
        let pos = view.to_screen(board_pin_world_pos(board, pin, board_pos));
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
        // Label toward the board interior.
        let (anchor, dx) = match pin.side {
            Side::Left => (Align2::LEFT_CENTER, r + 3.0),
            Side::Right => (Align2::RIGHT_CENTER, -r - 3.0),
            _ => (Align2::CENTER_TOP, 0.0),
        };
        if view.scale > 2.0 {
            painter.text(
                pos + Vec2::new(dx, 0.0),
                anchor,
                &pin.label,
                font.clone(),
                Color32::from_gray(210),
            );
        }
    }

    // On-board extras: RGB LED, RESET / BOOT buttons.
    for (kind, world_rect) in board_feature_rects(board, board_pos) {
        let r = Rect::from_min_max(
            view.to_screen([world_rect.min.x, world_rect.min.y]),
            view.to_screen([world_rect.max.x, world_rect.max.y]),
        );
        let hovered = hovered_feature == Some(kind);
        match kind {
            BoardControl::Reset | BoardControl::Boot => {
                let fill = if hovered { Color32::from_gray(105) } else { Color32::from_gray(70) };
                painter.rect_filled(r, CornerRadius::same(2), fill);
                painter.rect_stroke(
                    r,
                    CornerRadius::same(2),
                    Stroke::new(1.0, Color32::from_gray(if hovered { 220 } else { 140 })),
                    StrokeKind::Inside,
                );
                painter.circle_filled(
                    r.center(),
                    (r.height() * 0.28).clamp(1.5, 5.0),
                    Color32::from_gray(30),
                );
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
                if let Some(c) = color {
                    painter.circle_filled(
                        r.center(),
                        r.width() * 1.4,
                        Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 90),
                    );
                }
                painter.rect_filled(r, CornerRadius::same(1), Color32::from_gray(235));
                painter.circle_filled(
                    r.center(),
                    r.width() * 0.28,
                    color.unwrap_or(Color32::from_gray(170)),
                );
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
    let w = view.px(def.visual.width_mm);
    let h = view.px(def.visual.height_mm);
    let body = Rect::from_center_size(center, Vec2::new(w, h));
    let base = Color32::from_rgb(def.visual.color[0], def.visual.color[1], def.visual.color[2]);

    if selected {
        painter.rect_stroke(
            body.expand(4.0),
            CornerRadius::same(4),
            Stroke::new(1.5, accent),
            StrokeKind::Outside,
        );
    }
    if let Some(glow) = active {
        let on = match vis {
            Some(VisualState::LedBrightness(b)) => b > 0.01,
            Some(VisualState::RelayClosed(c)) => c,
            Some(VisualState::BuzzerOn { .. }) => true,
            _ => false,
        } || matches!(
            comp.state,
            CompState::Button { pressed: true } | CompState::Toggle { on: true }
        );
        if on {
            painter.rect_stroke(
                body.expand(2.0),
                CornerRadius::same(3),
                Stroke::new(1.5, glow),
                StrokeKind::Outside,
            );
        }
    }

    // Junction dots are just a node: no body, pads or name.
    if def.visual.shape == Shape::Dot {
        let r = (w.min(h) * 0.5).clamp(3.0, 7.0);
        painter.circle_filled(center, r, base);
        painter.circle_stroke(center, r, Stroke::new(1.2, base.linear_multiply(1.6)));
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

    // Terminal pads with role labels placed outward from the body.
    let label_font = FontId::monospace((view.px(1.6)).clamp(7.0, 11.0));
    for (i, t) in def.terminals.iter().enumerate() {
        let p = view.to_screen(terminal_world_pos(comp, def, i));
        let r = view.px(0.9).clamp(2.5, 6.0);
        painter.circle_filled(p, r, Color32::from_gray(200));
        painter.circle_stroke(p, r, Stroke::new(1.0, Color32::from_gray(90)));
        if view.scale > 2.2 {
            let dir = p - center;
            let dir = if dir.length() > 0.5 { dir.normalized() } else { Vec2::new(0.0, 1.0) };
            let text_pos = p + dir * (r + view.px(1.4)).max(8.0);
            // Bus-heavy modules (SPI displays, readers) label by pin id.
            let label = if def.terminals.len() > 3 {
                t.id.to_uppercase()
            } else {
                terminal_short_label(t.role).to_string()
            };
            painter.text(
                text_pos,
                Align2::CENTER_CENTER,
                label,
                label_font.clone(),
                Color32::from_gray(185),
            );
        }
    }

    match def.visual.shape {
        Shape::Led => {
            let r = w.min(h) * 0.42;
            let brightness = match vis {
                Some(VisualState::LedBrightness(b)) => b,
                _ => 0.0,
            };
            if brightness > 0.01 {
                let glow = (brightness * 150.0) as u8;
                painter.circle_filled(
                    center,
                    r * (1.6 + 0.7 * brightness),
                    Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), glow / 2),
                );
                painter.circle_filled(
                    center,
                    r * 1.15,
                    Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), glow),
                );
            }
            let body_col = if brightness > 0.05 {
                base
            } else {
                base.linear_multiply(0.45)
            };
            painter.circle_filled(center, r, body_col);
            painter.circle_stroke(center, r, Stroke::new(1.5, base.linear_multiply(0.7)));
            // Flat edge marks the cathode side.
            let flat = rotate(Vec2::new(w * 0.42 / view.scale, 0.0), comp.rotation);
            painter.line_segment(
                [
                    center + Vec2::new(view.px(flat.x), view.px(flat.y))
                        + rotate(Vec2::new(0.0, -r * 0.6 / view.scale), comp.rotation) * view.scale,
                    center + Vec2::new(view.px(flat.x), view.px(flat.y))
                        + rotate(Vec2::new(0.0, r * 0.6 / view.scale), comp.rotation) * view.scale,
                ],
                Stroke::new(2.0, Color32::from_gray(220)),
            );
        }
        Shape::PushButton => {
            let pressed = matches!(
                comp.state,
                wirelab_core::component::CompState::Button { pressed: true }
            );
            painter.rect_filled(body, CornerRadius::same(3), Color32::from_gray(45));
            let cap_r = w.min(h) * (if pressed { 0.26 } else { 0.32 });
            painter.circle_filled(
                center,
                cap_r,
                if pressed { Color32::from_rgb(150, 40, 40) } else { Color32::from_rgb(200, 60, 60) },
            );
            painter.circle_stroke(center, cap_r, Stroke::new(1.0, Color32::from_gray(20)));
        }
        Shape::ToggleSwitch | Shape::SlideSwitch => {
            let on = matches!(
                comp.state,
                wirelab_core::component::CompState::Toggle { on: true }
            );
            painter.rect_filled(body, CornerRadius::same(3), Color32::from_gray(70));
            let knob_off = rotate(Vec2::new(if on { w * 0.2 } else { -w * 0.2 }, 0.0), comp.rotation);
            painter.rect_filled(
                Rect::from_center_size(center + knob_off, Vec2::new(w * 0.3, h * 0.6)),
                CornerRadius::same(2),
                if on { Color32::from_rgb(90, 200, 120) } else { Color32::from_gray(160) },
            );
        }
        Shape::Resistor => {
            painter.rect_filled(body, CornerRadius::same(3), base);
            let ohms = comp
                .props
                .get("ohms")
                .copied()
                .unwrap_or(match def.sim {
                    SimModel::Resistor { ohms } => f64::from(ohms),
                    _ => 1000.0,
                }) as f32;
            for (i, c) in resistor_bands(ohms).iter().enumerate() {
                let t = -0.25 + 0.25 * i as f32;
                let off = rotate(Vec2::new(w * t / view.scale, 0.0), comp.rotation) * view.scale;
                painter.rect_filled(
                    Rect::from_center_size(center + off, Vec2::new(w * 0.08, h * 0.9)),
                    CornerRadius::ZERO,
                    *c,
                );
            }
        }
        Shape::Potentiometer => {
            painter.circle_filled(center, w.min(h) * 0.5, base);
            let frac = match comp.state {
                wirelab_core::component::CompState::Fraction { value } => value,
                _ => 0.5,
            };
            let angle = (-135.0 + 270.0 * frac).to_radians();
            let dir = Vec2::new(angle.sin(), -angle.cos());
            painter.line_segment(
                [center, center + dir * w.min(h) * 0.42],
                Stroke::new(2.5, Color32::WHITE),
            );
        }
        Shape::Photoresistor => {
            painter.circle_filled(center, w.min(h) * 0.45, Color32::from_rgb(220, 170, 90));
            let r = w.min(h) * 0.45;
            for i in 0..3 {
                let y = -r * 0.5 + r * 0.5 * i as f32;
                painter.line_segment(
                    [
                        center + Vec2::new(-r * 0.7, y),
                        center + Vec2::new(r * 0.7, y + r * 0.25),
                    ],
                    Stroke::new(1.5, Color32::from_rgb(120, 60, 30)),
                );
            }
        }
        Shape::Buzzer => {
            painter.circle_filled(center, w.min(h) * 0.48, Color32::from_gray(30));
            painter.circle_filled(center, w.min(h) * 0.12, Color32::from_gray(10));
            if let Some(VisualState::BuzzerOn { .. }) = vis {
                let phase = (time * 8.0).fract() as f32;
                for k in 0..2 {
                    let rr = w.min(h) * (0.55 + 0.25 * (phase + k as f32 * 0.5).fract());
                    painter.circle_stroke(
                        center,
                        rr,
                        Stroke::new(1.5, Color32::from_rgba_unmultiplied(255, 220, 90, 140)),
                    );
                }
            }
        }
        Shape::Servo => {
            painter.rect_filled(body, CornerRadius::same(3), base);
            let hub = center + rotate(Vec2::new(-w * 0.18 / view.scale, 0.0), comp.rotation) * view.scale;
            painter.circle_filled(hub, h * 0.28, Color32::from_gray(220));
            let angle = match vis {
                Some(VisualState::ServoAngle(a)) => a,
                _ => 90.0,
            };
            let rad = (angle - 90.0).to_radians();
            let dir = rotate(Vec2::new(rad.sin(), -rad.cos()), comp.rotation);
            painter.line_segment([hub, hub + dir * h * 0.75], Stroke::new(3.0, Color32::WHITE));
            painter.circle_filled(hub, h * 0.08, Color32::from_gray(60));
        }
        Shape::Relay => {
            painter.rect_filled(body, CornerRadius::same(3), Color32::from_rgb(30, 60, 140));
            let cube = Rect::from_center_size(
                center - Vec2::new(0.0, h * 0.12),
                Vec2::new(w * 0.62, h * 0.45),
            );
            painter.rect_filled(cube, CornerRadius::same(2), Color32::from_rgb(60, 120, 220));
            let closed = matches!(vis, Some(VisualState::RelayClosed(true)));
            painter.circle_filled(
                center + Vec2::new(w * 0.3, h * 0.3),
                3.5,
                if closed { Color32::from_rgb(120, 255, 120) } else { Color32::from_gray(70) },
            );
        }
        Shape::SensorModule | Shape::Chip | Shape::Generic | Shape::Dot => {
            painter.rect_filled(body, CornerRadius::same(3), base.linear_multiply(0.8));
            painter.rect_stroke(
                body,
                CornerRadius::same(3),
                Stroke::new(1.0, base),
                StrokeKind::Inside,
            );
            let on = matches!(
                comp.state,
                wirelab_core::component::CompState::Toggle { on: true }
            );
            painter.circle_filled(
                body.min + Vec2::new(w * 0.12, h * 0.15),
                3.0,
                if on { Color32::from_rgb(120, 255, 120) } else { Color32::from_gray(60) },
            );
        }
    }

    // Scripted components carry a scroll tag in the corner.
    if comp.script.is_some() && view.scale > 1.2 && def.visual.shape != Shape::Dot {
        let tag = Pos2::new(body.max.x + 3.0, body.min.y - 3.0);
        painter.circle_filled(tag, 7.0, Color32::from_rgb(52, 42, 64));
        painter.circle_stroke(tag, 7.0, Stroke::new(1.0, accent));
        painter.text(
            tag,
            Align2::CENTER_CENTER,
            egui_phosphor::regular::SCROLL,
            FontId::proportional(9.0),
            Color32::from_gray(220),
        );
    }

    // Name label above the body, clear of terminal pads and their labels.
    let label = if comp.label.is_empty() { &def.name } else { &comp.label };
    if view.scale > 1.6 {
        painter.text(
            Pos2::new(center.x, body.min.y - 5.0),
            Align2::CENTER_BOTTOM,
            label,
            FontId::proportional((view.px(1.8)).clamp(8.0, 13.0)),
            Color32::from_gray(200),
        );
    }
}

/// Standard two-digit + multiplier color code for a resistance.
fn resistor_bands(ohms: f32) -> [Color32; 3] {
    let digit = |d: i32| match d {
        -1 => Color32::from_rgb(190, 150, 60),
        0 => Color32::from_gray(25),
        1 => Color32::from_rgb(120, 70, 30),
        2 => Color32::from_rgb(200, 45, 40),
        3 => Color32::from_rgb(230, 120, 30),
        4 => Color32::from_rgb(225, 195, 50),
        5 => Color32::from_rgb(60, 160, 70),
        6 => Color32::from_rgb(65, 95, 220),
        7 => Color32::from_rgb(150, 70, 200),
        8 => Color32::from_gray(130),
        _ => Color32::from_gray(240),
    };
    let mut v = ohms.max(0.1);
    let mut mag = 0i32;
    while v >= 100.0 {
        v /= 10.0;
        mag += 1;
    }
    while v < 10.0 && mag > -1 {
        v *= 10.0;
        mag -= 1;
    }
    let sig = v.round() as i32;
    let (d1, d2, mag) = if sig >= 100 { (1, 0, mag + 1) } else { (sig / 10, sig % 10, mag) };
    [digit(d1), digit(d2), digit(mag.clamp(-1, 9))]
}

/// How a wire leaves its endpoints and dodges its siblings.
#[derive(Clone, Copy, Default)]
pub struct Route {
    /// Unit-ish exit direction at each end (away from the pad's body);
    /// `Vec2::ZERO` = no preference (routing dots, the mouse cursor).
    pub exit_a: Vec2,
    pub exit_b: Vec2,
    /// Small deterministic offset so parallel runs don't overlap.
    pub lane: i32,
    /// Stub length in pixels before the wire starts routing.
    pub stub: f32,
    /// Pixels needed to clear each endpoint's body when the route has to
    /// double back — keeps wires from cutting through their own component.
    pub clear_a: f32,
    pub clear_b: f32,
}

/// Orthogonal wire path between two screen points, honoring exit stubs,
/// with rounded corners.
pub fn wire_path(a: Pos2, b: Pos2, route: Route) -> Vec<Pos2> {
    let snap = |v: Vec2| -> Vec2 {
        if v.length() < 0.1 {
            Vec2::ZERO
        } else if v.x.abs() >= v.y.abs() {
            Vec2::new(v.x.signum(), 0.0)
        } else {
            Vec2::new(0.0, v.y.signum())
        }
    };
    let (ea, eb) = (snap(route.exit_a), snap(route.exit_b));
    let mut sa = a + ea * route.stub;
    let mut sb = b + eb * route.stub;
    let lane = route.lane as f32 * 7.0;

    // When the target sits BEHIND an exit direction, step sideways past the
    // body first instead of routing straight back through it.
    let dodge = |s: Pos2, e: Vec2, toward: Pos2, clear: f32| -> Option<Pos2> {
        if e == Vec2::ZERO || clear <= 0.0 {
            return None;
        }
        let behind = if e.y == 0.0 {
            (toward.x - s.x) * e.x < 0.0
        } else {
            (toward.y - s.y) * e.y < 0.0
        };
        if !behind {
            return None;
        }
        if e.y == 0.0 {
            let dir = if toward.y >= s.y { 1.0 } else { -1.0 };
            Some(Pos2::new(s.x, s.y + dir * clear))
        } else {
            let dir = if toward.x >= s.x { 1.0 } else { -1.0 };
            Some(Pos2::new(s.x + dir * clear, s.y))
        }
    };
    let extra_a = dodge(sa, ea, sb, route.clear_a);
    if let Some(p) = extra_a {
        sa = p;
    }
    let extra_b = dodge(sb, eb, sa, route.clear_b);
    if let Some(p) = extra_b {
        sb = p;
    }

    // Fall back to geometry when an end has no exit preference.
    let axis_h = |e: Vec2, from: Pos2, to: Pos2| {
        if e == Vec2::ZERO { (to.x - from.x).abs() >= (to.y - from.y).abs() } else { e.y == 0.0 }
    };
    let mut ha = axis_h(ea, sa, sb);
    let mut hb = axis_h(eb, sb, sa);
    // After a dodge the leg leaves perpendicular to the original exit.
    if extra_a.is_some() {
        ha = ea.y != 0.0;
    }
    if extra_b.is_some() {
        hb = eb.y != 0.0;
    }

    let mut corners: Vec<Pos2> = vec![a];
    if extra_a.is_some() {
        corners.push(a + ea * route.stub);
    }
    if sa != a {
        corners.push(sa);
    }
    match (ha, hb) {
        (true, true) => {
            let mid_x = (sa.x + sb.x) / 2.0 + lane;
            corners.push(Pos2::new(mid_x, sa.y));
            corners.push(Pos2::new(mid_x, sb.y));
        }
        (false, false) => {
            let mid_y = (sa.y + sb.y) / 2.0 + lane;
            corners.push(Pos2::new(sa.x, mid_y));
            corners.push(Pos2::new(sb.x, mid_y));
        }
        (true, false) => corners.push(Pos2::new(sb.x, sa.y)),
        (false, true) => corners.push(Pos2::new(sa.x, sb.y)),
    }
    if sb != b {
        corners.push(sb);
    }
    if extra_b.is_some() {
        corners.push(b + eb * route.stub);
    }
    corners.push(b);
    corners.dedup_by(|p, q| (*p - *q).length() < 0.5);
    if corners.len() < 2 {
        return vec![a, b];
    }
    rounded_polyline(&corners, 9.0)
}

/// Plain midpoint route, for previews with no endpoint context.
pub fn wire_points(a: Pos2, b: Pos2) -> Vec<Pos2> {
    wire_path(a, b, Route::default())
}

/// Densify a corner path into a polyline with quarter-round corners.
fn rounded_polyline(corners: &[Pos2], radius: f32) -> Vec<Pos2> {
    let mut pts = vec![corners[0]];
    for i in 1..corners.len().saturating_sub(1) {
        let (prev, p, next) = (corners[i - 1], corners[i], corners[i + 1]);
        let (d1, d2) = (p - prev, next - p);
        let r = radius.min(d1.length() * 0.5).min(d2.length() * 0.5);
        if r < 0.5 {
            pts.push(p);
            continue;
        }
        let enter = p - d1.normalized() * r;
        let exit = p + d2.normalized() * r;
        for k in 0..=6 {
            let t = k as f32 / 6.0;
            let u = 1.0 - t;
            pts.push(Pos2::new(
                u * u * enter.x + 2.0 * u * t * p.x + t * t * exit.x,
                u * u * enter.y + 2.0 * u * t * p.y + t * t * exit.y,
            ));
        }
    }
    pts.push(*corners.last().unwrap());
    pts
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

/// Replay simulated ST7735 draw ops onto the component's screen area.
pub fn draw_lcd(
    painter: &egui::Painter,
    view: &View,
    comp: &PlacedComponent,
    def: &ComponentDef,
    ops: &[wirelab_core::sim::LcdOp],
) {
    use wirelab_core::sim::LcdOp;
    let center = view.to_screen(comp.pos);
    let w = view.px(def.visual.width_mm);
    let h = view.px(def.visual.height_mm);
    // Square screen, slightly above centre like the real module.
    let side = w.min(h) * 0.82;
    let screen = Rect::from_center_size(
        Pos2::new(center.x, center.y - h * 0.06),
        Vec2::splat(side),
    );
    let px = side / 128.0;
    let at = |x: u8, y: u8| {
        Pos2::new(screen.min.x + f32::from(x) * px, screen.min.y + f32::from(y) * px)
    };
    painter.rect_filled(screen.expand(2.0), CornerRadius::same(2), Color32::from_gray(15));
    painter.rect_filled(screen, CornerRadius::ZERO, Color32::BLACK);
    for op in ops {
        match op {
            LcdOp::Clear(rgb) => {
                painter.rect_filled(
                    screen,
                    CornerRadius::ZERO,
                    Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
                );
            }
            LcdOp::Rect { x, y, w, h, rgb } => {
                let r = Rect::from_min_size(
                    at(*x, *y),
                    Vec2::new(f32::from(*w) * px, f32::from(*h) * px),
                );
                painter.rect_filled(
                    r.intersect(screen),
                    CornerRadius::ZERO,
                    Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
                );
            }
            LcdOp::Text { x, y, rgb, text } => {
                painter.text(
                    at(*x, *y),
                    Align2::LEFT_TOP,
                    text,
                    FontId::monospace((10.0 * px).max(6.0)),
                    Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
                );
            }
        }
    }
}
