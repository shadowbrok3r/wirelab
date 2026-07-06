//! Board profiles: per-devkit pin maps, capabilities and layout.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};
use wirelab_proto::ChipKind;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PinCaps: u32 {
        const DIGITAL_IN     = 1 << 0;
        const DIGITAL_OUT    = 1 << 1;
        const PWM            = 1 << 2;
        const ADC            = 1 << 3;
        const DAC            = 1 << 4;
        const TOUCH          = 1 << 5;
        const INPUT_ONLY     = 1 << 6;
        const STRAPPING      = 1 << 7;
        const FLASH_RESERVED = 1 << 8;
        const USB_JTAG       = 1 << 9;
        const UART0          = 1 << 10;
        const RTC            = 1 << 11;
    }
}

impl Serialize for PinCaps {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        bitflags::serde::serialize(self, s)
    }
}

impl<'de> Deserialize<'de> for PinCaps {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        bitflags::serde::deserialize(d)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PinKind {
    Gpio(u8),
    Gnd,
    V3_3,
    V5,
    En,
    NotConnected,
    Other,
}

impl PinKind {
    pub fn gpio(self) -> Option<u8> {
        match self {
            PinKind::Gpio(n) => Some(n),
            _ => None,
        }
    }

    pub fn is_power_rail(self) -> bool {
        matches!(self, PinKind::Gnd | PinKind::V3_3 | PinKind::V5)
    }

    /// Rail potential in millivolts, if this is a power pin.
    pub fn rail_mv(self) -> Option<f32> {
        match self {
            PinKind::Gnd => Some(0.0),
            PinKind::V3_3 => Some(3300.0),
            PinKind::V5 => Some(5000.0),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardPin {
    /// Unique key within the profile, e.g. "GPIO4", "GND1", "3V3".
    pub key: String,
    /// Silkscreen label, e.g. "IO4", "D5".
    pub label: String,
    pub kind: PinKind,
    pub side: Side,
    /// Zero-based position along the side, counted from the USB end.
    pub index: u32,
    #[serde(default)]
    pub caps: PinCaps,
    /// ADC (unit, channel) if the pin is analog-capable.
    #[serde(default)]
    pub adc: Option<(u8, u8)>,
    /// Human warning shown in the GUI, e.g. strapping-pin caveats.
    #[serde(default)]
    pub warning: Option<String>,
    /// Function-group tags beyond `caps`, e.g. "fspi", "sdio", "lp-uart".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl Default for PinCaps {
    fn default() -> Self {
        PinCaps::empty()
    }
}

/// On-board extras beyond the pin headers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BoardFeatures {
    /// Addressable RGB LED (WS2812-style) data GPIO.
    #[serde(default)]
    pub rgb_led_gpio: Option<u8>,
    /// Physical reset button (clickable in the GUI over serial DTR/RTS).
    #[serde(default)]
    pub reset_button: bool,
    /// Boot/download button and the strapping GPIO it pulls low.
    #[serde(default)]
    pub boot_button_gpio: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardProfile {
    /// Stable identifier, e.g. "esp32-devkitc-v4".
    pub id: String,
    pub name: String,
    pub chip: ChipKind,
    #[serde(default)]
    pub description: String,
    /// Body size in millimetres, pin headers included.
    pub width_mm: f32,
    pub height_mm: f32,
    #[serde(default)]
    pub notes: Vec<String>,
    /// Capability lines shown in the GUI and queryable from scripts,
    /// e.g. "Wi-Fi 6 (802.11ax) 2.4 & 5 GHz".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub specs: Vec<String>,
    #[serde(default)]
    pub features: BoardFeatures,
    pub pins: Vec<BoardPin>,
}

impl BoardProfile {
    pub fn pin(&self, key: &str) -> Option<&BoardPin> {
        self.pins.iter().find(|p| p.key == key)
    }

    pub fn gpio_pin(&self, gpio: u8) -> Option<&BoardPin> {
        self.pins.iter().find(|p| p.kind.gpio() == Some(gpio))
    }

    /// Bit N set = GPIO N is broken out on this board.
    pub fn gpio_mask(&self) -> u64 {
        self.pins
            .iter()
            .filter_map(|p| p.kind.gpio())
            .fold(0u64, |m, g| m | (1u64 << g.min(63)))
    }

    pub fn input_only_mask(&self) -> u64 {
        self.pins
            .iter()
            .filter(|p| p.caps.contains(PinCaps::INPUT_ONLY))
            .filter_map(|p| p.kind.gpio())
            .fold(0u64, |m, g| m | (1u64 << g.min(63)))
    }

    /// Pins sharing a function group with `key`, for hover highlighting.
    /// Returns the group description and the member pin keys.
    pub fn pin_group(&self, key: &str) -> Option<(String, Vec<String>)> {
        let pin = self.pin(key)?;
        if pin.kind.gpio().is_some() {
            let tags = gpio_tagset(pin);
            if tags.is_empty() {
                return None;
            }
            let keys: Vec<String> = self
                .pins
                .iter()
                .filter(|p| p.kind.gpio().is_some())
                .filter(|p| gpio_tagset(p).iter().any(|t| tags.contains(t)))
                .map(|p| p.key.clone())
                .collect();
            Some((tags.join(" · "), keys))
        } else {
            let label = match pin.kind {
                PinKind::Gnd => "ground",
                PinKind::V3_3 => "3.3 V rail",
                PinKind::V5 => "5 V rail",
                PinKind::En => "enable / reset",
                PinKind::NotConnected => "not connected",
                _ => return None,
            };
            let keys: Vec<String> = self
                .pins
                .iter()
                .filter(|p| p.kind == pin.kind)
                .map(|p| p.key.clone())
                .collect();
            Some((label.to_string(), keys))
        }
    }
}

/// Function tags for a GPIO pin: explicit `tags` plus cap-derived ones.
pub fn gpio_tagset(pin: &BoardPin) -> Vec<String> {
    let mut tags: Vec<String> = pin.tags.iter().map(|t| t.to_lowercase()).collect();
    let derived = [
        (PinCaps::INPUT_ONLY, "input-only"),
        (PinCaps::STRAPPING, "strapping"),
        (PinCaps::UART0, "uart0"),
        (PinCaps::USB_JTAG, "usb-jtag"),
        (PinCaps::ADC, "adc"),
        (PinCaps::DAC, "dac"),
        (PinCaps::TOUCH, "touch"),
        (PinCaps::RTC, "rtc"),
        (PinCaps::FLASH_RESERVED, "flash"),
    ];
    for (flag, name) in derived {
        if pin.caps.contains(flag) {
            tags.push(name.to_string());
        }
    }
    tags.sort();
    tags.dedup();
    tags
}
