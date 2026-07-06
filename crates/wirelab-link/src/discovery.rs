//! Passive UDP beacon listener for boards announcing themselves on the LAN.
//!
//! Beacon payload, ASCII: `WIRELAB1 <ip> <port> <chip-name>`.

use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const DISCOVERY_PORT: u16 = 4519;

/// Beacons older than this are dropped from `boards()`.
const BEACON_TTL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct Beacon {
    /// `ip:port` of the board's TCP listener.
    pub addr: String,
    pub chip: String,
    pub last_seen: Instant,
}

pub struct Discovery {
    boards: Arc<Mutex<HashMap<String, Beacon>>>,
    error: Option<String>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Discovery {
    pub fn listen() -> Self {
        Self::listen_on(DISCOVERY_PORT)
    }

    pub fn listen_on(port: u16) -> Self {
        let boards: Arc<Mutex<HashMap<String, Beacon>>> = Arc::new(Mutex::new(HashMap::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let socket = match UdpSocket::bind(("0.0.0.0", port)) {
            Ok(s) => s,
            Err(e) => {
                return Discovery {
                    boards,
                    error: Some(format!("bind udp {port}: {e}")),
                    stop,
                    handle: None,
                };
            }
        };
        if let Err(e) = socket.set_read_timeout(Some(Duration::from_millis(500))) {
            return Discovery {
                boards,
                error: Some(format!("udp read timeout: {e}")),
                stop,
                handle: None,
            };
        }
        let thread_boards = boards.clone();
        let thread_stop = stop.clone();
        let handle = std::thread::Builder::new()
            .name(format!("discovery :{port}"))
            .spawn(move || {
                let mut buf = [0u8; 256];
                loop {
                    if thread_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    match socket.recv_from(&mut buf) {
                        Ok((n, _)) => {
                            if let Some(beacon) = parse_beacon(&buf[..n]) {
                                let mut map = thread_boards.lock().expect("boards lock");
                                map.insert(beacon.addr.clone(), beacon);
                            }
                        }
                        Err(e)
                            if e.kind() == std::io::ErrorKind::TimedOut
                                || e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => break,
                    }
                }
            })
            .expect("spawn discovery listener");
        Discovery { boards, error: None, stop, handle: Some(handle) }
    }

    /// Fresh beacons sorted by address.
    pub fn boards(&self) -> Vec<Beacon> {
        let mut map = self.boards.lock().expect("boards lock");
        map.retain(|_, b| b.last_seen.elapsed() < BEACON_TTL);
        let mut v: Vec<Beacon> = map.values().cloned().collect();
        v.sort_by(|a, b| a.addr.cmp(&b.addr));
        v
    }

    pub fn error(&self) -> Option<String> {
        self.error.clone()
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Parse `WIRELAB1 <ip> <port> <chip-name>`; chip may contain spaces.
fn parse_beacon(payload: &[u8]) -> Option<Beacon> {
    let text = std::str::from_utf8(payload).ok()?.trim();
    let mut parts = text.splitn(4, ' ');
    if parts.next()? != "WIRELAB1" {
        return None;
    }
    let ip = parts.next()?;
    let port: u16 = parts.next()?.parse().ok()?;
    let chip = parts.next()?.trim();
    if ip.is_empty() || chip.is_empty() {
        return None;
    }
    Some(Beacon {
        addr: format!("{ip}:{port}"),
        chip: chip.to_string(),
        last_seen: Instant::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_beacon() {
        let b = parse_beacon(b"WIRELAB1 192.168.1.57 4518 ESP32-C5").expect("beacon");
        assert_eq!(b.addr, "192.168.1.57:4518");
        assert_eq!(b.chip, "ESP32-C5");
    }

    #[test]
    fn rejects_malformed_beacons() {
        assert!(parse_beacon(b"NOTWIRELAB 1.2.3.4 4518 X").is_none());
        assert!(parse_beacon(b"WIRELAB1 1.2.3.4 notaport X").is_none());
        assert!(parse_beacon(b"WIRELAB1 1.2.3.4 4518").is_none());
        assert!(parse_beacon(b"\xff\xfe").is_none());
    }

    #[test]
    fn listener_collects_beacons() {
        // OS-assigned probe port; racy in theory, fine for localhost tests.
        let probe = UdpSocket::bind("127.0.0.1:0").expect("probe bind");
        let port = probe.local_addr().expect("probe addr").port();
        drop(probe);

        let disc = Discovery::listen_on(port);
        assert_eq!(disc.error(), None);

        let sender = UdpSocket::bind("127.0.0.1:0").expect("sender bind");
        sender
            .send_to(b"WIRELAB1 192.168.1.57 4518 ESP32-C5", ("127.0.0.1", port))
            .expect("send beacon");

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let boards = disc.boards();
            if let Some(b) = boards.first() {
                assert_eq!(b.addr, "192.168.1.57:4518");
                assert_eq!(b.chip, "ESP32-C5");
                break;
            }
            assert!(Instant::now() < deadline, "no beacon within 2s");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn bind_failure_reports_error() {
        let first = UdpSocket::bind("127.0.0.1:0").expect("bind");
        let port = first.local_addr().expect("addr").port();
        let disc = Discovery::listen_on(port);
        assert!(disc.error().is_some(), "expected bind error on taken port");
        assert!(disc.boards().is_empty());
    }
}
