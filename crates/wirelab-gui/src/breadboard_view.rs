//! Breadboard art: solder mask, gold pads, silkscreen, bus highlights.

use egui::epaint::TextShape;
use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Stroke, StrokeKind, Vec2};
use wirelab_core::breadboard::{BreadboardProfile, Bus, HoleRef, local_to_world};
use wirelab_core::circuit::PlacedBreadboard;

use crate::draw::View;

/// Outer radius of a hole's copper ring.
pub const PAD_R_MM: f32 = 0.9;
const DRILL_R_MM: f32 = 0.5;
const DRILL: Color32 = Color32::from_rgb(12, 11, 10);

pub struct BbPaint<'a> {
    /// Holes on the copper under the cursor.
    pub bus: &'a [HoleRef],
    pub bus_color: Color32,
    pub hovered: Option<HoleRef>,
    pub selected: bool,
    pub accent: Color32,
}

fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Screen rect of a placed board; quarter turns keep it axis-aligned.
pub fn screen_rect(view: &View, bb: &PlacedBreadboard, p: &BreadboardProfile) -> Rect {
    let a = view.to_screen(local_to_world(bb, p, [0.0, 0.0]));
    let b = view.to_screen(local_to_world(bb, p, [p.width_mm, p.height_mm]));
    Rect::from_two_pos(a, b)
}

fn corner(px: f32) -> CornerRadius {
    CornerRadius::same(px.round().clamp(0.0, 255.0) as u8)
}

