//! App-wide control conventions on top of egui-theme-kit: SVG icons, fixed-size buttons, tab titles.

use egui::{Color32, Response, Ui, WidgetText};
pub use egui_icon_pack::phosphor_regular as ph;
use egui_theme_kit::controls::{self, ButtonKind, IconBox, IconButton, StaticImage, TextButton};

/// Width every labelled button shares, so button rows line up.
pub const BUTTON_W: f32 = 112.0;

/// The kit icon for a pack icon, duotone when the pack ships a back layer.
pub fn kit(icon: egui_icon_pack::Icon) -> controls::Icon {
    let front = StaticImage::new(icon.uri(), icon.svg());
    match icon.duo_svg() {
        Some(back) => controls::Icon::duotone(front, StaticImage::new(icon.duo_uri(), back)),
        None => controls::Icon::Image(front),
    }
}

/// Dock tab title: the icon's font glyph and the title.
pub fn tab_title(icon: egui_icon_pack::Icon, title: &str) -> WidgetText {
    controls::tab_title(controls::Icon::glyph(icon.glyph()), title)
}

fn labelled(
    icon: egui_icon_pack::Icon,
    label: &str,
    kind: ButtonKind,
    enabled: bool,
) -> TextButton<'_> {
    TextButton::new(label)
        .icon(kit(icon))
        .kind(kind)
        .min_width(BUTTON_W)
        .enabled(enabled)
}

/// Neutral icon + label button at the shared width.
pub fn button(ui: &mut Ui, icon: egui_icon_pack::Icon, label: &str) -> Response {
    labelled(icon, label, ButtonKind::Neutral, true).show(ui)
}

pub fn button_enabled(
    ui: &mut Ui,
    enabled: bool,
    icon: egui_icon_pack::Icon,
    label: &str,
) -> Response {
    labelled(icon, label, ButtonKind::Neutral, enabled).show(ui)
}

pub fn primary(ui: &mut Ui, enabled: bool, icon: egui_icon_pack::Icon, label: &str) -> Response {
    labelled(icon, label, ButtonKind::Primary, enabled).show(ui)
}

pub fn danger(ui: &mut Ui, icon: egui_icon_pack::Icon, label: &str) -> Response {
    labelled(icon, label, ButtonKind::Danger, true).show(ui)
}

/// Borderless icon + label button for list rows and inline actions.
pub fn ghost(ui: &mut Ui, icon: egui_icon_pack::Icon, label: &str) -> Response {
    TextButton::new(label)
        .icon(kit(icon))
        .kind(ButtonKind::Ghost)
        .show(ui)
}

/// Square icon-only toolbar button with a tooltip.
pub fn tool(ui: &mut Ui, icon: egui_icon_pack::Icon, tip: &str) -> Response {
    IconButton::new(kit(icon), IconBox::Md).tip(tip).show(ui)
}

pub fn tool_enabled(ui: &mut Ui, enabled: bool, icon: egui_icon_pack::Icon, tip: &str) -> Response {
    IconButton::new(kit(icon), IconBox::Md)
        .tip(tip)
        .enabled(enabled)
        .show(ui)
}

/// Square icon-only toggle; `on` draws it selected.
pub fn tool_toggle(ui: &mut Ui, on: bool, icon: egui_icon_pack::Icon, tip: &str) -> Response {
    IconButton::new(kit(icon), IconBox::Md)
        .selected(on)
        .tip(tip)
        .show(ui)
}

/// Small square icon button tinted `tone`, for row actions.
pub fn row_action(ui: &mut Ui, icon: egui_icon_pack::Icon, tip: &str, tone: Color32) -> Response {
    IconButton::new(kit(icon), IconBox::Sm)
        .tint(tone)
        .tip(tip)
        .show(ui)
}

/// Menu row with an SVG icon.
pub fn menu_item(ui: &mut Ui, icon: egui_icon_pack::Icon, label: &str) -> Response {
    controls::menu_item(ui, kit(icon), label)
}

/// Status-tinted icon and text.
pub fn status(ui: &mut Ui, icon: egui_icon_pack::Icon, text: &str, tone: Color32) -> Response {
    controls::status_label(ui, kit(icon), text, tone)
}
