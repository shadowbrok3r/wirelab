//! In-process simulated device: same protocol semantics as the firmware,
//! backed by the wirelab-core electrical solver.

use std::collections::HashMap;
use std::time::Instant;

use wirelab_core::board::BoardProfile;
use wirelab_core::circuit::Circuit;
use wirelab_core::library::Library;
use wirelab_core::netlist::Netlist;
use wirelab_core::sim::{PinBank, SimOutput, solve};
use wirelab_proto::{
    AnalogSample, Behavior, ChipKind, DeviceMsg, ErrorCode, EventEdge, FW_VERSION, HostMsg,
    MAX_ANALOG_SAMPLES, PROTO_VERSION, PinMode,
};

use crate::{Device, LinkError};

/// Temperature in centi-degrees C, drifting 20.0–28.0 over a 60 s period.
fn sim_temp_c_x100(millis: u64) -> i16 {
    let phase = (millis % 60_000) as f64 / 60_000.0;
    2400 + (400.0 * (std::f64::consts::TAU * phase).sin()) as i16
}

/// Relative humidity in percent, drifting 40–60 over a 45 s period.
fn sim_humidity_pct(millis: u64) -> f64 {
    let phase = (millis % 45_000) as f64 / 45_000.0;
    50.0 + 10.0 * (std::f64::consts::TAU * phase).sin()
}

/// Register payload of a virtual I2C sensor; None for unknown addresses.
fn i2c_sensor_bytes(addr: u8, reg: u16, millis: u64) -> Option<Vec<u8>> {
    match addr {
        // BME280: chip id + big-endian centi-degree temperature.
        0x76 => Some(match reg {
            0xD0 => vec![0x60],
            0xFA | 0xE3 => {
                let v = sim_temp_c_x100(millis);
                vec![(v >> 8) as u8, v as u8]
            }
            _ => Vec::new(),
        }),
        // SHT31: [t_msb, t_lsb, crc, h_msb, h_lsb, crc] measurement frame.
        0x44 => {
            let t = ((f64::from(sim_temp_c_x100(millis)) / 100.0 + 45.0) / 175.0 * 65535.0) as u16;
            let h = (sim_humidity_pct(millis) / 100.0 * 65535.0) as u16;
            Some(vec![(t >> 8) as u8, t as u8, 0, (h >> 8) as u8, h as u8, 0])
        }
        _ => None,
    }
}

struct BehaviorState {
    behavior: Behavior,
    next_toggle_ms: u64,
    phase_start_ms: u64,
}

pub struct SimDevice {
    board: BoardProfile,
    lib: Library,
    circuit: Circuit,
    netlist: Netlist,
    bank: PinBank,
    behaviors: [Option<BehaviorState>; wirelab_proto::BEHAVIOR_SLOTS],
    telemetry_ms: u16,
    watched_analog: HashMap<u8, u16>,
    next_telemetry_ms: u64,
    prev_inputs: HashMap<u8, bool>,
    outbox: Vec<DeviceMsg>,
    epoch: Instant,
    last_tick_ms: u64,
    /// Latest solve, exposed for the GUI to draw component visuals.
    pub last_output: SimOutput,
    /// Last commanded WS2812 color.
    rgb: Option<[u8; 3]>,
    /// Simulated display op log (None until LcdInit).
    lcd: Option<Vec<wirelab_core::sim::LcdOp>>,
    /// Simulated button holds: GPIO -> forced-low-until (ms).
    forced_low: HashMap<u8, u64>,
    /// Simulated Wi-Fi station state.
    wifi: (wirelab_proto::WifiState, [u8; 4]),
}

impl SimDevice {
    pub fn new(board: BoardProfile, lib: Library, circuit: Circuit) -> Self {
        let netlist = Netlist::build(&circuit, &board, &lib);
        SimDevice {
            board,
            lib,
            circuit,
            netlist,
            bank: PinBank::default(),
            behaviors: Default::default(),
            telemetry_ms: 0,
            watched_analog: HashMap::new(),
            next_telemetry_ms: 0,
            prev_inputs: HashMap::new(),
            outbox: Vec::new(),
            epoch: Instant::now(),
            last_tick_ms: 0,
            last_output: SimOutput::default(),
            rgb: None,
            lcd: None,
            forced_low: HashMap::new(),
            wifi: (wirelab_proto::WifiState::Off, [0; 4]),
        }
    }

