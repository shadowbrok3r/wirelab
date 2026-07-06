//! WireLab firmware: a hot-reconfigurable pin server.
//!
//! The host owns all logic; this runtime executes pin configuration, digital
//! I/O, software PWM, debounced input events, telemetry and detached
//! behaviors — all switchable at runtime with no reflash.

#![no_std]
#![no_main]

#[cfg(feature = "wifi")]
extern crate alloc;

use esp_backtrace as _;
use esp_hal::gpio::{Flex, InputConfig, OutputConfig, Pull};
use esp_hal::main;
use esp_hal::time::Instant;
use wirelab_proto::frame::{Decoder, encode};
use wirelab_proto::{
    AnalogSample, BEHAVIOR_SLOTS, Behavior, ChipKind, DeviceMsg, ErrorCode, EventEdge, FW_VERSION,
    HostMsg, MAX_ANALOG_SAMPLES, MAX_FRAME, PROTO_VERSION, PinMode,
};

#[cfg(feature = "wifi")]
mod wifi;
#[cfg(feature = "wifi")]
use wirelab_proto::WifiState;

esp_bootloader_esp_idf::esp_app_desc!();

const MAX_PINS: usize = 64;
const DEBOUNCE_US: u64 = 5_000;
const INPUT_SCAN_US: u64 = 1_000;

#[cfg(feature = "esp32c3")]
const CHIP: ChipKind = ChipKind::Esp32C3;
#[cfg(feature = "esp32c5")]
const CHIP: ChipKind = ChipKind::Esp32C5;
#[cfg(feature = "esp32")]
const CHIP: ChipKind = ChipKind::Esp32;
#[cfg(feature = "esp32s3")]
const CHIP: ChipKind = ChipKind::Esp32S3;

struct SoftPwm {
    freq_hz: u32,
    duty_permille: u16,
    next_edge_us: u64,
    level: bool,
}

struct PinRt {
    mode: PinMode,
    out_high: bool,
    pwm: Option<SoftPwm>,
    stable: bool,
    candidate: bool,
    candidate_since_us: u64,
    seeded: bool,
    debounce_us: u64,
    analog_watched: bool,
}

impl Default for PinRt {
    fn default() -> Self {
        PinRt {
            mode: PinMode::Disabled,
            out_high: false,
            pwm: None,
            stable: false,
            candidate: false,
            candidate_since_us: 0,
            seeded: false,
            debounce_us: DEBOUNCE_US,
            analog_watched: false,
        }
    }
}

struct BehaviorRt {
    behavior: Behavior,
    next_toggle_us: u64,
    phase_start_us: u64,
}

fn pull_of(mode: PinMode) -> Pull {
    match mode {
        PinMode::InputPullUp => Pull::Up,
        PinMode::InputPullDown => Pull::Down,
        _ => Pull::None,
    }
}

