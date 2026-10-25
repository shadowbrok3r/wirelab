//! Part, category and board icons from `assets/icons`, generated into a typed table by build.rs.

use egui::emath::GuiRounding as _;
use egui::{Color32, Rect, Ui, pos2};
use egui_icon_pack::{Icon, snapped_square};
use wirelab_core::board::BoardProfile;
use wirelab_core::component::{ComponentDef, Shape, SimModel};

#[allow(dead_code)]
mod table {
    include!(concat!(env!("OUT_DIR"), "/icon_table.rs"));
}
pub use table::*;

/// Id keywords that pick an icon before the drawn shape does.
const BY_ID: [(&str, Icon); 16] = [
    ("dht", PART_DHT22),
    ("hc-sr04", PART_ULTRASONIC),
    ("ultrasonic", PART_ULTRASONIC),
    ("pir", PART_PIR),
    ("soil", PART_SOIL_MOISTURE),
    ("touch", PART_TOUCH_SENSOR),
    ("rc522", PART_RFID_CARD),
    ("rdm6300", PART_RFID_COIL),
    ("st7735", PART_TFT),
    ("tft", PART_TFT),
    ("oled", PART_TFT),
    ("lcd", PART_TFT),
    ("reed", PART_REED_SWITCH),
    ("tilt", PART_TILT_SWITCH),
    ("analog-sensor", PART_ANALOG_SENSOR),
    ("relay", PART_RELAY),
];

/// Resistor band slots on the 256-unit icon canvas: (x0, x1) per band, shared y range.
const BAND_X: [(f32, f32); 4] = [(80.0, 98.0), (104.0, 122.0), (128.0, 146.0), (170.0, 184.0)];
const BAND_Y: (f32, f32) = (96.0, 160.0);

const DIGIT: [Color32; 10] = [
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
const GOLD: Color32 = Color32::from_rgb(212, 175, 55);
const SILVER: Color32 = Color32::from_rgb(192, 192, 192);

/// Icon for a library component: an id keyword first, then the drawn shape.
pub fn part(def: &ComponentDef) -> Icon {
    if let Some((_, icon)) = BY_ID.iter().find(|(key, _)| def.id.contains(key)) {
        return *icon;
    }
    match def.visual.shape {
        Shape::Led => PART_LED,
        Shape::PushButton => PART_PUSH_BUTTON,
        Shape::ToggleSwitch => PART_TOGGLE_SWITCH,
        Shape::SlideSwitch => PART_SLIDE_SWITCH,
        Shape::Resistor => PART_RESISTOR,
        Shape::Potentiometer => PART_POTENTIOMETER,
        Shape::Photoresistor => PART_PHOTORESISTOR,
        Shape::Buzzer => match def.sim {
            SimModel::Buzzer { active: false } => PART_BUZZER_TONE,
            _ => PART_BUZZER,
        },
        Shape::Servo => PART_SERVO,
        Shape::Relay => PART_RELAY,
        Shape::SensorModule => PART_SENSOR,
        Shape::Chip | Shape::Generic => PART_CHIP,
        Shape::Dot => PART_JUNCTION,
    }
}

pub fn category(name: &str) -> Icon {
    match name.to_ascii_lowercase().as_str() {
        "input" => CAT_INPUT,
        "output" => CAT_OUTPUT,
        "analog" => CAT_ANALOG,
        "display" => CAT_DISPLAY,
        "passive" => CAT_PASSIVE,
        "routing" => CAT_ROUTING,
        _ => CAT_OTHER,
    }
}

pub fn board(profile: &BoardProfile) -> Icon {
    let id = profile.id.to_ascii_lowercase();
    if ["lcd", "tft", "oled", "display"]
        .iter()
        .any(|k| id.contains(k))
    {
        BOARD_LCD
    } else {
        BOARD_DEVKIT
    }
}

/// The part's body colour, pulled toward the text colour when it would vanish into the panel.
pub fn part_tint(ui: &Ui, def: &ComponentDef) -> Color32 {
    let [r, g, b] = def.visual.color;
    let body = Color32::from_rgb(r, g, b);
    if (luma(body) - luma(ui.visuals().panel_fill)).abs() < 0.3 {
        body.lerp_to_gamma(ui.visuals().text_color(), 0.55)
    } else {
        body
    }
}

/// Paints `def`'s icon in its body colour; resistors also get their colour code.
pub fn paint_part(ui: &Ui, rect: Rect, def: &ComponentDef) {
    let size = rect.width().min(rect.height());
    let tint = part_tint(ui, def);
    let icon = part(def);
    match def.sim {
        SimModel::Resistor { ohms } if icon == PART_RESISTOR => {
            icon.paint_duo(ui, rect, size, tint, tint.gamma_multiply(0.85));
            paint_bands(ui, rect, size, ohms);
        }
        _ => icon.paint_duo(ui, rect, size, tint, tint.gamma_multiply(0.35)),
    }
}

/// Four-band colour code: two digits, multiplier, gold (5 %) tolerance.
pub fn color_code(ohms: f32) -> Option<[Color32; 4]> {
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
        -2 => SILVER,
        -1 => GOLD,
        0..=9 => DIGIT[exp as usize],
        _ => return None,
    };
    Some([
        DIGIT[(digits / 10) as usize],
        DIGIT[(digits % 10) as usize],
        multiplier,
        GOLD,
    ])
}

