//! Real hardware over a TCP socket (Wi-Fi link to the board).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, unbounded};
use wirelab_proto::frame::{Decoder, encode};
use wirelab_proto::{DeviceMsg, HostMsg, MAX_FRAME};

use crate::{Device, LinkError};

pub const DEFAULT_TCP_PORT: u16 = 4518;

pub struct TcpDevice {
    addr: String,
    writer: TcpStream,
    rx: Receiver<DeviceMsg>,
    alive: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl TcpDevice {
    /// Connect to `host:port`, resolving hostnames if needed.
    pub fn connect(addr: &str) -> Result<Self, LinkError> {
        let sock_addr = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| LinkError::Io(std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                format!("no address for {addr}"),
            )))?;
        Self::connect_addr(sock_addr)
    }

    pub fn connect_addr(sock_addr: SocketAddr) -> Result<Self, LinkError> {
        let stream = TcpStream::connect_timeout(&sock_addr, Duration::from_secs(3))?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_millis(20)))?;
        let writer = stream.try_clone()?;
        let (tx, rx) = unbounded();
        let alive = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_alive = alive.clone();
        let thread_stop = stop.clone();
        let mut reader = stream;
        std::thread::Builder::new()
            .name(format!("tcp-read {sock_addr}"))
            .spawn(move || {
                let mut decoder: Decoder<DeviceMsg> = Decoder::new();
                let mut buf = [0u8; 512];
                loop {
                    if thread_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    match reader.read(&mut buf) {
                        Ok(0) => {
                            thread_alive.store(false, Ordering::Relaxed);
                            return;
                        }
                        Ok(n) => {
                            for &byte in &buf[..n] {
                                if let Some(Ok(msg)) = decoder.push(byte)
                                    && tx.send(msg).is_err() {
                                        return;
                                    }
                            }
                        }
                        Err(e)
                            if e.kind() == std::io::ErrorKind::TimedOut
                                || e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => {
                            thread_alive.store(false, Ordering::Relaxed);
                            return;
                        }
                    }
                }
            })
            .expect("spawn tcp reader");
        Ok(TcpDevice { addr: sock_addr.to_string(), writer, rx, alive, stop })
    }
}

impl Device for TcpDevice {
    fn send(&mut self, msg: &HostMsg) -> Result<(), LinkError> {
        let mut buf = [0u8; MAX_FRAME];
        let n = encode(msg, &mut buf).map_err(LinkError::Encode)?;
        self.writer.write_all(&buf[..n]).map_err(|e| {
            self.alive.store(false, Ordering::Relaxed);
            LinkError::Io(e)
        })?;
        Ok(())
    }

    fn poll(&mut self) -> Vec<DeviceMsg> {
        self.rx.try_iter().collect()
    }

    fn description(&self) -> String {
        format!("tcp {}", self.addr)
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl Drop for TcpDevice {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.writer.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use wirelab_proto::{ChipKind, PROTO_VERSION};

    /// Accept one connection and answer Hello/Ping like real firmware.
    fn fake_device(listener: TcpListener) {
        let (mut stream, _) = listener.accept().expect("accept");
        stream.set_nodelay(true).expect("nodelay");
        let mut decoder: Decoder<HostMsg> = Decoder::new();
        let mut buf = [0u8; 512];
        let mut out = [0u8; MAX_FRAME];
        loop {
            let n = match stream.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            for &byte in &buf[..n] {
                let Some(Ok(msg)) = decoder.push(byte) else { continue };
                let reply = match msg {
                    HostMsg::Hello { .. } => DeviceMsg::HelloAck {
                        proto: PROTO_VERSION,
                        fw_version: 1,
                        chip: ChipKind::Simulated,
                        gpio_mask: 0xff,
                        input_only_mask: 0,
                    },
                    HostMsg::Ping { seq } => DeviceMsg::Pong { seq },
                    _ => continue,
                };
                let len = encode(&reply, &mut out).expect("encode reply");
                stream.write_all(&out[..len]).expect("write reply");
            }
        }
    }

    fn poll_until(dev: &mut TcpDevice, timeout: Duration) -> Vec<DeviceMsg> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let msgs = dev.poll();
            if !msgs.is_empty() {
                return msgs;
            }
            assert!(std::time::Instant::now() < deadline, "no reply within {timeout:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn hello_and_ping_round_trip() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let server = std::thread::spawn(move || fake_device(listener));

        let mut dev = TcpDevice::connect(&addr.to_string()).expect("connect");
        assert_eq!(dev.description(), format!("tcp {addr}"));
        assert!(dev.is_alive());

        dev.send(&HostMsg::Hello { proto: PROTO_VERSION }).expect("send hello");
        let msgs = poll_until(&mut dev, Duration::from_secs(2));
        assert!(
            msgs.iter().any(|m| matches!(
                m,
                DeviceMsg::HelloAck { chip: ChipKind::Simulated, gpio_mask: 0xff, .. }
            )),
            "expected HelloAck, got {msgs:?}"
        );

        dev.send(&HostMsg::Ping { seq: 42 }).expect("send ping");
        let msgs = poll_until(&mut dev, Duration::from_secs(2));
        assert!(
            msgs.iter().any(|m| matches!(m, DeviceMsg::Pong { seq: 42 })),
            "expected Pong, got {msgs:?}"
        );

        drop(dev);
        server.join().expect("server thread");
    }
}