/// GPIOs handed to the runtime; flash-connected pins stay `None`.
macro_rules! build_pins {
    ($p:ident; $($n:literal => $pin:ident),+ $(,)?) => {{
        let mut pins: [Option<Flex<'static>>; MAX_PINS] = [const { None }; MAX_PINS];
        $( pins[$n] = Some(Flex::new($p.$pin)); )+
        pins
    }};
}

#[cfg(feature = "esp32")]
const INPUT_ONLY_MASK: u64 = (1 << 34) | (1 << 35) | (1 << 36) | (1 << 39);
#[cfg(not(feature = "esp32"))]
const INPUT_ONLY_MASK: u64 = 0;

/// One-shot ADC reads on ADC1-capable pins.
#[cfg(any(feature = "esp32c3", feature = "esp32c5"))]
mod adc {
    use esp_hal::analog::adc::{Adc, AdcConfig, Attenuation};

    pub fn capable(gpio: u8) -> bool {
        #[cfg(feature = "esp32c3")]
        return (0..=4).contains(&gpio);
        #[cfg(feature = "esp32c5")]
        return (1..=6).contains(&gpio);
    }

    /// Blocking one-shot conversion, scaled to approximate millivolts (11 dB).
    ///
    /// The runtime only calls this for pins in `Analog` mode, so stealing the
    /// pin and ADC1 for the duration of one conversion cannot alias a live
    /// driver.
    pub fn read_millivolts(gpio: u8) -> Option<u16> {
        macro_rules! read_pin {
            ($($n:literal => $P:ident),+ $(,)?) => {
                match gpio {
                    $($n => {
                        let pin = unsafe { esp_hal::peripherals::$P::steal() };
                        let adc1 = unsafe { esp_hal::peripherals::ADC1::steal() };
                        let mut cfg = AdcConfig::new();
                        let mut apin = cfg.enable_pin(pin, Attenuation::_11dB);
                        let mut adc = Adc::new(adc1, cfg);
                        loop {
                            match adc.read_oneshot(&mut apin) {
                                Ok(raw) => break Some(((raw as u32 * 3100) / 4095) as u16),
                                Err(nb::Error::WouldBlock) => continue,
                                Err(_) => break None,
                            }
                        }
                    })+
                    _ => None,
                }
            };
        }
        #[cfg(feature = "esp32c3")]
        {
            read_pin!(0 => GPIO0, 1 => GPIO1, 2 => GPIO2, 3 => GPIO3, 4 => GPIO4)
        }
        #[cfg(feature = "esp32c5")]
        {
            read_pin!(1 => GPIO1, 2 => GPIO2, 3 => GPIO3, 4 => GPIO4, 5 => GPIO5, 6 => GPIO6)
        }
    }
}

#[cfg(not(any(feature = "esp32c3", feature = "esp32c5")))]
mod adc {
    pub fn capable(_gpio: u8) -> bool {
        false
    }

    pub fn read_millivolts(_gpio: u8) -> Option<u16> {
        None
    }
}

/// WS2812 addressable LED via RMT (the devkits' on-board RGB LED).
#[cfg(any(feature = "esp32c3", feature = "esp32c5"))]
mod rgb {
    use esp_hal::gpio::Level;
    use esp_hal::rmt::{PulseCode, Rmt, TxChannelConfig, TxChannelCreator};
    use esp_hal::time::Rate;

    /// Blocking single-LED write, GRB order, MSB first.
    ///
    /// RMT and the pin are stolen for the ~90 µs of one frame; the runtime
    /// never drives the RGB data pin as a plain GPIO at the same time.
    pub fn write(gpio: u8, r: u8, g: u8, b: u8) -> bool {
        let p = unsafe { esp_hal::peripherals::Peripherals::steal() };
        let Ok(rmt) = Rmt::new(p.RMT, Rate::from_mhz(80)) else { return false };
        // Divider 4: 20 MHz ticks, 50 ns each.
        let cfg = TxChannelConfig::default()
            .with_clk_divider(4)
            .with_idle_output(true)
            .with_idle_output_level(Level::Low);
        macro_rules! send {
            ($pin:expr) => {{
                let Ok(ch) = rmt.channel0.configure_tx(&cfg) else { return false };
                let ch = ch.with_pin($pin);
                let mut data = [PulseCode::default(); 26];
                let mut i = 0;
                for byte in [g, r, b] {
                    for bit in (0..8).rev() {
                        data[i] = if (byte >> bit) & 1 == 1 {
                            PulseCode::new(Level::High, 16, Level::Low, 9)
                        } else {
                            PulseCode::new(Level::High, 8, Level::Low, 17)
                        };
                        i += 1;
                    }
                }
                // Latch: >50 µs low.
                data[24] = PulseCode::new(Level::Low, 1200, Level::Low, 1200);
                data[25] = PulseCode::end_marker();
                match ch.transmit(&data) {
                    Ok(t) => t.wait().is_ok(),
                    Err(_) => false,
                }
            }};
        }
        match gpio {
            #[cfg(feature = "esp32c5")]
            27 => send!(p.GPIO27),
            #[cfg(feature = "esp32c3")]
            8 => send!(p.GPIO8),
            _ => false,
        }
    }
}

#[cfg(not(any(feature = "esp32c3", feature = "esp32c5")))]
mod rgb {
    pub fn write(_gpio: u8, _r: u8, _g: u8, _b: u8) -> bool {
        false
    }
}

/// Raw ST7735 driver over SPI2 (Adafruit 1.44" 128x128 and friends).
/// Minimal init + windowed fills; text via embedded-graphics' DrawTarget.
#[cfg(any(feature = "esp32c3", feature = "esp32c5"))]
mod lcd {
    use embedded_graphics::Pixel;
    use embedded_graphics::draw_target::DrawTarget;
    use embedded_graphics::geometry::{OriginDimensions, Size};
    use embedded_graphics::mono_font::MonoTextStyle;
    use embedded_graphics::mono_font::ascii::FONT_6X10;
    use embedded_graphics::pixelcolor::Rgb565;
    use embedded_graphics::pixelcolor::raw::RawU16;
    use embedded_graphics::prelude::*;
    use embedded_graphics::text::Text;
    use esp_hal::Blocking;
    use esp_hal::delay::Delay;
    use esp_hal::gpio::{Level, Output, OutputConfig};
    use esp_hal::spi::Mode;
    use esp_hal::spi::master::{Config, Spi};
    use esp_hal::time::Rate;

    // Panel offsets for the common 128x128 "green tab" glass.
    const COL_OFF: u8 = 2;
    const ROW_OFF: u8 = 3;

    pub struct Lcd {
        spi: Spi<'static, Blocking>,
        dc: Output<'static>,
        cs: Output<'static>,
        _rst: Output<'static>,
        _bl: Option<Output<'static>>,
    }

    pub fn open(sck: u8, mosi: u8, cs: u8, dc: u8, rst: u8, bl: u8) -> Option<Lcd> {
        let pins = [sck, mosi, cs, dc, rst];
        for (i, a) in pins.iter().enumerate() {
            if pins[i + 1..].contains(a) {
                return None;
            }
        }
        let p = unsafe { esp_hal::peripherals::Peripherals::steal() };
        let config = Config::default()
            .with_frequency(Rate::from_mhz(26))
            .with_mode(Mode::_0);
        let spi = Spi::new(p.SPI2, config)
            .ok()?
            .with_sck(super::uart1::steal_pin(sck)?)
            .with_mosi(super::uart1::steal_pin(mosi)?);
        let out = |n: u8, level: Level| -> Option<Output<'static>> {
            Some(Output::new(super::uart1::steal_pin(n)?, level, OutputConfig::default()))
        };
        let mut lcd = Lcd {
            spi,
            dc: out(dc, Level::Low)?,
            cs: out(cs, Level::High)?,
            _rst: {
                // Hardware reset pulse.
                let mut r = out(rst, Level::High)?;
                let delay = Delay::new();
                r.set_low();
                delay.delay_millis(50);
                r.set_high();
                delay.delay_millis(150);
                r
            },
            _bl: if bl == 255 { None } else { Some(out(bl, Level::High)?) },
        };
        lcd.init();
        Some(lcd)
    }

    impl Lcd {
        fn cmd(&mut self, c: u8, data: &[u8]) {
            self.cs.set_low();
            self.dc.set_low();
            let _ = self.spi.write(&[c]);
            if !data.is_empty() {
                self.dc.set_high();
                let _ = self.spi.write(data);
            }
            self.cs.set_high();
        }

        fn init(&mut self) {
            let delay = Delay::new();
            self.cmd(0x01, &[]); // SWRESET
            delay.delay_millis(150);
            self.cmd(0x11, &[]); // SLPOUT
            delay.delay_millis(150);
            self.cmd(0x3A, &[0x05]); // COLMOD: 16-bit
            self.cmd(0x36, &[0xC8]); // MADCTL: row/col order + BGR
            self.cmd(0x29, &[]); // DISPON
            delay.delay_millis(100);
        }

        fn window(&mut self, x0: u8, y0: u8, x1: u8, y1: u8) {
            self.cmd(0x2A, &[0, x0 + COL_OFF, 0, x1 + COL_OFF]);
            self.cmd(0x2B, &[0, y0 + ROW_OFF, 0, y1 + ROW_OFF]);
        }

        pub fn fill_rect(&mut self, x: u8, y: u8, w: u8, h: u8, rgb565: u16) {
            if w == 0 || h == 0 || x > 127 || y > 127 {
                return;
            }
            let x1 = (x as u16 + w as u16 - 1).min(127) as u8;
            let y1 = (y as u16 + h as u16 - 1).min(127) as u8;
            self.window(x, y, x1, y1);
            self.cmd(0x2C, &[]); // RAMWR
            self.cs.set_low();
            self.dc.set_high();
            let px = [(rgb565 >> 8) as u8, rgb565 as u8];
            let mut row = [0u8; 256];
            for chunk in row.chunks_exact_mut(2) {
                chunk.copy_from_slice(&px);
            }
            let count = (x1 - x + 1) as u32 * (y1 - y + 1) as u32;
            let mut left = count * 2;
            while left > 0 {
                let n = left.min(row.len() as u32) as usize;
                let _ = self.spi.write(&row[..n]);
                left -= n as u32;
            }
            self.cs.set_high();
        }

        pub fn clear(&mut self, rgb565: u16) {
            self.fill_rect(0, 0, 128, 128, rgb565);
        }

        pub fn text(&mut self, x: u8, y: u8, rgb565: u16, s: &str) {
            let color = Rgb565::from(RawU16::new(rgb565));
            let style = MonoTextStyle::new(&FONT_6X10, color);
            // FONT baseline: shift down so (x, y) is the glyph top-left.
            let _ = Text::new(s, Point::new(i32::from(x), i32::from(y) + 8), style)
                .draw(self);
        }
    }

    impl OriginDimensions for Lcd {
        fn size(&self) -> Size {
            Size::new(128, 128)
        }
    }

    impl DrawTarget for Lcd {
        type Color = Rgb565;
        type Error = core::convert::Infallible;

        fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
        where
            I: IntoIterator<Item = Pixel<Self::Color>>,
        {
            for Pixel(pt, color) in pixels {
                if (0..128).contains(&pt.x) && (0..128).contains(&pt.y) {
                    let v: RawU16 = color.into();
                    self.fill_rect(pt.x as u8, pt.y as u8, 1, 1, v.into_inner());
                }
            }
            Ok(())
        }
    }
}

