//! Fake board over TCP: serves a `SimDevice` with the real component
//! library so network clients can be tested without hardware.
//!
//! ```sh
//! cargo run -p wirelab-link --example board_server -- [--port 4518] [--ip 127.0.0.1] [--no-beacon]
//! ```

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::Path;
use std::time::Duration;

use wirelab_core::circuit::Circuit;
use wirelab_core::library::Library;
use wirelab_link::Device;
use wirelab_link::sim::SimDevice;
use wirelab_proto::frame::{Decoder, encode};
use wirelab_proto::{DISCOVERY_PORT, HostMsg, MAX_FRAME, TCP_LINK_PORT};

const BOARD_ID: &str = "esp32-c5-devkitc-1";

fn main() {
    let mut port = TCP_LINK_PORT;
    let mut ip = String::from("127.0.0.1");
    let mut beacon = true;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                port = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--port needs a number");
            }
            "--ip" => ip = args.next().expect("--ip needs an address"),
            "--no-beacon" => beacon = false,
            other => {
                eprintln!("unknown argument: {other}");
                eprintln!("usage: board_server [--port 4518] [--ip 127.0.0.1] [--no-beacon]");
                std::process::exit(2);
            }
        }
    }

    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    let lib = Library::load(&assets.join("boards"), &assets.join("components"))
        .expect("load asset library");
    let board = lib.board(BOARD_ID).expect("board profile").clone();
    let mut dev = SimDevice::new(board, lib, Circuit::new(BOARD_ID));

    if beacon {
        spawn_beacon(ip.clone(), port);
    }

    let listener = TcpListener::bind(("0.0.0.0", port)).expect("bind tcp listener");
    println!("board_server: simulated {BOARD_ID} listening on {ip}:{port}");
    loop {
        let (stream, peer) = match listener.accept() {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("accept failed: {e}");
                continue;
            }
        };
        println!("client connected: {peer}");
        match serve_client(stream, &mut dev) {
            Ok(()) => println!("client disconnected: {peer}"),
            Err(e) => println!("client dropped: {peer} ({e})"),
        }
    }
}

/// Bridge one client: socket bytes -> HostMsg -> device, device -> socket.
fn serve_client(mut stream: TcpStream, dev: &mut SimDevice) -> std::io::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_millis(10)))?;
    let mut decoder: Decoder<HostMsg> = Decoder::new();
    let mut buf = [0u8; 512];
    let mut out = [0u8; MAX_FRAME];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                for &byte in &buf[..n] {
                    if let Some(Ok(msg)) = decoder.push(byte) {
                        let _ = dev.send(&msg);
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::TimedOut || e.kind() == ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }
        for msg in dev.poll() {
            let len = encode(&msg, &mut out).expect("encode device msg");
            stream.write_all(&out[..len])?;
        }
    }
}

/// Broadcast a discovery beacon every 2 s, like the firmware does.
fn spawn_beacon(ip: String, port: u16) {
    std::thread::Builder::new()
        .name("beacon".into())
        .spawn(move || {
            let Ok(sock) = UdpSocket::bind(("0.0.0.0", 0)) else { return };
            let _ = sock.set_broadcast(true);
            let payload = format!("WIRELAB1 {ip} {port} Simulated C5");
            loop {
                let _ = sock.send_to(payload.as_bytes(), ("255.255.255.255", DISCOVERY_PORT));
                let _ = sock.send_to(payload.as_bytes(), ("127.0.0.1", DISCOVERY_PORT));
                std::thread::sleep(Duration::from_secs(2));
            }
        })
        .expect("spawn beacon thread");
}
