//! WireLab wire protocol shared between the desktop app and ESP32 firmware.
//!
//! Framing: `postcard` payload + CRC16 (little-endian) appended, COBS-encoded,
//! terminated with a single `0x00` sentinel byte.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod frame;

pub use heapless;

use serde::{Deserialize, Serialize};

pub const PROTO_VERSION: u8 = 1;

/// Max postcard payload size before CRC/COBS overhead.
pub const MAX_PAYLOAD: usize = 192;
/// Max encoded frame size on the wire, including the 0x00 terminator.
pub const MAX_FRAME: usize = MAX_PAYLOAD + 2 + (MAX_PAYLOAD + 2).div_ceil(254) + 2;

/// Max analog samples carried by one telemetry frame.
pub const MAX_ANALOG_SAMPLES: usize = 8;
/// Number of firmware behavior slots.
pub const BEHAVIOR_SLOTS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PinMode {
    Disabled,
    Input,
    InputPullUp,
    InputPullDown,
    Output,
    OutputOpenDrain,
    Pwm,
    Analog,
}

impl PinMode {
    pub fn is_input(self) -> bool {
        matches!(self, PinMode::Input | PinMode::InputPullUp | PinMode::InputPullDown)
    }

    pub fn is_output(self) -> bool {
        matches!(self, PinMode::Output | PinMode::OutputOpenDrain | PinMode::Pwm)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChipKind {
    Esp32,
    Esp32S2,
    Esp32S3,
    Esp32C3,
    Esp32C5,
    Esp32C6,
    Esp32H2,
    Simulated,
    Other,
}

impl ChipKind {
    pub fn name(self) -> &'static str {
        match self {
            ChipKind::Esp32 => "ESP32",
            ChipKind::Esp32S2 => "ESP32-S2",
            ChipKind::Esp32S3 => "ESP32-S3",
            ChipKind::Esp32C3 => "ESP32-C3",
            ChipKind::Esp32C5 => "ESP32-C5",
            ChipKind::Esp32C6 => "ESP32-C6",
            ChipKind::Esp32H2 => "ESP32-H2",
            ChipKind::Simulated => "Simulated",
            ChipKind::Other => "Unknown",
        }
    }
}

/// On-device logic that keeps running without host round-trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Behavior {
    /// Toggle an output pin every `period_ms / 2`.
    Blink { pin: u8, period_ms: u16 },
    /// Triangle-wave PWM sweep on an output pin.
    Breathe { pin: u8, period_ms: u16 },
    /// Copy a (debounced) input level to an output pin.
    Mirror { from: u8, to: u8, invert: bool },
    /// Custom debounce window for one input pin.
    Watch { pin: u8, debounce_ms: u8 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HostMsg {
    Hello { proto: u8 },
    /// Return every pin to `Disabled` and clear all behaviors.
    Reset,
    Ping { seq: u32 },
    SetPinMode { pin: u8, mode: PinMode },
    WriteDigital { pin: u8, high: bool },
    /// `duty_permille` is 0..=1000.
    SetPwm { pin: u8, freq_hz: u32, duty_permille: u16 },
    /// One-shot analog read; the device answers with `AnalogValue`.
    ReadAnalog { pin: u8 },
    /// Periodic analog sampling; `interval_ms == 0` disables.
    WatchAnalog { pin: u8, interval_ms: u16 },
    /// Digital telemetry period; `interval_ms == 0` disables.
    SetTelemetry { interval_ms: u16 },
    AttachBehavior { slot: u8, behavior: Behavior },
    DetachBehavior { slot: u8 },
    /// Drive a WS2812-style addressable LED on `pin`. Appended last so the
    /// wire indices of every earlier variant stay stable.
    SetRgb { pin: u8, r: u8, g: u8, b: u8 },
    /// Bring up UART1 on arbitrary pins; `baud == 0` tears it down.
    UartConfig { tx: u8, rx: u8, baud: u32 },
    /// Transmit raw bytes on UART1.
    UartWrite { data: heapless::Vec<u8, UART_CHUNK> },
    /// Bring up an ST7735-style SPI display. `bl == 255` = no backlight pin.
    LcdInit { sck: u8, mosi: u8, cs: u8, dc: u8, rst: u8, bl: u8 },
    /// Fill the whole screen with an RGB565 color.
    LcdClear { rgb565: u16 },
    /// Fill a rectangle (coordinates in display pixels).
    LcdRect { x: u8, y: u8, w: u8, h: u8, rgb565: u16 },
    /// Draw text at a pixel position (6x10 font).
    LcdText { x: u8, y: u8, rgb565: u16, text: heapless::String<32> },
    /// Generic SPI bus on SPI2 (replaces the LCD if one was configured).
    SpiConfig { sck: u8, mosi: u8, miso: u8, freq_khz: u32 },
    /// Full-duplex transfer with `cs` asserted; the read bytes come back
    /// as `SpiData`.
    SpiTransfer { cs: u8, data: heapless::Vec<u8, UART_CHUNK> },
    /// I2C master on any pins.
    I2cConfig { sda: u8, scl: u8, freq_khz: u32 },
    I2cWrite { addr: u8, data: heapless::Vec<u8, UART_CHUNK> },
    /// Optional register write before reading `len` bytes (`reg == 256` = none).
    I2cRead { addr: u8, reg: u16, len: u8 },
    /// Join a Wi-Fi network as a station; empty `ssid` tears the radio down.
    WifiConfig { ssid: heapless::String<32>, pass: heapless::String<64> },
    /// Ask for the current `WifiStatus`.
    WifiStatusReq,
}

/// Max payload bytes per UART frame in either direction.
pub const UART_CHUNK: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventEdge {
    Rising,
    Falling,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalogSample {
    pub pin: u8,
    pub millivolts: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    BadPin,
    BadMode,
    BadValue,
    Unsupported,
    NoFreeSlot,
    Decode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DeviceMsg {
    HelloAck {
        proto: u8,
        fw_version: u16,
        chip: ChipKind,
        /// Bit N set = GPIO N exists and is controllable.
        gpio_mask: u64,
        /// Bit N set = GPIO N is input-only.
        input_only_mask: u64,
    },
    Pong { seq: u32 },
    /// Digital snapshot; bit N of `levels` = current level of GPIO N.
    Telemetry {
        millis: u32,
        levels: u64,
        analog: heapless::Vec<AnalogSample, MAX_ANALOG_SAMPLES>,
    },
    /// Debounced edge on an input pin.
    Event { millis: u32, pin: u8, edge: EventEdge },
    AnalogValue { pin: u8, millivolts: u16 },
    Error { code: ErrorCode, pin: u8 },
    Log { msg: heapless::String<64> },
    /// Bytes received on UART1 (appended last for wire stability).
    UartData { data: heapless::Vec<u8, UART_CHUNK> },
    /// Reply to `SpiTransfer`: what the device clocked back.
    SpiData { data: heapless::Vec<u8, UART_CHUNK> },
    /// Reply to `I2cRead`.
    I2cData { addr: u8, data: heapless::Vec<u8, UART_CHUNK> },
    /// Wi-Fi station state, pushed on change and on `WifiStatusReq`.
    WifiStatus { state: WifiState, ip: [u8; 4] },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WifiState {
    Off,
    Connecting,
    /// Associated and holding the IP in `WifiStatus::ip`.
    Connected,
    Failed,
}

/// TCP port the firmware listens on for the WireLab link over Wi-Fi.
pub const TCP_LINK_PORT: u16 = 4518;
/// UDP port for discovery beacons broadcast by the firmware.
pub const DISCOVERY_PORT: u16 = 4519;

/// Firmware version reported in `HelloAck`, `major << 8 | minor`.
pub const FW_VERSION: u16 = 0x0001;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Decoder, encode};

    #[test]
    fn roundtrip_host_msgs() {
        let msgs = [
            HostMsg::Hello { proto: PROTO_VERSION },
            HostMsg::SetPinMode { pin: 4, mode: PinMode::InputPullUp },
            HostMsg::SetPwm { pin: 2, freq_hz: 5000, duty_permille: 333 },
            HostMsg::AttachBehavior { slot: 1, behavior: Behavior::Blink { pin: 2, period_ms: 500 } },
            HostMsg::SetRgb { pin: 27, r: 255, g: 80, b: 10 },
            HostMsg::UartConfig { tx: 4, rx: 5, baud: 115_200 },
            HostMsg::UartWrite {
                data: heapless::Vec::from_slice(b"hello uart\r\n").unwrap(),
            },
            HostMsg::LcdInit { sck: 6, mosi: 7, cs: 8, dc: 9, rst: 10, bl: 255 },
            HostMsg::LcdText {
                x: 4,
                y: 20,
                rgb565: 0xFFE0,
                text: heapless::String::try_from("hello lcd").unwrap(),
            },
            HostMsg::SpiConfig { sck: 6, mosi: 7, miso: 2, freq_khz: 1000 },
            HostMsg::SpiTransfer {
                cs: 8,
                data: heapless::Vec::from_slice(&[0x9f, 0, 0]).unwrap(),
            },
            HostMsg::I2cConfig { sda: 0, scl: 1, freq_khz: 400 },
            HostMsg::I2cRead { addr: 0x76, reg: 0xd0, len: 1 },
            HostMsg::WifiConfig {
                ssid: heapless::String::try_from("shadownet").unwrap(),
                pass: heapless::String::try_from("hunter2hunter2").unwrap(),
            },
            HostMsg::WifiStatusReq,
        ];
        let mut buf = [0u8; MAX_FRAME];
        let mut dec: Decoder<HostMsg> = Decoder::new();
        for msg in &msgs {
            let n = encode(msg, &mut buf).unwrap();
            let mut out = None;
            for &b in &buf[..n] {
                if let Some(res) = dec.push(b) {
                    out = Some(res.unwrap());
                }
            }
            assert_eq!(out.as_ref(), Some(msg));
        }
    }

    #[test]
    fn roundtrip_telemetry_with_analog() {
        let mut analog = heapless::Vec::new();
        analog.push(AnalogSample { pin: 3, millivolts: 1650 }).unwrap();
        analog.push(AnalogSample { pin: 4, millivolts: 12 }).unwrap();
        let msg = DeviceMsg::Telemetry { millis: 123456, levels: 0b1010_0101, analog };
        let mut buf = [0u8; MAX_FRAME];
        let n = encode(&msg, &mut buf).unwrap();
        let mut dec: Decoder<DeviceMsg> = Decoder::new();
        let mut out = None;
        for &b in &buf[..n] {
            if let Some(res) = dec.push(b) {
                out = Some(res.unwrap());
            }
        }
        assert_eq!(out, Some(msg));
    }

    #[test]
    fn corrupt_frame_is_rejected() {
        let msg = HostMsg::Ping { seq: 42 };
        let mut buf = [0u8; MAX_FRAME];
        let n = encode(&msg, &mut buf).unwrap();
        buf[1] ^= 0xff;
        let mut dec: Decoder<HostMsg> = Decoder::new();
        let mut saw_err = false;
        for &b in &buf[..n] {
            if let Some(res) = dec.push(b) {
                saw_err = res.is_err();
            }
        }
        assert!(saw_err);
    }

    #[test]
    fn garbage_between_frames_is_skipped() {
        let msg = HostMsg::Reset;
        let mut buf = [0u8; MAX_FRAME];
        let n = encode(&msg, &mut buf).unwrap();
        let mut dec: Decoder<HostMsg> = Decoder::new();
        for &b in [0x13u8, 0x37, 0x00].iter() {
            let _ = dec.push(b);
        }
        let mut out = None;
        for &b in &buf[..n] {
            if let Some(res) = dec.push(b) {
                out = Some(res.unwrap());
            }
        }
        assert_eq!(out, Some(msg));
    }
}