#[cfg(not(any(feature = "esp32c3", feature = "esp32c5")))]
mod lcd {
    pub struct Lcd;

    pub fn open(_sck: u8, _mosi: u8, _cs: u8, _dc: u8, _rst: u8, _bl: u8) -> Option<Lcd> {
        None
    }

    impl Lcd {
        pub fn fill_rect(&mut self, _x: u8, _y: u8, _w: u8, _h: u8, _c: u16) {}
        pub fn clear(&mut self, _c: u16) {}
        pub fn text(&mut self, _x: u8, _y: u8, _c: u16, _s: &str) {}
    }
}

/// Generic SPI transfers on SPI2 with GPIO chip-selects; mutually
/// exclusive with the LCD (same bus) — configuring one drops the other.
#[cfg(any(feature = "esp32c3", feature = "esp32c5"))]
mod spibus {
    use esp_hal::Blocking;
    use esp_hal::gpio::{Level, Output, OutputConfig};
    use esp_hal::spi::Mode;
    use esp_hal::spi::master::{Config, Spi};
    use esp_hal::time::Rate;

    pub struct SpiBus {
        spi: Spi<'static, Blocking>,
    }

    pub fn open(sck: u8, mosi: u8, miso: u8, freq_khz: u32) -> Option<SpiBus> {
        if sck == mosi || sck == miso || mosi == miso {
            return None;
        }
        let p = unsafe { esp_hal::peripherals::Peripherals::steal() };
        let config = Config::default()
            .with_frequency(Rate::from_khz(freq_khz.clamp(10, 40_000)))
            .with_mode(Mode::_0);
        let spi = Spi::new(p.SPI2, config)
            .ok()?
            .with_sck(super::uart1::steal_pin(sck)?)
            .with_mosi(super::uart1::steal_pin(mosi)?)
            .with_miso(super::uart1::steal_pin(miso)?);
        Some(SpiBus { spi })
    }

    impl SpiBus {
        /// Full-duplex transfer with `cs` held low for its duration.
        pub fn transfer(&mut self, cs: u8, data: &mut [u8]) -> bool {
            let Some(pin) = super::uart1::steal_pin(cs) else { return false };
            let mut cs_out = Output::new(pin, Level::Low, OutputConfig::default());
            let ok = self.spi.transfer(data).is_ok();
            cs_out.set_high();
            ok
        }
    }
}

#[cfg(not(any(feature = "esp32c3", feature = "esp32c5")))]
mod spibus {
    pub struct SpiBus;

    pub fn open(_sck: u8, _mosi: u8, _miso: u8, _freq_khz: u32) -> Option<SpiBus> {
        None
    }

    impl SpiBus {
        pub fn transfer(&mut self, _cs: u8, _data: &mut [u8]) -> bool {
            false
        }
    }
}

/// I2C master on any pins.
#[cfg(any(feature = "esp32c3", feature = "esp32c5"))]
mod i2cbus {
    use esp_hal::Blocking;
    use esp_hal::i2c::master::{Config, I2c};
    use esp_hal::time::Rate;

    pub struct I2cBus {
        i2c: I2c<'static, Blocking>,
    }

    pub fn open(sda: u8, scl: u8, freq_khz: u32) -> Option<I2cBus> {
        if sda == scl {
            return None;
        }
        let p = unsafe { esp_hal::peripherals::Peripherals::steal() };
        let config =
            Config::default().with_frequency(Rate::from_khz(freq_khz.clamp(10, 1_000)));
        let i2c = I2c::new(p.I2C0, config)
            .ok()?
            .with_sda(super::uart1::steal_pin(sda)?)
            .with_scl(super::uart1::steal_pin(scl)?);
        Some(I2cBus { i2c })
    }

    impl I2cBus {
        pub fn write(&mut self, addr: u8, data: &[u8]) -> bool {
            self.i2c.write(addr, data).is_ok()
        }

