//! Provision a board's Wi-Fi over serial and watch the join:
//! `cargo run -p wirelab-link --example wifi_probe -- /dev/ttyUSB0 <ssid> [pass]`
//! With no ssid, sends an empty config (radio off) and reports status.

use std::time::{Duration, Instant};

use wirelab_link::Device;
use wirelab_link::serial::{DEFAULT_BAUD, SerialDevice};
use wirelab_proto::{DeviceMsg, HostMsg, PROTO_VERSION, WifiState};

fn main() -> Result<(), String> {
    let port = std::env::args().nth(1).unwrap_or_else(|| "/dev/ttyUSB0".into());
    let ssid = std::env::args().nth(2).unwrap_or_default();
    let pass = std::env::args().nth(3).unwrap_or_default();

    let mut dev = SerialDevice::open(&port, DEFAULT_BAUD).map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_millis(600));

    let mut ready = false;
    for _ in 0..10 {
        dev.send(&HostMsg::Hello { proto: PROTO_VERSION }).map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline && !ready {
            for msg in dev.poll() {
                if matches!(msg, DeviceMsg::HelloAck { .. }) {
                    ready = true;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if ready {
            break;
        }
    }
    if !ready {
        return Err("no HelloAck".into());
    }
    println!("✔ hello");

    dev.send(&HostMsg::WifiConfig {
        ssid: wirelab_proto::heapless::String::try_from(ssid.as_str())
            .map_err(|_| "ssid too long (max 32)")?,
        pass: wirelab_proto::heapless::String::try_from(pass.as_str())
            .map_err(|_| "password too long (max 64)")?,
    })
    .map_err(|e| e.to_string())?;
    println!("→ WifiConfig sent (ssid: '{ssid}')");

    let mut last: Option<(WifiState, [u8; 4])> = None;
    let mut seq = 0u32;
    let mut next_ping = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        // The link must stay responsive while the radio works.
        if Instant::now() >= next_ping {
            next_ping = Instant::now() + Duration::from_secs(2);
            seq += 1;
            dev.send(&HostMsg::Ping { seq }).map_err(|e| e.to_string())?;
            dev.send(&HostMsg::WifiStatusReq).map_err(|e| e.to_string())?;
        }
        for msg in dev.poll() {
            match msg {
                DeviceMsg::WifiStatus { state, ip } => {
                    if last != Some((state, ip)) {
                        println!(
                            "  wifi: {state:?} ip {}.{}.{}.{}",
                            ip[0], ip[1], ip[2], ip[3]
                        );
                        last = Some((state, ip));
                    }
                    if state == WifiState::Connected && ip != [0; 4] {
                        println!("✔ joined — connect with backend 'Wi-Fi (TCP)' to {}.{}.{}.{}:4518",
                            ip[0], ip[1], ip[2], ip[3]);
                        return Ok(());
                    }
                    if ssid.is_empty() && state == WifiState::Off {
                        println!("✔ radio off");
                        return Ok(());
                    }
                    if state == WifiState::Failed {
                        println!(
                            "join FAILED (bad credentials or AP out of range) — the board \
                             keeps retrying every 20 s; serial stayed alive"
                        );
                        return Ok(());
                    }
                }
                DeviceMsg::Pong { seq } => println!("  pong {seq} (serial link alive)"),
                DeviceMsg::Error { code, pin } => println!("  device error {code:?} pin {pin}"),
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(format!("no terminal wifi state within 30 s (last: {last:?})"))
}
