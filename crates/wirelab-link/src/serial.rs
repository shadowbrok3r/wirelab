//! Real hardware over a serial port (USB-Serial-JTAG or UART bridge).

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, unbounded};
use wirelab_proto::frame::{Decoder, encode};
use wirelab_proto::{DeviceMsg, HostMsg, MAX_FRAME};

use crate::{ControlRequest, Device, LinkError};

pub const DEFAULT_BAUD: u32 = 115_200;

/// List candidate serial ports, USB devices first.
pub fn available_ports() -> Vec<String> {
    let mut ports: Vec<(bool, String)> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            let usb = matches!(p.port_type, serialport::SerialPortType::UsbPort(_));
            (usb, p.port_name)
        })
        .collect();
    ports.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    ports.into_iter().map(|(_, name)| name).collect()
}

pub struct SerialDevice {
    port_name: String,
    writer: Box<dyn serialport::SerialPort>,
    rx: Receiver<DeviceMsg>,
    alive: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl SerialDevice {
    pub fn open(port_name: &str, baud: u32) -> Result<Self, LinkError> {
        let port = serialport::new(port_name, baud)
            .timeout(Duration::from_millis(20))
            .open()?;
        let writer = port.try_clone()?;
        let (tx, rx) = unbounded();
        let alive = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_alive = alive.clone();
        let thread_stop = stop.clone();
        let mut reader = port;
        std::thread::Builder::new()
            .name(format!("serial-read {port_name}"))
            .spawn(move || {
                let mut decoder: Decoder<DeviceMsg> = Decoder::new();
                let mut buf = [0u8; 512];
                loop {
                    if thread_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    match reader.read(&mut buf) {
                        Ok(0) => {}
                        Ok(n) => {
                            for &byte in &buf[..n] {
                                if let Some(Ok(msg)) = decoder.push(byte)
                                    && tx.send(msg).is_err() {
                                        return;
                                    }
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                        Err(_) => {
                            thread_alive.store(false, Ordering::Relaxed);
                            return;
                        }
                    }
                }
            })
            .expect("spawn serial reader");
        Ok(SerialDevice { port_name: port_name.to_string(), writer, rx, alive, stop })
    }
}

impl Device for SerialDevice {
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
        format!("serial {}", self.port_name)
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Drive the auto-reset circuit (RTS→EN, DTR→BOOT) like esptool does.
    fn control(&mut self, req: ControlRequest) -> bool {
        let w = &mut self.writer;
        let run = |w: &mut Box<dyn serialport::SerialPort>| -> serialport::Result<()> {
            match req {
                ControlRequest::PulseReset => {
                    w.write_data_terminal_ready(false)?;
                    w.write_request_to_send(true)?;
                    std::thread::sleep(Duration::from_millis(100));
                    w.write_request_to_send(false)?;
                }
                ControlRequest::EnterBootloader => {
                    w.write_data_terminal_ready(false)?;
                    w.write_request_to_send(true)?;
                    std::thread::sleep(Duration::from_millis(100));
                    w.write_data_terminal_ready(true)?;
                    w.write_request_to_send(false)?;
                    std::thread::sleep(Duration::from_millis(100));
                    w.write_data_terminal_ready(false)?;
                }
            }
            Ok(())
        };
        run(w).is_ok()
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl Drop for SerialDevice {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