        pub fn read(&mut self, addr: u8, reg: u16, buf: &mut [u8]) -> bool {
            if reg <= 255 {
                self.i2c.write_read(addr, &[reg as u8], buf).is_ok()
            } else {
                self.i2c.read(addr, buf).is_ok()
            }
        }
    }
}

#[cfg(not(any(feature = "esp32c3", feature = "esp32c5")))]
mod i2cbus {
    pub struct I2cBus;

    pub fn open(_sda: u8, _scl: u8, _freq_khz: u32) -> Option<I2cBus> {
        None
    }

    impl I2cBus {
        pub fn write(&mut self, _addr: u8, _data: &[u8]) -> bool {
            false
        }

        pub fn read(&mut self, _addr: u8, _reg: u16, _buf: &mut [u8]) -> bool {
            false
        }
    }
}

/// Host-configurable secondary UART on arbitrary broken-out pins
/// (RDM6300 tags, GPS modules, links to other boards...).
#[cfg(any(feature = "esp32c3", feature = "esp32c5"))]
mod uart1 {
    use esp_hal::Blocking;
    use esp_hal::gpio::{AnyPin, Pin};
    use esp_hal::uart::{Config, Uart};

    pub struct Uart1 {
        port: Uart<'static, Blocking>,
    }

    pub fn steal_pin(n: u8) -> Option<AnyPin<'static>> {
        any_pin(n)
    }

    fn any_pin(n: u8) -> Option<AnyPin<'static>> {
        macro_rules! pick {
            ($($num:literal => $P:ident),+ $(,)?) => {
                match n {
                    $($num => Some(unsafe { esp_hal::peripherals::$P::steal() }.degrade()),)+
                    _ => None,
                }
            };
        }
        #[cfg(feature = "esp32c5")]
        {
            pick!(0 => GPIO0, 1 => GPIO1, 2 => GPIO2, 3 => GPIO3, 4 => GPIO4,
                  5 => GPIO5, 6 => GPIO6, 7 => GPIO7, 8 => GPIO8, 9 => GPIO9,
                  10 => GPIO10, 23 => GPIO23, 24 => GPIO24, 25 => GPIO25,
                  26 => GPIO26, 27 => GPIO27, 28 => GPIO28)
        }
        #[cfg(feature = "esp32c3")]
        {
            pick!(0 => GPIO0, 1 => GPIO1, 2 => GPIO2, 3 => GPIO3, 4 => GPIO4,
                  5 => GPIO5, 6 => GPIO6, 7 => GPIO7, 8 => GPIO8, 9 => GPIO9,
                  10 => GPIO10, 18 => GPIO18, 19 => GPIO19, 20 => GPIO20, 21 => GPIO21)
        }
    }

    /// Steal-and-rebuild on every (re)config; the runtime keeps the pins
    /// otherwise untouched while a UART owns them.
    pub fn open(tx: u8, rx: u8, baud: u32) -> Option<Uart1> {
        if tx == rx {
            return None;
        }
        let p = unsafe { esp_hal::peripherals::Peripherals::steal() };
        let config = Config::default().with_baudrate(baud);
        let port = Uart::new(p.UART1, config)
            .ok()?
            .with_tx(any_pin(tx)?)
            .with_rx(any_pin(rx)?);
        Some(Uart1 { port })
    }

    impl Uart1 {
        pub fn write(&mut self, data: &[u8]) -> bool {
            let mut rest = data;
            while !rest.is_empty() {
                match self.port.write(rest) {
                    Ok(n) if n > 0 => rest = &rest[n..],
                    _ => return false,
                }
            }
            true
        }

        pub fn read_available(&mut self, buf: &mut [u8]) -> usize {
            self.port.read_buffered(buf).unwrap_or(0)
        }
    }
}

#[cfg(not(any(feature = "esp32c3", feature = "esp32c5")))]
mod uart1 {
    pub struct Uart1;

    pub fn open(_tx: u8, _rx: u8, _baud: u32) -> Option<Uart1> {
        None
    }

    impl Uart1 {
        pub fn write(&mut self, _data: &[u8]) -> bool {
            false
        }

        pub fn read_available(&mut self, _buf: &mut [u8]) -> usize {
            0
        }
    }
}

struct Transport {
    #[cfg(any(feature = "esp32c3", feature = "esp32s3"))]
    port: esp_hal::usb_serial_jtag::UsbSerialJtag<'static, esp_hal::Blocking>,
    #[cfg(any(feature = "esp32", feature = "esp32c5"))]
    port: esp_hal::uart::Uart<'static, esp_hal::Blocking>,
    /// TCP link over Wi-Fi; every reply and telemetry frame goes to both.
    #[cfg(feature = "wifi")]
    net: Option<wifi::Net>,
}

impl Transport {
    /// Non-blocking drain of whatever the host has sent.
    #[cfg(any(feature = "esp32", feature = "esp32c5"))]
    fn read_available(&mut self, buf: &mut [u8]) -> usize {
        self.port.read_buffered(buf).unwrap_or(0)
    }

    #[cfg(any(feature = "esp32c3", feature = "esp32s3"))]
    fn read_available(&mut self, buf: &mut [u8]) -> usize {
        let mut n = 0;
        while n < buf.len() {
            match self.port.read_byte() {
                Ok(b) => {
                    buf[n] = b;
                    n += 1;
                }
                Err(_) => break,
            }
        }
        n
    }

    fn send(&mut self, msg: &DeviceMsg) {
        let mut buf = [0u8; MAX_FRAME];
        if let Ok(n) = encode(msg, &mut buf) {
            let _ = embedded_io::Write::write_all(&mut self.port, &buf[..n]);
            let _ = embedded_io::Write::flush(&mut self.port);
            #[cfg(feature = "wifi")]
            if let Some(net) = &mut self.net {
                net.send_frame(&buf[..n]);
            }
        }
    }

    #[cfg(feature = "wifi")]
    fn wifi_status(&self, now_us: u64) -> DeviceMsg {
        match &self.net {
            Some(net) => DeviceMsg::WifiStatus { state: net.state(now_us), ip: net.ip() },
            None => DeviceMsg::WifiStatus { state: WifiState::Off, ip: [0; 4] },
        }
    }
}

