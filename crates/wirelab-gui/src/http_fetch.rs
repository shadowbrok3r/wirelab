//! Background HTTP fetches for scripts: http_get runs here, on the host.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

/// Response bodies are truncated to this many bytes.
pub const MAX_BODY: usize = 64 * 1024;
const MAX_IN_FLIGHT: usize = 4;
const TIMEOUT: Duration = Duration::from_secs(15);

/// Runs GET requests on background threads; poll with `drain_done`.
pub struct HttpPool {
    tx: Sender<(u16, String)>,
    rx: Receiver<(u16, String)>,
    in_flight: Arc<AtomicUsize>,
}

impl Default for HttpPool {
    fn default() -> Self {
        let (tx, rx) = channel();
        HttpPool { tx, rx, in_flight: Arc::new(AtomicUsize::new(0)) }
    }
}

impl HttpPool {
    /// Start a request; false when the in-flight cap rejects it.
    pub fn spawn(&self, url: String) -> bool {
        if self.in_flight.load(Ordering::Acquire) >= MAX_IN_FLIGHT {
            return false;
        }
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        let tx = self.tx.clone();
        let in_flight = Arc::clone(&self.in_flight);
        std::thread::spawn(move || {
            let result = fetch(&url);
            in_flight.fetch_sub(1, Ordering::AcqRel);
            let _ = tx.send(result);
        });
        true
    }

    /// Completed responses as (status, body); errors are (0, error text).
    pub fn drain_done(&self) -> Vec<(u16, String)> {
        self.rx.try_iter().collect()
    }
}

fn fetch(url: &str) -> (u16, String) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .build()
        .into();
    match agent.get(url).call() {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            let mut bytes = Vec::new();
            let _ = resp
                .body_mut()
                .as_reader()
                .take(MAX_BODY as u64)
                .read_to_end(&mut bytes);
            (status, String::from_utf8_lossy(&bytes).into_owned())
        }
        Err(e) => (0, e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::time::Instant;

    fn wait_one(pool: &HttpPool) -> (u16, String) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(r) = pool.drain_done().into_iter().next() {
                return r;
            }
            assert!(Instant::now() < deadline, "no response within 10 s");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn fetches_a_local_http_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let body = "hello wirelab";
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        });

        let pool = HttpPool::default();
        assert!(pool.spawn(format!("http://{addr}/")));
        let (status, body) = wait_one(&pool);
        assert_eq!(status, 200);
        assert_eq!(body, "hello wirelab");
    }

    #[test]
    fn connection_error_becomes_status_zero() {
        // Bind then drop: the OS refuses connections to the freed port.
        let addr = {
            let l = TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().expect("addr")
        };
        let pool = HttpPool::default();
        assert!(pool.spawn(format!("http://{addr}/")));
        let (status, body) = wait_one(&pool);
        assert_eq!(status, 0);
        assert!(!body.is_empty(), "error text expected");
    }

    #[test]
    fn in_flight_cap_rejects_the_fifth_request() {
        // Accepted by the listen backlog but never answered.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let pool = HttpPool::default();
        for _ in 0..MAX_IN_FLIGHT {
            assert!(pool.spawn(format!("http://{addr}/")));
        }
        assert!(!pool.spawn(format!("http://{addr}/")), "cap exceeded");
        drop(listener);
    }
}