    /// Adopt edited wiring / component state from the GUI.
    pub fn sync_circuit(&mut self, circuit: &Circuit) {
        self.circuit = circuit.clone();
        self.netlist = Netlist::build(&self.circuit, &self.board, &self.lib);
    }

    pub fn bank(&self) -> &PinBank {
        &self.bank
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn gpio_valid(&self, pin: u8) -> bool {
        self.board.gpio_mask() & (1u64 << pin.min(63)) != 0
    }

    /// One simulation step; public so tests can drive time manually.
    /// Hold a pin low for `ms`, like pressing an on-board button.
    pub fn press_pin(&mut self, gpio: u8, ms: u64) {
        let now = self.now_ms();
        self.forced_low.insert(gpio, now + ms);
    }

    pub fn tick(&mut self, now_ms: u64) {
        self.last_tick_ms = now_ms;
        self.run_behaviors(now_ms);
        let mut out = solve(&self.circuit, &self.board, &self.lib, &self.netlist, &self.bank);
        out.rgb = self.rgb;
        out.lcd = self.lcd.clone();
        self.forced_low.retain(|_, until| now_ms < *until);
        for &gpio in self.forced_low.keys() {
            if self.bank.get(gpio).mode.is_input() {
                out.digital.insert(gpio, false);
            }
        }

        // Edge events for input-mode pins.
        for (&gpio, &level) in &out.digital {
            if !self.bank.get(gpio).mode.is_input() {
                continue;
            }
            match self.prev_inputs.insert(gpio, level) {
                Some(prev) if prev != level => {
                    self.outbox.push(DeviceMsg::Event {
                        millis: now_ms as u32,
                        pin: gpio,
                        edge: if level { EventEdge::Rising } else { EventEdge::Falling },
                    });
                }
                _ => {}
            }
        }

        if self.telemetry_ms > 0 && now_ms >= self.next_telemetry_ms {
            self.next_telemetry_ms = now_ms + u64::from(self.telemetry_ms);
            let mut levels = 0u64;
            for pin in &self.board.pins {
                let Some(gpio) = pin.kind.gpio() else { continue };
                let drive = self.bank.get(gpio);
                let high = if drive.mode.is_input() {
                    out.digital.get(&gpio).copied().unwrap_or(false)
                } else if drive.mode == PinMode::Pwm {
                    drive.duty > 0.5
                } else if drive.mode.is_output() {
                    drive.out_high
                } else {
                    false
                };
                if high {
                    levels |= 1 << gpio.min(63);
                }
            }
            let mut analog = wirelab_proto::heapless::Vec::<AnalogSample, MAX_ANALOG_SAMPLES>::new();
            for (&gpio, _) in self.watched_analog.iter() {
                if let Some(&mv) = out.analog_mv.get(&gpio) {
                    let _ = analog.push(AnalogSample { pin: gpio, millivolts: mv });
                }
            }
            self.outbox.push(DeviceMsg::Telemetry { millis: now_ms as u32, levels, analog });
        }

        self.last_output = out;
    }

    fn run_behaviors(&mut self, now_ms: u64) {
        let inputs = self.last_output.digital.clone();
        for slot in self.behaviors.iter_mut() {
            let Some(state) = slot else { continue };
            match state.behavior {
                Behavior::Blink { pin, period_ms } => {
                    if now_ms >= state.next_toggle_ms {
                        state.next_toggle_ms = now_ms + u64::from(period_ms.max(20)) / 2;
                        if let Some(p) = self.bank.get_mut(pin) {
                            p.out_high = !p.out_high;
                        }
                    }
                }
                Behavior::Breathe { pin, period_ms } => {
                    let period = u64::from(period_ms.max(100));
                    let t = ((now_ms - state.phase_start_ms) % period) as f32 / period as f32;
                    let duty = if t < 0.5 { t * 2.0 } else { 2.0 - t * 2.0 };
                    if let Some(p) = self.bank.get_mut(pin) {
                        p.mode = PinMode::Pwm;
                        p.freq_hz = 1000.0;
                        p.duty = duty;
                    }
                }
                Behavior::Mirror { from, to, invert } => {
                    let level = inputs.get(&from).copied().unwrap_or(false) ^ invert;
                    if let Some(p) = self.bank.get_mut(to) {
                        p.out_high = level;
                    }
                }
                Behavior::Watch { .. } => {}
            }
        }
    }

    fn handle(&mut self, msg: &HostMsg) {
        let now = self.now_ms();
        match msg {
            HostMsg::Hello { .. } => {
                self.outbox.push(DeviceMsg::HelloAck {
                    proto: PROTO_VERSION,
                    fw_version: FW_VERSION,
                    chip: ChipKind::Simulated,
                    gpio_mask: self.board.gpio_mask(),
                    input_only_mask: self.board.input_only_mask(),
                });
            }
            HostMsg::Reset => {
                self.bank = PinBank::default();
                self.behaviors = Default::default();
                self.watched_analog.clear();
                self.telemetry_ms = 0;
                self.prev_inputs.clear();
                self.rgb = None;
                self.lcd = None;
                self.forced_low.clear();
            }
            HostMsg::Ping { seq } => self.outbox.push(DeviceMsg::Pong { seq: *seq }),
            HostMsg::SetPinMode { pin, mode } => {
                if !self.gpio_valid(*pin) {
                    self.outbox.push(DeviceMsg::Error { code: ErrorCode::BadPin, pin: *pin });
                    return;
                }
                let input_only = self.board.input_only_mask() & (1u64 << pin.min(&63)) != 0;
                if input_only && mode.is_output() {
                    self.outbox.push(DeviceMsg::Error { code: ErrorCode::BadMode, pin: *pin });
                    return;
                }
                self.bank.apply(msg);
                self.prev_inputs.remove(pin);
            }
            HostMsg::WriteDigital { pin, .. } | HostMsg::SetPwm { pin, .. } => {
                if !self.gpio_valid(*pin) {
                    self.outbox.push(DeviceMsg::Error { code: ErrorCode::BadPin, pin: *pin });
                    return;
                }
                self.bank.apply(msg);
            }
            HostMsg::ReadAnalog { pin } => {
                let out = solve(&self.circuit, &self.board, &self.lib, &self.netlist, &self.bank);
                let mv = out.analog_mv.get(pin).copied().unwrap_or(0);
                self.outbox.push(DeviceMsg::AnalogValue { pin: *pin, millivolts: mv });
            }
            HostMsg::WatchAnalog { pin, interval_ms } => {
                if *interval_ms == 0 {
                    self.watched_analog.remove(pin);
                } else {
                    self.watched_analog.insert(*pin, *interval_ms);
                }
            }
            HostMsg::SetTelemetry { interval_ms } => {
                self.telemetry_ms = *interval_ms;
                self.next_telemetry_ms = 0;
            }
            HostMsg::AttachBehavior { slot, behavior } => {
                let idx = *slot as usize;
                if idx >= self.behaviors.len() {
                    self.outbox.push(DeviceMsg::Error { code: ErrorCode::NoFreeSlot, pin: 0 });
                    return;
                }
                self.behaviors[idx] = Some(BehaviorState {
                    behavior: *behavior,
                    next_toggle_ms: now,
                    phase_start_ms: now,
                });
            }
            HostMsg::SetRgb { r, g, b, .. } => {
                self.rgb = Some([*r, *g, *b]);
            }
            HostMsg::LcdInit { .. } => {
                self.lcd = Some(vec![wirelab_core::sim::LcdOp::Clear([0, 0, 0])]);
            }
            HostMsg::LcdClear { rgb565 } => {
                if let Some(ops) = &mut self.lcd {
                    ops.clear();
                    ops.push(wirelab_core::sim::LcdOp::Clear(wirelab_core::sim::rgb888(*rgb565)));
                }
            }
            HostMsg::LcdRect { x, y, w, h, rgb565 } => {
                if let Some(ops) = &mut self.lcd {
                    ops.push(wirelab_core::sim::LcdOp::Rect {
                        x: *x,
                        y: *y,
                        w: *w,
                        h: *h,
                        rgb: wirelab_core::sim::rgb888(*rgb565),
                    });
                    if ops.len() > 512 {
                        ops.drain(..256);
                    }
                }
            }
            HostMsg::LcdText { x, y, rgb565, text } => {
                if let Some(ops) = &mut self.lcd {
                    ops.push(wirelab_core::sim::LcdOp::Text {
                        x: *x,
                        y: *y,
                        rgb: wirelab_core::sim::rgb888(*rgb565),
                        text: text.to_string(),
                    });
                    if ops.len() > 512 {
                        ops.drain(..256);
                    }
                }
            }
            // Generic buses: SPI echoes the written bytes; I2C reads answer
            // from virtual sensors (0x76/0x44) and zero-fill other addresses.
            HostMsg::SpiConfig { .. } | HostMsg::I2cConfig { .. } => {}
            HostMsg::SpiTransfer { data, .. } => {
                self.outbox.push(DeviceMsg::SpiData { data: data.clone() });
            }
            HostMsg::I2cWrite { .. } => {}
            HostMsg::I2cRead { addr, reg, len } => {
                let bytes = i2c_sensor_bytes(*addr, *reg, self.last_tick_ms).unwrap_or_default();
                let mut data = wirelab_proto::heapless::Vec::new();
                for i in 0..usize::from(*len).min(48) {
                    let _ = data.push(bytes.get(i).copied().unwrap_or(0));
                }
                self.outbox.push(DeviceMsg::I2cData { addr: *addr, data });
            }
            HostMsg::UartConfig { .. } => {}
            // The simulator loops UART writes straight back, so scripts can
            // be tested without a jumper wire.
            HostMsg::UartWrite { data } => {
                self.outbox.push(DeviceMsg::UartData { data: data.clone() });
            }
            // Simulated Wi-Fi joins any non-empty SSID instantly.
            HostMsg::WifiConfig { ssid, .. } => {
                self.wifi = if ssid.is_empty() {
                    (wirelab_proto::WifiState::Off, [0; 4])
                } else {
                    (wirelab_proto::WifiState::Connected, [192, 168, 0, 42])
                };
                self.outbox.push(DeviceMsg::WifiStatus { state: self.wifi.0, ip: self.wifi.1 });
            }
            HostMsg::WifiStatusReq => {
                self.outbox.push(DeviceMsg::WifiStatus { state: self.wifi.0, ip: self.wifi.1 });
            }
            HostMsg::DetachBehavior { slot } => {
                let idx = *slot as usize;
                if idx < self.behaviors.len()
                    && let Some(state) = self.behaviors[idx].take() {
                        // Leave the pin in a quiet state.
                        match state.behavior {
                            Behavior::Blink { pin, .. } | Behavior::Breathe { pin, .. } => {
                                if let Some(p) = self.bank.get_mut(pin) {
                                    p.out_high = false;
                                    if p.mode == PinMode::Pwm {
                                        p.duty = 0.0;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
            }
        }
    }
}

impl Device for SimDevice {
    fn send(&mut self, msg: &HostMsg) -> Result<(), LinkError> {
        self.handle(msg);
        Ok(())
    }

    fn poll(&mut self) -> Vec<DeviceMsg> {
        let now = self.now_ms();
        if now > self.last_tick_ms {
            self.tick(now);
        }
        std::mem::take(&mut self.outbox)
    }

    fn description(&self) -> String {
        format!("simulated {}", self.board.name)
    }

    fn is_alive(&self) -> bool {
        true
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}
