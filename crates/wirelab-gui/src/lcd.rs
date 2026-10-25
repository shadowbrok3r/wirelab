//! Pixel-exact preview of the simulated or mirrored ST7735, rasterized with the firmware's font.

use std::hash::{Hash, Hasher};

use egui::epaint::Vertex;
use egui::{
    Color32, ColorImage, Mesh, Painter, Pos2, Rect, TextureHandle, TextureId, TextureOptions, Vec2,
    pos2,
};
use embedded_graphics::mono_font::MonoTextStyle;
use embedded_graphics::mono_font::ascii::FONT_6X10;
use embedded_graphics::pixelcolor::Rgb888;
use embedded_graphics::prelude::*;
use embedded_graphics::text::Text;
use wirelab_core::circuit::PlacedComponent;
use wirelab_core::component::ComponentDef;
use wirelab_core::sim::{LCD_SIDE, LcdOp};

use crate::draw::View;

const SIDE: usize = LCD_SIDE as usize;

struct Frame(Vec<Color32>);

impl OriginDimensions for Frame {
    fn size(&self) -> Size {
        Size::new(SIDE as u32, SIDE as u32)
    }
}

impl DrawTarget for Frame {
    type Color = Rgb888;
    type Error = core::convert::Infallible;

    fn draw_iter<I: IntoIterator<Item = Pixel<Rgb888>>>(
        &mut self,
        pixels: I,
    ) -> Result<(), Self::Error> {
        for Pixel(p, c) in pixels {
            if let (Ok(x), Ok(y)) = (usize::try_from(p.x), usize::try_from(p.y))
                && x < SIDE
                && y < SIDE
            {
                self.0[y * SIDE + x] = Color32::from_rgb(c.r(), c.g(), c.b());
            }
        }
        Ok(())
    }
}

/// The panel image an op log produces.
pub fn rasterize(ops: &[LcdOp]) -> ColorImage {
    let mut frame = Frame(vec![Color32::BLACK; SIDE * SIDE]);
    for op in ops {
        match op {
            LcdOp::Clear(rgb) => frame.0.fill(Color32::from_rgb(rgb[0], rgb[1], rgb[2])),
            LcdOp::Rect { rgb, .. } => {
                let [x0, y0, x1, y1] = op.bounds().map(usize::from);
                let color = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                for row in frame.0.chunks_exact_mut(SIDE).take(y1).skip(y0) {
                    row[x0..x1].fill(color);
                }
            }
            LcdOp::Text { x, y, rgb, text } => {
                let style = MonoTextStyle::new(&FONT_6X10, Rgb888::new(rgb[0], rgb[1], rgb[2]));
                // The firmware anchors (x, y) at the glyph top-left of an alphabetic baseline.
                let at = Point::new(i32::from(*x), i32::from(*y) + 8);
                let _ = Text::new(text, at, style).draw(&mut frame);
            }
        }
    }
    ColorImage::new([SIDE, SIDE], frame.0)
}

fn ops_key(ops: &[LcdOp]) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    for op in ops {
        match op {
            LcdOp::Clear(rgb) => (0u8, rgb).hash(&mut h),
            LcdOp::Rect {
                x,
                y,
                w,
                h: rh,
                rgb,
            } => (1u8, x, y, w, rh, rgb).hash(&mut h),
            LcdOp::Text { x, y, rgb, text } => (2u8, x, y, rgb, text).hash(&mut h),
        }
    }
    h.finish()
}

/// Texture of the panel image, re-uploaded only when the op log changes.
#[derive(Default)]
pub struct LcdTexture {
    key: Option<u64>,
    texture: Option<TextureHandle>,
}

impl LcdTexture {
    pub fn texture(&mut self, ctx: &egui::Context, ops: &[LcdOp]) -> TextureId {
        let key = ops_key(ops);
        match &mut self.texture {
            Some(tex) if self.key == Some(key) => return tex.id(),
            Some(tex) => tex.set(rasterize(ops), TextureOptions::NEAREST),
            None => {
                self.texture =
                    Some(ctx.load_texture("wirelab-lcd", rasterize(ops), TextureOptions::NEAREST));
            }
        }
        self.key = Some(key);
        self.texture
            .as_ref()
            .map_or(TextureId::default(), TextureHandle::id)
    }
}

/// Screen-space glass of an ST7735 module, rotated with the part.
pub fn screen_rect(view: &View, comp: &PlacedComponent, def: &ComponentDef) -> Rect {
    let (w, h) = (view.px(def.visual.width_mm), view.px(def.visual.height_mm));
    let [dx, dy] = wirelab_core::geometry::rotate([0.0, -0.06 * h], comp.rotation);
    Rect::from_center_size(
        view.to_screen(comp.pos) + Vec2::new(dx, dy),
        Vec2::splat(0.82 * w.min(h)),
    )
}

/// Paints the panel image into `rect`, turned clockwise by `rotation` degrees.
pub fn paint(painter: &Painter, rect: Rect, rotation: u16, texture: TextureId) {
    const UV: [Pos2; 4] = [
        pos2(0.0, 0.0),
        pos2(1.0, 0.0),
        pos2(1.0, 1.0),
        pos2(0.0, 1.0),
    ];
    let turns = usize::from(rotation / 90 % 4);
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    let mut mesh = Mesh::with_texture(texture);
    for (i, pos) in corners.into_iter().enumerate() {
        mesh.vertices.push(Vertex {
            pos,
            uv: UV[(i + 4 - turns) % 4],
            color: Color32::WHITE,
        });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(mesh);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rects_and_text_land_where_the_firmware_draws_them() {
        let ops = vec![
            LcdOp::Clear([0, 0, 255]),
            LcdOp::Rect {
                x: 10,
                y: 20,
                w: 4,
                h: 2,
                rgb: [255, 0, 0],
            },
            LcdOp::Text {
                x: 0,
                y: 100,
                rgb: [255, 255, 255],
                text: "I".into(),
            },
        ];
        let img = rasterize(&ops);
        let at = |x: usize, y: usize| img.pixels[y * SIDE + x];
        assert_eq!(at(0, 0), Color32::from_rgb(0, 0, 255));
        assert_eq!(at(10, 20), Color32::from_rgb(255, 0, 0));
        assert_eq!(at(13, 21), Color32::from_rgb(255, 0, 0));
        assert_eq!(at(14, 21), Color32::from_rgb(0, 0, 255));
        let lit = (100..110)
            .flat_map(|y| (0..6).map(move |x| (x, y)))
            .filter(|&(x, y)| at(x, y) == Color32::WHITE)
            .count();
        assert!(lit > 4, "glyph pixels inside the 6x10 cell: {lit}");
        assert!(
            (0..SIDE).all(|x| at(x, 99) != Color32::WHITE),
            "nothing above the cell"
        );
    }
}
