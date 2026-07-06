//! Host-side device transports: serial hardware link, TCP link over Wi-Fi,
//! UDP board discovery and in-process simulator.

pub mod discovery;
pub mod serial;
pub mod sim;
pub mod tcp;

use wirelab_core::sim::PinBank;
use wirelab_proto::{AnalogSample, ChipKind, DeviceMsg, HostMsg, PinMode, WifiState};

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("serial: {0}")]
    Serial(#[from] serialport::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("encode: {0:?}")]
    Encode(wirelab_proto::frame::FrameError),
    #[error("device disconnected")]
    Disconnected,
}

/// Out-of-band board control, e.g. DTR/RTS reset pulses on a UART bridge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlRequest {
    /// Pulse EN low: reboot into the application firmware.
    PulseReset,
    /// esptool-style reset with BOOT held low: enter ROM download mode.
    EnterBootloader,
}

/// A live device the host can command: real hardware or the simulator.
pub trait Device: Send {
    fn send(&mut self, msg: &HostMsg) -> Result<(), LinkError>;
    /// Non-blocking drain of everything the device produced since last poll.
    fn poll(&mut self) -> Vec<DeviceMsg>;
    fn description(&self) -> String;
    fn is_alive(&self) -> bool;
    /// Out-of-band control; returns false when the transport can't do it.
    fn control(&mut self, _req: ControlRequest) -> bool {
        false
    }
    /// Downcast hook, e.g. to reach simulator-only state.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    AwaitingHello,
    Ready,
    Dead,
}

/// Device info reported by `HelloAck`.
#[derive(Debug, Clone, Copy)]
pub struct DeviceInfo {
    pub chip: ChipKind,
    pub fw_version: u16,
    pub gpio_mask: u64,
    pub input_only_mask: u64,
}

/// Handshake, command mirror and telemetry cache around a `Device`.
pub struct Session {
    pub device: Box<dyn Device>,
    pub phase: SessionPhase,
    pub info: Option<DeviceInfo>,
    /// Modes and commanded outputs, as sent to the device.
    pub mirror: PinBank,
    /// Latest digital snapshot: bit N = level of GPIO N.
    pub levels: u64,
    /// Latest analog readings per GPIO, millivolts.
    pub analog: std::collections::HashMap<u8, u16>,
    pub last_telemetry_ms: Option<u32>,
    pub log: Vec<String>,
    /// Latest Wi-Fi station state reported by the device.
    pub wifi: Option<(WifiState, [u8; 4])>,
}

impl Session {
    pub fn new(mut device: Box<dyn Device>) -> Result<Self, LinkError> {
        device.send(&HostMsg::Hello { proto: wirelab_proto::PROTO_VERSION })?;
        Ok(Session {
            device,
            phase: SessionPhase::AwaitingHello,
            info: None,
            mirror: PinBank::default(),
            levels: 0,
            analog: std::collections::HashMap::new(),
            last_telemetry_ms: None,
            log: Vec::new(),
            wifi: None,
        })
    }

    pub fn send(&mut self, msg: &HostMsg) -> Result<(), LinkError> {
        self.mirror.apply(msg);
        self.device.send(msg)
    }

    pub fn send_all(&mut self, msgs: &[HostMsg]) -> Result<(), LinkError> {
        for msg in msgs {
            self.send(msg)?;
        }
        Ok(())
    }

    pub fn control(&mut self, req: ControlRequest) -> bool {
        self.device.control(req)
    }

    /// Poll the device, absorb session-level messages, return the rest.
    pub fn update(&mut self) -> Vec<DeviceMsg> {
        if !self.device.is_alive() {
            self.phase = SessionPhase::Dead;
        }
        let msgs = self.device.poll();
        let mut engine_msgs = Vec::with_capacity(msgs.len());
        for msg in msgs {
            match &msg {
                DeviceMsg::HelloAck { proto, fw_version, chip, gpio_mask, input_only_mask } => {
                    if *proto != wirelab_proto::PROTO_VERSION {
                        self.log.push(format!(
                            "protocol mismatch: host {} vs device {proto}",
                            wirelab_proto::PROTO_VERSION
                        ));
                    }
                    self.info = Some(DeviceInfo {
                        chip: *chip,
                        fw_version: *fw_version,
                        gpio_mask: *gpio_mask,
                        input_only_mask: *input_only_mask,
                    });
                    self.phase = SessionPhase::Ready;
                }
                DeviceMsg::Telemetry { millis, levels, analog } => {
                    self.levels = *levels;
                    self.last_telemetry_ms = Some(*millis);
                    for s in analog.iter() {
                        self.analog.insert(s.pin, s.millivolts);
                    }
                    engine_msgs.push(msg);
                }
                DeviceMsg::AnalogValue { pin, millivolts } => {
                    self.analog.insert(*pin, *millivolts);
                    engine_msgs.push(msg);
                }
                DeviceMsg::Event { .. }
                | DeviceMsg::Pong { .. }
                | DeviceMsg::UartData { .. }
                | DeviceMsg::SpiData { .. }
                | DeviceMsg::I2cData { .. } => engine_msgs.push(msg),
                DeviceMsg::Error { code, pin } => {
                    self.log.push(format!("device error {code:?} on pin {pin}"));
                }
                DeviceMsg::Log { msg: text } => self.log.push(format!("device: {text}")),
                DeviceMsg::WifiStatus { state, ip } => {
                    if self.wifi != Some((*state, *ip)) {
                        self.log.push(format!(
                            "wifi: {state:?} {}.{}.{}.{}",
                            ip[0], ip[1], ip[2], ip[3]
                        ));
                    }
                    self.wifi = Some((*state, *ip));
                }
            }
        }
        engine_msgs
    }

    /// Mirror with telemetry levels folded in, for on-canvas visualization.
    pub fn effective_bank(&self) -> PinBank {
        let mut bank = self.mirror.clone();
        for gpio in 0..64u8 {
            let drive = bank.get(gpio);
            if drive.mode == PinMode::Output
                && let Some(p) = bank.get_mut(gpio) {
                    p.out_high = self.levels & (1 << gpio) != 0;
                }
        }
        bank
    }

    /// Latest analog samples as a plain list.
    pub fn analog_samples(&self) -> Vec<AnalogSample> {
        let mut v: Vec<AnalogSample> = self
            .analog
            .iter()
            .map(|(&pin, &millivolts)| AnalogSample { pin, millivolts })
            .collect();
        v.sort_by_key(|s| s.pin);
        v
    }
}