pub fn draw(painter: &egui::Painter, view: &View, bb: &PlacedBreadboard, p: &BreadboardProfile, paint: &BbPaint) {
    let rect = screen_rect(view, bb, p);
    if !painter.clip_rect().intersects(rect.expand(8.0)) {
        return;
    }
    let (mask, pad, silk) = (rgb(p.colors.mask), rgb(p.colors.pad), rgb(p.colors.silk));
    let at = |local: [f32; 2]| view.to_screen(local_to_world(bb, p, local));
    let radius = corner(view.px(p.corner_radius_mm));

    painter.rect_filled(rect.translate(Vec2::new(2.0, 3.0)), radius, Color32::from_black_alpha(90));
    painter.rect_filled(rect, radius, mask);
    painter.rect_stroke(rect, radius, Stroke::new(1.0, Color32::from_gray(58)), StrokeKind::Inside);
    if paint.selected {
        painter.rect_stroke(rect.expand(3.0), radius, Stroke::new(1.6, paint.accent), StrokeKind::Outside);
    }

    let silk_w = view.px(0.16).clamp(0.6, 1.5);
    if p.silk.border_inset_mm > 0.0 {
        let inset = view.px(p.silk.border_inset_mm);
        let r = corner(view.px((p.corner_radius_mm - p.silk.border_inset_mm).max(0.4)));
        painter.rect_stroke(rect.shrink(inset), r, Stroke::new(silk_w, silk), StrokeKind::Middle);
    }

    for m in &p.mounts {
        let c = at(m.at);
        let r = view.px(m.dia_mm / 2.0);
        if m.plated {
            painter.circle_filled(c, r + view.px(0.8), pad);
            painter.circle_stroke(c, r + view.px(0.8), Stroke::new(0.8, pad.gamma_multiply(0.6)));
        } else {
            painter.circle_stroke(c, r + view.px(0.35), Stroke::new(silk_w, silk.gamma_multiply(0.8)));
        }
        painter.circle_filled(c, r, DRILL);
    }

    let ring = view.px(PAD_R_MM);
    let drill = view.px(DRILL_R_MM);
    let pad_edge = Stroke::new(0.7, Color32::from_rgb(150, 118, 48));
    for (li, l) in p.lines.iter().enumerate() {
        for index in l.first..l.first + l.count {
            let c = at(p.hole_local(HoleRef { line: li, index }));
            if ring < 1.4 {
                painter.circle_filled(c, 1.0, pad);
                continue;
            }
            painter.circle_filled(c, ring, pad);
            painter.circle_stroke(c, ring, pad_edge);
            painter.circle_filled(c, drill, DRILL);
            if ring > 4.0 {
                painter.circle_filled(c + Vec2::new(-0.35, -0.4) * ring, ring * 0.22, Color32::from_white_alpha(46));
            }
        }
    }

    // Silkscreen: hole numbers, line names, rail polarity marks, free text.
    let font_px = |mm: f32| view.px(mm).clamp(0.0, 22.0);
    let label = |pos: Pos2, text: &str, size: f32| {
        if size >= 6.0 {
            painter.text(pos, Align2::CENTER_CENTER, text, FontId::monospace(size), silk);
        }
    };
    let polarity_mark = |pos: Pos2, sign: &str| {
        let r = view.px(0.95);
        if r < 2.5 {
            return;
        }
        let s = Stroke::new(silk_w.max(1.0), silk);
        painter.circle_stroke(pos, r, s);
        painter.line_segment([pos - Vec2::new(r * 0.55, 0.0), pos + Vec2::new(r * 0.55, 0.0)], s);
        if sign == "+" {
            painter.line_segment([pos - Vec2::new(0.0, r * 0.55), pos + Vec2::new(0.0, r * 0.55)], s);
        }
    };
    for n in &p.silk.numbers {
        let Some(li) = p.lines.iter().position(|l| l.name == n.line) else { continue };
        let l = &p.lines[li];
        for index in l.first..l.first + l.count {
            let h = p.hole_local(HoleRef { line: li, index });
            label(at([h[0] + n.offset[0], h[1] + n.offset[1]]), &index.to_string(), font_px(1.2));
        }
    }
    for n in &p.silk.names {
        for (li, l) in p.lines.iter().enumerate() {
            let wanted = if n.lines.is_empty() {
                matches!(l.bus, Bus::Strip { .. })
            } else {
                n.lines.iter().any(|x| x == &l.name)
            };
            if !wanted || n.index < l.first || n.index >= l.first + l.count {
                continue;
            }
            let h = p.hole_local(HoleRef { line: li, index: n.index });
            let pos = at([h[0] + n.offset[0], h[1] + n.offset[1]]);
            if l.polarity.is_empty() {
                label(pos, &l.name, font_px(1.3));
            } else {
                polarity_mark(pos, &l.polarity);
            }
        }
    }
    for t in &p.silk.texts {
        let size = font_px(t.size_mm);
        if size < 6.0 {
            continue;
        }
        let galley = painter.layout_no_wrap(t.text.clone(), FontId::proportional(size), silk);
        let c = at(t.at);
        // Board text turns with the board but never reads upside down.
        let turns = (bb.rotation % 360 / 90) as i32 - i32::from(t.vertical);
        let turns = match turns.rem_euclid(4) {
            2 => 0,
            3 => -1,
            n => n,
        };
        let angle = turns as f32 * std::f32::consts::FRAC_PI_2;
        let half = galley.size() * 0.5;
        let (s, cs) = angle.sin_cos();
        let rotated = Vec2::new(half.x * cs - half.y * s, half.x * s + half.y * cs);
        painter.add(TextShape::new(c - rotated, galley, silk).with_angle(angle));
    }

    for &h in paint.bus {
        let c = at(p.hole_local(h));
        painter.circle_stroke(c, ring.max(2.5) + 1.6, Stroke::new(1.8, paint.bus_color));
    }
    if let Some(h) = paint.hovered {
        let c = at(p.hole_local(h));
        painter.circle_stroke(c, ring.max(2.5) + 3.0, Stroke::new(2.0, paint.accent));
    }
}

/// Lead seated in a hole: a solder fillet over the pad.
pub fn draw_seat(painter: &egui::Painter, view: &View, world: [f32; 2]) {
    let c = view.to_screen(world);
    let r = view.px(PAD_R_MM * 0.95);
    if r < 2.0 {
        return;
    }
    painter.circle_filled(c, r, Color32::from_rgb(196, 200, 206));
    painter.circle_stroke(c, r, Stroke::new(1.0, Color32::from_rgb(120, 124, 130)));
    painter.circle_filled(c + Vec2::new(-0.3, -0.35) * r, r * 0.3, Color32::from_white_alpha(120));
}

/// Lead over a board that misses every hole.
pub fn draw_miss(painter: &egui::Painter, view: &View, world: [f32; 2], color: Color32) {
    let c = view.to_screen(world);
    let r = view.px(PAD_R_MM).max(3.0);
    let s = Stroke::new(1.6, color);
    painter.circle_stroke(c, r + 1.0, s);
    painter.line_segment([c + Vec2::new(-r, -r) * 0.6, c + Vec2::new(r, r) * 0.6], s);
    painter.line_segment([c + Vec2::new(-r, r) * 0.6, c + Vec2::new(r, -r) * 0.6], s);
}
