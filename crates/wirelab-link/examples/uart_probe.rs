//! Live UART probe: claim UART1 on the given pins and hex-dump whatever the
//! attached device sends — first-contact bring-up for serial sensors.
//!
//! `cargo run -p wirelab-link --example uart_probe -- /dev/ttyACM0 18 17 256000 [secs]`
//!
//! (tx rx from the ESP's perspective: tx is our transmit pin wired to the
//! sensor's RX. Counts HLK-LD2450 data frames as a bonus when it sees them.)

use std::time::{Duration, Instant};

use wirelab_link::Device;
use wirelab_link::serial::{DEFAULT_BAUD, SerialDevice};
use wirelab_proto::{DeviceMsg, HostMsg, PROTO_VERSION};

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let port = args.next().unwrap_or_else(|| "/dev/ttyACM0".into());
    let tx: u8 = args.next().and_then(|s| s.parse().ok()).unwrap_or(18);
    let rx: u8 = args.next().and_then(|s| s.parse().ok()).unwrap_or(17);
    let baud: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(256_000);
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(6);

    println!("opening {port} @ {DEFAULT_BAUD}...");
    let mut dev = SerialDevice::open(&port, DEFAULT_BAUD).map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_millis(600));

    let mut hello = None;
    for attempt in 1..=10 {
        dev.send(&HostMsg::Hello { proto: PROTO_VERSION }).map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_millis(500);
        'wait: while Instant::now() < deadline {
            for msg in dev.poll() {
                if let DeviceMsg::HelloAck { chip, fw_version, .. } = msg {
                    hello = Some((chip, fw_version));
                    break 'wait;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if hello.is_some() || attempt == 10 {
            break;
        }
    }
    let (chip, fw) = hello.ok_or("no HelloAck — is wirelab-fw flashed?")?;
    println!("✔ {} fw {}.{}", chip.name(), fw >> 8, fw & 0xff);

    dev.send(&HostMsg::UartConfig { tx, rx, baud }).map_err(|e| e.to_string())?;
    println!("UART1 tx=GPIO{tx} rx=GPIO{rx} @ {baud} — listening {secs}s...");

    let mut total = 0usize;
    let mut stream: Vec<u8> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        for msg in dev.poll() {
            match msg {
                DeviceMsg::UartData { data } => {
                    total += data.len();
                    stream.extend_from_slice(&data);
                    let hex: String = data.iter().map(|b| format!("{b:02x}")).collect();
                    println!("  rx[{}]: {hex}", data.len());
                }
                DeviceMsg::Error { code, pin } => {
                    return Err(format!("device rejected: {code:?} (pin {pin})"));
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    dev.send(&HostMsg::UartConfig { tx, rx, baud: 0 }).ok();

    let ld2450_frames = stream
        .windows(4)
        .filter(|w| *w == [0xAA, 0xFF, 0x03, 0x00])
        .count();
    println!("\n{total} bytes total");
    if ld2450_frames > 0 {
        println!("looks like an HLK-LD2450: {ld2450_frames} data-frame headers seen");
    }
    if total == 0 {
        return Err("no bytes — check wiring (device TX -> our rx pin), power, baud".into());
    }
    Ok(())
}
