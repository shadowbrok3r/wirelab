//! A `SimDevice` served over TCP, mirroring examples/board_server.rs:
//! a full `Session` handshakes through `TcpDevice` and receives telemetry.

use std::io::{ErrorKind, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::time::{Duration, Instant};

use wirelab_core::circuit::Circuit;
use wirelab_core::library::Library;
use wirelab_link::sim::SimDevice;
use wirelab_link::tcp::TcpDevice;
use wirelab_link::{Device, Session, SessionPhase};
use wirelab_proto::frame::{Decoder, encode};
use wirelab_proto::{DeviceMsg, HostMsg, MAX_FRAME};

const BOARD_ID: &str = "esp32-c5-devkitc-1";

/// Serve one client, same loop as examples/board_server.rs.
fn serve_one_client(listener: TcpListener, dev: &mut SimDevice) {
    let (mut stream, _) = listener.accept().expect("accept");
    stream.set_nodelay(true).expect("nodelay");
    stream.set_read_timeout(Some(Duration::from_millis(10))).expect("read timeout");
    let mut decoder: Decoder<HostMsg> = Decoder::new();
    let mut buf = [0u8; 512];
    let mut out = [0u8; MAX_FRAME];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                for &byte in &buf[..n] {
                    if let Some(Ok(msg)) = decoder.push(byte) {
                        let _ = dev.send(&msg);
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::TimedOut || e.kind() == ErrorKind::WouldBlock => {}
            Err(_) => return,
        }
        for msg in dev.poll() {
            let len = encode(&msg, &mut out).expect("encode device msg");
            if stream.write_all(&out[..len]).is_err() {
                return;
            }
        }
    }
}

#[test]
fn session_handshakes_and_streams_telemetry_over_tcp() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
    let lib = Library::load(&assets.join("boards"), &assets.join("components"))
        .expect("load asset library");
    let board = lib.board(BOARD_ID).expect("board profile").clone();
    let mut dev = SimDevice::new(board, lib, Circuit::new(BOARD_ID));

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    let server = std::thread::spawn(move || serve_one_client(listener, &mut dev));

    let tcp = TcpDevice::connect(&addr.to_string()).expect("connect");
    let mut session = Session::new(Box::new(tcp)).expect("session");
    session.send(&HostMsg::SetTelemetry { interval_ms: 20 }).expect("set telemetry");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got_telemetry = false;
    while Instant::now() < deadline && !(session.phase == SessionPhase::Ready && got_telemetry) {
        got_telemetry |= session
            .update()
            .iter()
            .any(|m| matches!(m, DeviceMsg::Telemetry { .. }));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(session.phase, SessionPhase::Ready, "handshake never completed");
    assert!(got_telemetry, "no telemetry within 5s");

    drop(session);
    server.join().expect("server thread");
}