struct Runtime {
    pins: [Option<Flex<'static>>; MAX_PINS],
    rt: [PinRt; MAX_PINS],
    behaviors: [Option<BehaviorRt>; BEHAVIOR_SLOTS],
    telemetry_us: u64,
    next_telemetry_us: u64,
    next_scan_us: u64,
    gpio_mask: u64,
    uart: Option<uart1::Uart1>,
    lcd: Option<lcd::Lcd>,
    spi: Option<spibus::SpiBus>,
    i2c: Option<i2cbus::I2cBus>,
}

impl Runtime {
    fn new(pins: [Option<Flex<'static>>; MAX_PINS]) -> Self {
        let mut gpio_mask = 0u64;
        for (i, p) in pins.iter().enumerate() {
            if p.is_some() {
                gpio_mask |= 1 << i;
            }
        }
        Runtime {
            uart: None,
            lcd: None,
            spi: None,
            i2c: None,
            pins,
            rt: core::array::from_fn(|_| PinRt::default()),
            behaviors: [const { None }; BEHAVIOR_SLOTS],
            telemetry_us: 0,
            next_telemetry_us: 0,
            next_scan_us: 0,
            gpio_mask,
        }
    }

    fn valid(&self, pin: u8) -> bool {
        (pin as usize) < MAX_PINS && self.gpio_mask & (1 << pin) != 0
    }

    fn set_mode(&mut self, pin: u8, mode: PinMode) -> Result<(), ErrorCode> {
        if !self.valid(pin) {
            return Err(ErrorCode::BadPin);
        }
        if mode.is_output() && INPUT_ONLY_MASK & (1 << pin) != 0 {
            return Err(ErrorCode::BadMode);
        }
        if mode == PinMode::Analog && !adc::capable(pin) {
            return Err(ErrorCode::Unsupported);
        }
        let idx = pin as usize;
        let flex = self.pins[idx].as_mut().unwrap();
        let rt = &mut self.rt[idx];
        rt.pwm = None;
        rt.out_high = false;
        rt.seeded = false;
        rt.debounce_us = DEBOUNCE_US;
        rt.analog_watched = false;
        match mode {
            PinMode::Disabled | PinMode::Analog => {
                flex.set_input_enable(false);
                flex.set_output_enable(false);
            }
            PinMode::Input | PinMode::InputPullUp | PinMode::InputPullDown => {
                flex.apply_input_config(&InputConfig::default().with_pull(pull_of(mode)));
                flex.set_output_enable(false);
                flex.set_input_enable(true);
            }
            PinMode::Output | PinMode::Pwm => {
                flex.apply_output_config(&OutputConfig::default());
                flex.set_low();
                flex.set_output_enable(true);
            }
            PinMode::OutputOpenDrain => {
                flex.apply_output_config(
                    &OutputConfig::default()
                        .with_drive_mode(esp_hal::gpio::DriveMode::OpenDrain),
                );
                flex.set_high();
                flex.set_output_enable(true);
            }
        }
        rt.mode = mode;
        Ok(())
    }

    fn write_digital(&mut self, pin: u8, high: bool) -> Result<(), ErrorCode> {
        if !self.valid(pin) {
            return Err(ErrorCode::BadPin);
        }
        let idx = pin as usize;
        if !self.rt[idx].mode.is_output() {
            self.set_mode(pin, PinMode::Output)?;
        }
        let rt = &mut self.rt[idx];
        if rt.mode == PinMode::Pwm {
            rt.pwm = None;
            rt.mode = PinMode::Output;
        }
        rt.out_high = high;
        let flex = self.pins[idx].as_mut().unwrap();
        if high {
            flex.set_high();
        } else {
            flex.set_low();
        }
        Ok(())
    }

    fn set_pwm(&mut self, pin: u8, freq_hz: u32, duty: u16, now_us: u64) -> Result<(), ErrorCode> {
        if !self.valid(pin) {
            return Err(ErrorCode::BadPin);
        }
        let idx = pin as usize;
        if !self.rt[idx].mode.is_output() {
            self.set_mode(pin, PinMode::Output)?;
        }
        let rt = &mut self.rt[idx];
        rt.mode = PinMode::Pwm;
        let freq = freq_hz.clamp(1, 20_000);
        rt.pwm = Some(SoftPwm {
            freq_hz: freq,
            duty_permille: duty.min(1000),
            next_edge_us: now_us,
            level: false,
        });
        Ok(())
    }

    fn handle(&mut self, msg: HostMsg, now_us: u64, out: &mut Transport) {
        let reply_err = |out: &mut Transport, code: ErrorCode, pin: u8| {
            out.send(&DeviceMsg::Error { code, pin });
        };
        match msg {
            HostMsg::Hello { .. } => out.send(&DeviceMsg::HelloAck {
                proto: PROTO_VERSION,
                fw_version: FW_VERSION,
                chip: CHIP,
                gpio_mask: self.gpio_mask,
                input_only_mask: INPUT_ONLY_MASK,
            }),
            HostMsg::Reset => {
                for pin in 0..MAX_PINS as u8 {
                    if self.valid(pin) {
                        let _ = self.set_mode(pin, PinMode::Disabled);
                    }
                }
                self.behaviors = [const { None }; BEHAVIOR_SLOTS];
                self.telemetry_us = 0;
                self.uart = None;
                self.lcd = None;
                self.spi = None;
                self.i2c = None;
            }
            HostMsg::Ping { seq } => out.send(&DeviceMsg::Pong { seq }),
            HostMsg::SetPinMode { pin, mode } => {
                if let Err(code) = self.set_mode(pin, mode) {
                    reply_err(out, code, pin);
                }
            }
            HostMsg::WriteDigital { pin, high } => {
                if let Err(code) = self.write_digital(pin, high) {
                    reply_err(out, code, pin);
                }
            }
            HostMsg::SetPwm { pin, freq_hz, duty_permille } => {
                if let Err(code) = self.set_pwm(pin, freq_hz, duty_permille, now_us) {
                    reply_err(out, code, pin);
                }
            }
            HostMsg::ReadAnalog { pin } => {
                if !self.valid(pin) {
                    reply_err(out, ErrorCode::BadPin, pin);
                } else if self.rt[pin as usize].mode != PinMode::Analog
                    && self.set_mode(pin, PinMode::Analog).is_err()
                {
                    reply_err(out, ErrorCode::Unsupported, pin);
                } else {
                    match adc::read_millivolts(pin) {
                        Some(mv) => out.send(&DeviceMsg::AnalogValue { pin, millivolts: mv }),
                        None => reply_err(out, ErrorCode::Unsupported, pin),
                    }
                }
            }
            HostMsg::WatchAnalog { pin, interval_ms } => {
                if !self.valid(pin) {
                    reply_err(out, ErrorCode::BadPin, pin);
                } else if interval_ms == 0 {
                    self.rt[pin as usize].analog_watched = false;
                } else if self.rt[pin as usize].mode == PinMode::Analog
                    || self.set_mode(pin, PinMode::Analog).is_ok()
                {
                    self.rt[pin as usize].analog_watched = true;
                } else {
                    reply_err(out, ErrorCode::Unsupported, pin);
                }
            }
            HostMsg::SetTelemetry { interval_ms } => {
                self.telemetry_us = u64::from(interval_ms) * 1000;
                self.next_telemetry_us = now_us;
            }
            HostMsg::AttachBehavior { slot, behavior } => {
                let idx = slot as usize;
                if idx >= BEHAVIOR_SLOTS {
                    reply_err(out, ErrorCode::NoFreeSlot, 0);
                    return;
                }
                if let Behavior::Watch { pin, debounce_ms } = behavior {
                    if self.valid(pin) {
                        self.rt[pin as usize].debounce_us = u64::from(debounce_ms) * 1000;
                    }
                }
                self.behaviors[idx] = Some(BehaviorRt {
                    behavior,
                    next_toggle_us: now_us,
                    phase_start_us: now_us,
                });
            }
            HostMsg::DetachBehavior { slot } => {
                let idx = slot as usize;
                if idx < BEHAVIOR_SLOTS {
                    if let Some(b) = self.behaviors[idx].take() {
                        match b.behavior {
                            Behavior::Blink { pin, .. } | Behavior::Breathe { pin, .. } => {
                                let _ = self.write_digital(pin, false);
                            }
                            _ => {}
                        }
                    }
                }
            }
            HostMsg::SetRgb { pin, r, g, b } => {
                if !rgb::write(pin, r, g, b) {
                    reply_err(out, ErrorCode::Unsupported, pin);
                }
            }
            HostMsg::UartConfig { tx, rx, baud } => {
                if baud == 0 {
                    self.uart = None;
                } else {
                    match uart1::open(tx, rx, baud) {
                        Some(u) => self.uart = Some(u),
                        None => reply_err(out, ErrorCode::Unsupported, tx),
                    }
                }
            }
            HostMsg::UartWrite { data } => match &mut self.uart {
                Some(u) => {
                    if !u.write(&data) {
                        reply_err(out, ErrorCode::BadValue, 0);
                    }
                }
                None => reply_err(out, ErrorCode::Unsupported, 0),
            },
            HostMsg::LcdInit { sck, mosi, cs, dc, rst, bl } => {
                self.spi = None; // LCD and generic SPI share SPI2.
                match lcd::open(sck, mosi, cs, dc, rst, bl) {
                    Some(l) => self.lcd = Some(l),
                    None => reply_err(out, ErrorCode::Unsupported, sck),
                }
            }
            HostMsg::SpiConfig { sck, mosi, miso, freq_khz } => {
                self.lcd = None; // shared bus
                match spibus::open(sck, mosi, miso, freq_khz) {
                    Some(b) => self.spi = Some(b),
                    None => reply_err(out, ErrorCode::Unsupported, sck),
                }
            }
            HostMsg::SpiTransfer { cs, data } => match &mut self.spi {
                Some(bus) => {
                    let mut buf = [0u8; wirelab_proto::UART_CHUNK];
                    let n = data.len();
                    buf[..n].copy_from_slice(&data);
                    if bus.transfer(cs, &mut buf[..n]) {
                        if let Ok(v) = wirelab_proto::heapless::Vec::from_slice(&buf[..n]) {
                            out.send(&DeviceMsg::SpiData { data: v });
                        }
                    } else {
                        reply_err(out, ErrorCode::BadValue, cs);
                    }
                }
                None => reply_err(out, ErrorCode::Unsupported, 0),
            },
            HostMsg::I2cConfig { sda, scl, freq_khz } => {
                match i2cbus::open(sda, scl, freq_khz) {
                    Some(b) => self.i2c = Some(b),
                    None => reply_err(out, ErrorCode::Unsupported, sda),
                }
            }
            HostMsg::I2cWrite { addr, data } => match &mut self.i2c {
                Some(bus) => {
                    if !bus.write(addr, &data) {
                        reply_err(out, ErrorCode::BadValue, addr);
                    }
                }
                None => reply_err(out, ErrorCode::Unsupported, 0),
            },
            HostMsg::I2cRead { addr, reg, len } => match &mut self.i2c {
                Some(bus) => {
                    let n = usize::from(len).min(wirelab_proto::UART_CHUNK);
                    let mut buf = [0u8; wirelab_proto::UART_CHUNK];
                    if bus.read(addr, reg, &mut buf[..n]) {
                        if let Ok(v) = wirelab_proto::heapless::Vec::from_slice(&buf[..n]) {
                            out.send(&DeviceMsg::I2cData { addr, data: v });
                        }
                    } else {
                        reply_err(out, ErrorCode::BadValue, addr);
                    }
                }
                None => reply_err(out, ErrorCode::Unsupported, 0),
            },
            HostMsg::LcdClear { rgb565 } => match &mut self.lcd {
                Some(l) => l.clear(rgb565),
                None => reply_err(out, ErrorCode::Unsupported, 0),
            },
            HostMsg::LcdRect { x, y, w, h, rgb565 } => match &mut self.lcd {
                Some(l) => l.fill_rect(x, y, w, h, rgb565),
                None => reply_err(out, ErrorCode::Unsupported, 0),
            },
            HostMsg::LcdText { x, y, rgb565, text } => match &mut self.lcd {
                Some(l) => l.text(x, y, rgb565, &text),
                None => reply_err(out, ErrorCode::Unsupported, 0),
            },
            #[cfg(feature = "wifi")]
            HostMsg::WifiConfig { ssid, pass } => {
                out.net = None; // release the radio before re-configuring
                if ssid.is_empty() {
                    out.send(&DeviceMsg::WifiStatus { state: WifiState::Off, ip: [0; 4] });
                } else {
                    match wifi::Net::connect(&ssid, &pass, now_us) {
                        Ok(net) => {
                            out.net = Some(net);
                            let status = out.wifi_status(now_us);
                            out.send(&status);
                        }
                        Err(()) => reply_err(out, ErrorCode::BadValue, 0),
                    }
                }
            }
            #[cfg(feature = "wifi")]
            HostMsg::WifiStatusReq => {
                let status = out.wifi_status(now_us);
                out.send(&status);
            }
            #[cfg(not(feature = "wifi"))]
            HostMsg::WifiConfig { .. } | HostMsg::WifiStatusReq => {
                reply_err(out, ErrorCode::Unsupported, 0);
            }
        }
    }

    /// Forward pending UART1 bytes to the host in protocol-sized chunks.
    fn pump_uart(&mut self, out: &mut Transport) {
        let Some(u) = &mut self.uart else { return };
        let mut buf = [0u8; wirelab_proto::UART_CHUNK];
        loop {
            let n = u.read_available(&mut buf);
            if n == 0 {
                break;
            }
            if let Ok(data) = wirelab_proto::heapless::Vec::from_slice(&buf[..n]) {
                out.send(&DeviceMsg::UartData { data });
            }
        }
    }

    /// Debounced input level for behaviors and telemetry.
    fn input_level(&self, pin: u8) -> bool {
        self.rt
            .get(pin as usize)
            .map(|r| r.stable)
            .unwrap_or(false)
    }

    fn run_behaviors(&mut self, now_us: u64) {
        for i in 0..BEHAVIOR_SLOTS {
            let Some(b) = &mut self.behaviors[i] else { continue };
            match b.behavior {
                Behavior::Blink { pin, period_ms } => {
                    if now_us >= b.next_toggle_us {
                        b.next_toggle_us =
                            now_us + u64::from(period_ms.max(20)) * 1000 / 2;
                        let high = !self.rt[pin as usize].out_high;
                        let _ = self.write_digital(pin, high);
                    }
                }
                Behavior::Breathe { pin, period_ms } => {
                    if now_us >= b.next_toggle_us {
                        b.next_toggle_us = now_us + 20_000;
                        let period = u64::from(period_ms.max(100)) * 1000;
                        let t = ((now_us - b.phase_start_us) % period) as f32 / period as f32;
                        let duty = if t < 0.5 { t * 2.0 } else { 2.0 - t * 2.0 };
                        let _ = self.set_pwm(pin, 500, (duty * 1000.0) as u16, now_us);
                    }
                }
                Behavior::Mirror { from, to, invert } => {
                    if now_us >= b.next_toggle_us {
                        b.next_toggle_us = now_us + 2_000;
                        let level = self.input_level(from) ^ invert;
                        if self.rt[to as usize].out_high != level
                            || self.rt[to as usize].mode == PinMode::Disabled
                        {
                            let _ = self.write_digital(to, level);
                        }
                    }
                }
                Behavior::Watch { .. } => {}
            }
        }
    }

    fn run_pwm(&mut self, now_us: u64) {
        for idx in 0..MAX_PINS {
            let Some(flex) = self.pins[idx].as_mut() else { continue };
            let rt = &mut self.rt[idx];
            let Some(pwm) = &mut rt.pwm else { continue };
            if now_us < pwm.next_edge_us {
                continue;
            }
            let period_us = 1_000_000u64 / u64::from(pwm.freq_hz);
            let high_us = period_us * u64::from(pwm.duty_permille) / 1000;
            if pwm.duty_permille == 0 {
                flex.set_low();
                pwm.level = false;
                pwm.next_edge_us = now_us + period_us;
            } else if pwm.duty_permille >= 1000 {
                flex.set_high();
                pwm.level = true;
                pwm.next_edge_us = now_us + period_us;
            } else if pwm.level {
                flex.set_low();
                pwm.level = false;
                pwm.next_edge_us += period_us - high_us;
            } else {
                flex.set_high();
                pwm.level = true;
                pwm.next_edge_us += high_us;
            }
            rt.out_high = pwm.level;
        }
    }

    fn scan_inputs(&mut self, now_us: u64, out: &mut Transport) {
        if now_us < self.next_scan_us {
            return;
        }
        self.next_scan_us = now_us + INPUT_SCAN_US;
        for idx in 0..MAX_PINS {
            let Some(flex) = self.pins[idx].as_mut() else { continue };
            let rt = &mut self.rt[idx];
            if !rt.mode.is_input() {
                continue;
            }
            let raw = flex.is_high();
            if !rt.seeded {
                rt.stable = raw;
                rt.candidate = raw;
                rt.seeded = true;
                continue;
            }
            if raw != rt.candidate {
                rt.candidate = raw;
                rt.candidate_since_us = now_us;
            } else if raw != rt.stable && now_us - rt.candidate_since_us >= rt.debounce_us {
                rt.stable = raw;
                out.send(&DeviceMsg::Event {
                    millis: (now_us / 1000) as u32,
                    pin: idx as u8,
                    edge: if raw { EventEdge::Rising } else { EventEdge::Falling },
                });
            }
        }
    }

    fn telemetry(&mut self, now_us: u64, out: &mut Transport) {
        if self.telemetry_us == 0 || now_us < self.next_telemetry_us {
            return;
        }
        self.next_telemetry_us = now_us + self.telemetry_us;
        let mut levels = 0u64;
        for idx in 0..MAX_PINS {
            let rt = &self.rt[idx];
            let high = match rt.mode {
                m if m.is_input() => rt.stable,
                PinMode::Pwm => rt.pwm.as_ref().is_some_and(|p| p.duty_permille > 500),
                m if m.is_output() => rt.out_high,
                _ => false,
            };
            if high {
                levels |= 1 << idx;
            }
        }
        let mut analog = heapless::Vec::<AnalogSample, MAX_ANALOG_SAMPLES>::new();
        for idx in 0..MAX_PINS {
            if self.rt[idx].mode == PinMode::Analog && self.rt[idx].analog_watched {
                if let Some(mv) = adc::read_millivolts(idx as u8) {
                    let _ = analog.push(AnalogSample { pin: idx as u8, millivolts: mv });
                }
            }
        }
        out.send(&DeviceMsg::Telemetry { millis: (now_us / 1000) as u32, levels, analog });
    }
}

#[main]
fn main() -> ! {
    let p = esp_hal::init(esp_hal::Config::default());

    #[cfg(feature = "wifi")]
    wifi::start_scheduler(p.TIMG0, p.SW_INTERRUPT);

    #[cfg(any(feature = "esp32c3", feature = "esp32s3"))]
    let mut transport = Transport {
        port: esp_hal::usb_serial_jtag::UsbSerialJtag::new(p.USB_DEVICE),
        #[cfg(feature = "wifi")]
        net: None,
    };
    #[cfg(any(feature = "esp32", feature = "esp32c5"))]
    let mut transport = {
        let config = esp_hal::uart::Config::default().with_baudrate(115_200);
        let uart = esp_hal::uart::Uart::new(p.UART0, config).unwrap();
        #[cfg(feature = "esp32")]
        let uart = uart.with_rx(p.GPIO3).with_tx(p.GPIO1);
        #[cfg(feature = "esp32c5")]
        let uart = uart.with_rx(p.GPIO12).with_tx(p.GPIO11);
        Transport {
            port: uart,
            #[cfg(feature = "wifi")]
            net: None,
        }
    };

    #[cfg(feature = "esp32c3")]
    let pins = build_pins!(p;
        0 => GPIO0, 1 => GPIO1, 2 => GPIO2, 3 => GPIO3, 4 => GPIO4, 5 => GPIO5,
        6 => GPIO6, 7 => GPIO7, 8 => GPIO8, 9 => GPIO9, 10 => GPIO10,
        18 => GPIO18, 19 => GPIO19, 20 => GPIO20, 21 => GPIO21,
    );
    // C5: GPIO11/12 are the UART0 console (our transport), 13/14 USB D-/D+,
    // 16-22 in-package flash.
    #[cfg(feature = "esp32c5")]
    let pins = build_pins!(p;
        0 => GPIO0, 1 => GPIO1, 2 => GPIO2, 3 => GPIO3, 4 => GPIO4, 5 => GPIO5,
        6 => GPIO6, 7 => GPIO7, 8 => GPIO8, 9 => GPIO9, 10 => GPIO10,
        23 => GPIO23, 24 => GPIO24, 25 => GPIO25, 26 => GPIO26,
        27 => GPIO27, 28 => GPIO28,
    );
    #[cfg(feature = "esp32")]
    let pins = build_pins!(p;
        0 => GPIO0, 2 => GPIO2, 4 => GPIO4, 5 => GPIO5, 12 => GPIO12, 13 => GPIO13,
        14 => GPIO14, 15 => GPIO15, 16 => GPIO16, 17 => GPIO17, 18 => GPIO18,
        19 => GPIO19, 21 => GPIO21, 22 => GPIO22, 23 => GPIO23, 25 => GPIO25,
        26 => GPIO26, 27 => GPIO27, 32 => GPIO32, 33 => GPIO33, 34 => GPIO34,
        35 => GPIO35, 36 => GPIO36, 39 => GPIO39,
    );
    #[cfg(feature = "esp32s3")]
    let pins = build_pins!(p;
        0 => GPIO0, 1 => GPIO1, 2 => GPIO2, 3 => GPIO3, 4 => GPIO4, 5 => GPIO5,
        6 => GPIO6, 7 => GPIO7, 8 => GPIO8, 9 => GPIO9, 10 => GPIO10, 11 => GPIO11,
        12 => GPIO12, 13 => GPIO13, 14 => GPIO14, 15 => GPIO15, 16 => GPIO16,
        17 => GPIO17, 18 => GPIO18, 21 => GPIO21, 38 => GPIO38, 39 => GPIO39,
        40 => GPIO40, 41 => GPIO41, 42 => GPIO42, 45 => GPIO45, 46 => GPIO46,
        47 => GPIO47, 48 => GPIO48,
    );

    let mut runtime = Runtime::new(pins);
    let mut decoder: Decoder<HostMsg> = Decoder::new();
    let boot = Instant::now();

    let mut rx = [0u8; 128];
    loop {
        let now_us = (Instant::now() - boot).as_micros();
        let n = transport.read_available(&mut rx);
        for &byte in &rx[..n] {
            match decoder.push(byte) {
                Some(Ok(msg)) => runtime.handle(msg, now_us, &mut transport),
                Some(Err(_)) => {
                    transport.send(&DeviceMsg::Error { code: ErrorCode::Decode, pin: 0 })
                }
                None => {}
            }
        }
        #[cfg(feature = "wifi")]
        {
            let mut net_msgs = heapless::Vec::<HostMsg, 8>::new();
            let mut status_changed = false;
            if let Some(net) = &mut transport.net {
                status_changed = net.poll(now_us, CHIP.name(), &mut net_msgs);
            }
            if status_changed {
                let status = transport.wifi_status(now_us);
                transport.send(&status);
            }
            for msg in net_msgs {
                runtime.handle(msg, now_us, &mut transport);
            }
        }
        runtime.run_pwm(now_us);
        runtime.scan_inputs(now_us, &mut transport);
        runtime.run_behaviors(now_us);
        runtime.telemetry(now_us, &mut transport);
        runtime.pump_uart(&mut transport);
    }
}
