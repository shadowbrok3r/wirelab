//! Hardware-in-the-loop smoke test against a flashed WireLab device:
//! `cargo run -p wirelab-link --example hil_check -- /dev/ttyUSB0`

use std::time::{Duration, Instant};

use wirelab_link::Device;
use wirelab_link::serial::{DEFAULT_BAUD, SerialDevice};
use wirelab_proto::{DeviceMsg, HostMsg, PROTO_VERSION, PinMode};

fn wait_for<F: Fn(&DeviceMsg) -> bool>(
    dev: &mut SerialDevice,
    what: &str,
    timeout: Duration,
    pred: F,
) -> Result<DeviceMsg, String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for msg in dev.poll() {
            if pred(&msg) {
                return Ok(msg);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err(format!("timeout waiting for {what}"))
}

fn main() -> Result<(), String> {
    let port = std::env::args().nth(1).unwrap_or_else(|| "/dev/ttyUSB0".into());
    let test_gpio: u8 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2);

    println!("opening {port} @ {DEFAULT_BAUD}...");
    let mut dev = SerialDevice::open(&port, DEFAULT_BAUD).map_err(|e| e.to_string())?;
    // Opening the port may reset the board; give it time to boot, then retry.
    std::thread::sleep(Duration::from_millis(600));

    let mut hello_ok = None;
    for attempt in 1..=10 {
        dev.send(&HostMsg::Hello { proto: PROTO_VERSION }).map_err(|e| e.to_string())?;
        match wait_for(&mut dev, "HelloAck", Duration::from_millis(500), |m| {
            matches!(m, DeviceMsg::HelloAck { .. })
        }) {
            Ok(msg) => {
                hello_ok = Some(msg);
                break;
            }
            Err(_) if attempt < 10 => continue,
            Err(e) => return Err(e),
        }
    }
    let Some(DeviceMsg::HelloAck { proto, fw_version, chip, gpio_mask, .. }) = hello_ok else {
        return Err("no HelloAck".into());
    };
    println!(
        "✔ HelloAck: proto v{proto}, fw {}.{}, chip {}, gpio mask {gpio_mask:#014b}...",
        fw_version >> 8,
        fw_version & 0xff,
        chip.name()
    );

    dev.send(&HostMsg::Ping { seq: 42 }).map_err(|e| e.to_string())?;
    wait_for(&mut dev, "Pong", Duration::from_secs(1), |m| {
        matches!(m, DeviceMsg::Pong { seq: 42 })
    })?;
    println!("✔ Pong");

    dev.send(&HostMsg::SetTelemetry { interval_ms: 50 }).map_err(|e| e.to_string())?;
    wait_for(&mut dev, "Telemetry", Duration::from_secs(1), |m| {
        matches!(m, DeviceMsg::Telemetry { .. })
    })?;
    println!("✔ telemetry streaming");

    dev.send(&HostMsg::SetPinMode { pin: test_gpio, mode: PinMode::Output })
        .map_err(|e| e.to_string())?;
    dev.send(&HostMsg::WriteDigital { pin: test_gpio, high: true })
        .map_err(|e| e.to_string())?;
    wait_for(&mut dev, "GPIO high in telemetry", Duration::from_secs(1), |m| {
        matches!(m, DeviceMsg::Telemetry { levels, .. } if levels & (1 << test_gpio) != 0)
    })?;
    println!("✔ GPIO{test_gpio} reads high after WriteDigital(true)");

    dev.send(&HostMsg::WriteDigital { pin: test_gpio, high: false })
        .map_err(|e| e.to_string())?;
    wait_for(&mut dev, "GPIO low in telemetry", Duration::from_secs(1), |m| {
        matches!(m, DeviceMsg::Telemetry { levels, .. } if levels & (1 << test_gpio) == 0)
    })?;
    println!("✔ GPIO{test_gpio} reads low after WriteDigital(false)");

    // Analog: one-shot read plus watched samples in telemetry (value is
    // whatever the floating pin reads; presence is what we verify).
    let adc_gpio: u8 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    dev.send(&HostMsg::ReadAnalog { pin: adc_gpio }).map_err(|e| e.to_string())?;
    match wait_for(&mut dev, "AnalogValue", Duration::from_secs(1), |m| {
        matches!(m, DeviceMsg::AnalogValue { pin, .. } if *pin == adc_gpio)
    }) {
        Ok(DeviceMsg::AnalogValue { millivolts, .. }) => {
            println!("✔ one-shot ADC read on GPIO{adc_gpio}: {millivolts} mV (floating)");
        }
        Ok(_) => unreachable!(),
        Err(e) => return Err(e),
    }
    dev.send(&HostMsg::WatchAnalog { pin: adc_gpio, interval_ms: 100 })
        .map_err(|e| e.to_string())?;
    wait_for(&mut dev, "analog sample in telemetry", Duration::from_secs(1), |m| {
        matches!(m, DeviceMsg::Telemetry { analog, .. } if analog.iter().any(|s| s.pin == adc_gpio))
    })?;
    println!("✔ watched analog samples arrive in telemetry");

    dev.send(&HostMsg::AttachBehavior {
        slot: 0,
        behavior: wirelab_proto::Behavior::Blink { pin: test_gpio, period_ms: 100 },
    })
    .map_err(|e| e.to_string())?;
    let mut seen_high = false;
    let mut seen_low = false;
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline && !(seen_high && seen_low) {
        for msg in dev.poll() {
            if let DeviceMsg::Telemetry { levels, .. } = msg {
                if levels & (1 << test_gpio) != 0 {
                    seen_high = true;
                } else {
                    seen_low = true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if !(seen_high && seen_low) {
        return Err("blink behavior did not toggle the pin".into());
    }
    println!("✔ on-device Blink behavior toggles GPIO{test_gpio} autonomously");

    // WS2812 sweep on the on-board RGB LED (C5: GPIO27). An Unsupported
    // error back means the firmware predates the RMT driver.
    let rgb_gpio = 27u8;
    let mut rgb_ok = true;
    for (r, g, b) in [(80, 0, 0), (0, 80, 0), (0, 0, 80), (60, 0, 60), (0, 0, 0)] {
        dev.send(&HostMsg::SetRgb { pin: rgb_gpio, r, g, b }).map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(250));
        for msg in dev.poll() {
            if matches!(msg, DeviceMsg::Error { pin, .. } if pin == rgb_gpio) {
                rgb_ok = false;
            }
        }
    }
    if rgb_ok {
        println!("✔ WS2812 color sweep on GPIO{rgb_gpio} (watch the LED: R, G, B, purple, off)");
    } else {
        return Err("firmware rejected SetRgb — reflash with the RMT driver".into());
    }

    // UART1 bring-up: config + transmit must be accepted silently.
    dev.send(&HostMsg::UartConfig { tx: 4, rx: 5, baud: 115_200 })
        .map_err(|e| e.to_string())?;
    dev.send(&HostMsg::UartWrite {
        data: wirelab_proto::heapless::Vec::from_slice(b"hil uart smoke\r\n").unwrap(),
    })
    .map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_millis(300));
    for msg in dev.poll() {
        if let DeviceMsg::Error { code, pin } = msg {
            return Err(format!("UART smoke rejected: {code:?} pin {pin}"));
        }
    }
    println!("✔ UART1 configured on GPIO4/5 and transmitted (jumper 4↔5 to see echo)");

    // Generic SPI: transfer on a floating bus must clock and reply.
    dev.send(&HostMsg::SpiConfig { sck: 6, mosi: 7, miso: 2, freq_khz: 1000 })
        .map_err(|e| e.to_string())?;
    dev.send(&HostMsg::SpiTransfer {
        cs: 8,
        data: wirelab_proto::heapless::Vec::from_slice(&[0x9f, 0, 0]).unwrap(),
    })
    .map_err(|e| e.to_string())?;
    wait_for(&mut dev, "SpiData", Duration::from_secs(1), |m| {
        matches!(m, DeviceMsg::SpiData { data } if data.len() == 3)
    })?;
    println!("✔ SPI transfer clocked 3 bytes and replied (floating MISO)");

    // I2C: bus config succeeds; a read with no device NACKs -> BadValue,
    // which proves the bus actually ran a transaction.
    dev.send(&HostMsg::I2cConfig { sda: 0, scl: 1, freq_khz: 100 })
        .map_err(|e| e.to_string())?;
    dev.send(&HostMsg::I2cRead { addr: 0x76, reg: 0xd0, len: 1 })
        .map_err(|e| e.to_string())?;
    let mut i2c_ok = false;
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline && !i2c_ok {
        for msg in dev.poll() {
            match msg {
                DeviceMsg::I2cData { .. } => i2c_ok = true, // a device answered!
                DeviceMsg::Error { code: wirelab_proto::ErrorCode::BadValue, .. } => {
                    i2c_ok = true // expected NACK on an empty bus
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if !i2c_ok {
        return Err("I2C transaction produced neither data nor NACK".into());
    }
    println!("✔ I2C bus ran a transaction (NACK on empty bus is the expected reply)");

    // Wi-Fi: an unprovisioned board must report its radio as Off.
    dev.send(&HostMsg::WifiStatusReq).map_err(|e| e.to_string())?;
    match wait_for(&mut dev, "WifiStatus", Duration::from_secs(1), |m| {
        matches!(m, DeviceMsg::WifiStatus { .. })
    }) {
        Ok(DeviceMsg::WifiStatus { state, ip }) => {
            println!("✔ WifiStatus answered: {state:?}, ip {}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
        }
        Ok(_) => unreachable!(),
        Err(e) => return Err(e),
    }

    dev.send(&HostMsg::Reset).map_err(|e| e.to_string())?;
    println!("\nALL HARDWARE CHECKS PASSED");
    Ok(())
}