fn paint_bands(ui: &Ui, rect: Rect, size: f32, ohms: f32) {
    let Some(colors) = color_code(ohms) else {
        return;
    };
    let square = snapped_square(rect.center(), size, ui.pixels_per_point());
    let unit = square.width() / 256.0;
    let at = |x: f32, y: f32| pos2(square.left() + x * unit, square.top() + y * unit);
    for ((x0, x1), color) in BAND_X.into_iter().zip(colors) {
        let band = Rect::from_min_max(at(x0, BAND_Y.0), at(x1, BAND_Y.1))
            .round_to_pixels(ui.pixels_per_point());
        ui.painter().rect_filled(band, 0.0, color);
    }
}

fn luma(c: Color32) -> f32 {
    (0.2126 * f32::from(c.r()) + 0.7152 * f32::from(c.g()) + 0.0722 * f32::from(c.b())) / 255.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use wirelab_core::library::Library;

    fn shipped() -> Library {
        let assets = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        Library::load(&assets.join("boards"), &assets.join("components"))
            .expect("shipped library loads")
    }

    #[test]
    fn every_shipped_part_has_a_specific_icon() {
        for def in shipped().components.values() {
            let icon = part(def);
            assert!(
                icon != PART_CHIP && icon != PART_SENSOR,
                "{} falls back to {}",
                def.id,
                icon.name()
            );
        }
    }

    #[test]
    fn every_shipped_category_has_its_own_icon() {
        for def in shipped().components.values() {
            assert!(
                category(&def.category) != CAT_OTHER,
                "category '{}' has no icon",
                def.category
            );
        }
    }

    #[test]
    fn buzzers_split_by_drive() {
        let lib = shipped();
        assert_eq!(part(lib.component("buzzer-active").unwrap()), PART_BUZZER);
        assert_eq!(
            part(lib.component("buzzer-passive").unwrap()),
            PART_BUZZER_TONE
        );
    }

    #[test]
    fn resistor_colour_codes() {
        let (r, v, y, o, k, n, g) = (
            DIGIT[2], DIGIT[7], DIGIT[4], DIGIT[3], DIGIT[0], DIGIT[1], DIGIT[5],
        );
        assert_eq!(color_code(220.0), Some([r, r, n, GOLD]));
        assert_eq!(color_code(330.0), Some([o, o, n, GOLD]));
        assert_eq!(color_code(1_000.0), Some([n, k, r, GOLD]));
        assert_eq!(color_code(4_700.0), Some([y, v, r, GOLD]));
        assert_eq!(color_code(100.0), Some([n, k, n, GOLD]));
        assert_eq!(color_code(1_000_000.0), Some([n, k, g, GOLD]));
        assert_eq!(color_code(4.7), Some([y, v, GOLD, GOLD]));
        assert_eq!(color_code(0.0), None);
    }

    #[test]
    fn every_shipped_resistor_has_a_colour_code() {
        for def in shipped().components.values() {
            if let SimModel::Resistor { ohms } = def.sim {
                assert!(color_code(ohms).is_some(), "{} ({ohms} ohm)", def.id);
            }
        }
    }
}
