//! User programs: rules mapping triggers to action sequences.

use serde::{Deserialize, Serialize};

use crate::circuit::CompId;
use crate::component::PropMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Trigger {
    /// Component event by id, e.g. "pressed" on a push button.
    CompEvent { comp: CompId, event: String },
    PinRises { gpio: u8 },
    PinFalls { gpio: u8 },
    /// Analog level on a GPIO crosses a threshold, with hysteresis.
    AnalogAbove { gpio: u8, millivolts: u16 },
    AnalogBelow { gpio: u8, millivolts: u16 },
    Every { ms: u32 },
    OnStart,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Component verb by id, e.g. "toggle" on an LED.
    CompAction { comp: CompId, action: String, #[serde(default)] params: PropMap },
    SetPin { gpio: u8, high: bool },
    TogglePin { gpio: u8 },
    SetPwm { gpio: u8, freq_hz: u32, duty_permille: u16 },
    /// Pause before the rest of this rule's actions run.
    Wait { ms: u32 },
    Log { text: String },
    /// Reconfigure a pin, e.g. watch the BOOT button as an input.
    SetPinMode { gpio: u8, mode: wirelab_proto::PinMode },
    /// Drive a WS2812-style addressable LED.
    SetRgb { gpio: u8, r: u8, g: u8, b: u8 },
    /// Periodic ADC sampling on a pin; 0 disables.
    WatchAnalog { gpio: u8, interval_ms: u16 },
    /// Bring up UART1 on arbitrary pins; baud 0 tears it down.
    UartConfig { tx: u8, rx: u8, baud: u32 },
    /// Send bytes out UART1 (chunked into protocol frames).
    UartWrite { data: Vec<u8> },
    /// Bring up an ST7735 SPI display; `bl` 255 = no backlight pin.
    LcdInit { sck: u8, mosi: u8, cs: u8, dc: u8, rst: u8, bl: u8 },
    LcdClear { rgb: [u8; 3] },
    LcdRect { x: u8, y: u8, w: u8, h: u8, rgb: [u8; 3] },
    LcdText { x: u8, y: u8, rgb: [u8; 3], text: String },
    SpiConfig { sck: u8, mosi: u8, miso: u8, freq_khz: u32 },
    SpiTransfer { cs: u8, data: Vec<u8> },
    I2cConfig { sda: u8, scl: u8, freq_khz: u32 },
    I2cWrite { addr: u8, data: Vec<u8> },
    /// `reg` 256 = plain read without register select.
    I2cRead { addr: u8, reg: u16, len: u8 },
    /// Host-side routing to another board tab's scripts; never sent to a device.
    BoardMsg { to: String, text: String },
}

/// Pack 8-bit RGB into the display's native RGB565.
pub fn rgb565(rgb: [u8; 3]) -> u16 {
    (u16::from(rgb[0] >> 3) << 11) | (u16::from(rgb[1] >> 2) << 5) | u16::from(rgb[2] >> 3)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub trigger: Trigger,
    pub actions: Vec<Action>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Program {
    pub rules: Vec<Rule>,
}
