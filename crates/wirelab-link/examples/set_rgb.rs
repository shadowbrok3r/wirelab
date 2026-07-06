//! Set the board's WS2812 to a color: `set_rgb <port> <r> <g> <b> [gpio]`.

use std::time::Duration;

use wirelab_link::Device;
use wirelab_link::serial::{DEFAULT_BAUD, SerialDevice};
use wirelab_proto::{DeviceMsg, HostMsg, PROTO_VERSION};

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let port = args.next().ok_or("usage: set_rgb <port> <r> <g> <b> [gpio]")?;
    let mut byte = |d: u8| args.next().and_then(|s| s.parse().ok()).unwrap_or(d);
    let (r, g, b, pin) = (byte(60), byte(0), byte(90), byte(27));

    let mut dev = SerialDevice::open(&port, DEFAULT_BAUD).map_err(|e| e.to_string())?;
    // The port open may reset the board; handshake until it answers.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    'hello: while std::time::Instant::now() < deadline {
        dev.send(&HostMsg::Hello { proto: PROTO_VERSION }).map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(300));
        for msg in dev.poll() {
            if matches!(msg, DeviceMsg::HelloAck { .. }) {
                break 'hello;
            }
        }
    }
    dev.send(&HostMsg::SetRgb { pin, r, g, b }).map_err(|e| e.to_string())?;
    std::thread::sleep(Duration::from_millis(200));
    for msg in dev.poll() {
        if let DeviceMsg::Error { code, .. } = msg {
            return Err(format!("firmware rejected SetRgb: {code:?}"));
        }
    }
    println!("GPIO{pin} set to ({r}, {g}, {b})");
    Ok(())
}
