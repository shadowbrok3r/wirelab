//! Live session management: connect, sync, engine loop.

use std::time::Instant;

use wirelab_core::circuit::Circuit;
use wirelab_core::engine::{Bindings, Engine, InKind, plan_setup};
use wirelab_core::library::Library;
use wirelab_core::netlist::Netlist;
use wirelab_core::program::{Action, Program};
use wirelab_core::script::{ScriptHost, World};
use wirelab_core::sim::{PinBank, SimOutput, solve};
use wirelab_proto::{DeviceMsg, EventEdge};
use wirelab_link::discovery::Discovery;
use wirelab_link::serial::{DEFAULT_BAUD, SerialDevice, available_ports};
use wirelab_link::sim::SimDevice;
use wirelab_link::tcp::TcpDevice;
use wirelab_link::{ControlRequest, Session, SessionPhase};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Simulator,
    Serial,
    /// WireLab link over the board's Wi-Fi TCP server.
    Tcp,
}

pub struct LiveState {
    pub session: Option<Session>,
    pub backend: Backend,
    pub engine: Engine,
    pub program_running: bool,
    pub epoch: Instant,
    pub live_output: Option<SimOutput>,
    pub ports: Vec<String>,
    pub selected_port: usize,
    /// host:port for the TCP backend, editable and filled by discovery.
    pub tcp_addr: String,
    /// UDP beacon listener; started when the TCP backend is selected.
    pub discovery: Option<Discovery>,
    /// Wi-Fi provisioning fields (sent to the board over the live session).
    pub wifi_ssid: String,
    pub wifi_pass: String,
    pub synced_topo: u64,
    pub synced_state: u64,
    pub setup_sent: bool,
    /// Latest digital telemetry snapshot, bit N = GPIO N.
    pub telemetry_levels: u64,
    /// Last WS2812 color sent over serial, for the board drawing.
    last_rgb: Option<[u8; 3]>,
    /// UART1 receive assembly buffer (split into lines for scripts).
    uart_buf: String,
    /// Mirror of display ops sent over serial, for the canvas preview.
    lcd_ops: Option<Vec<wirelab_core::sim::LcdOp>>,
    next_hello_ms: u64,
    /// Cross-board messages emitted by scripts: (target board name, text).
    /// Drained and routed by the app each frame.
    pub outbox: Vec<(String, String)>,
    /// Component scripts: always live while connected.
    pub scripts: ScriptHost,
    /// Background http_get requests; replies dispatch to on_http.
    http: crate::http_fetch::HttpPool,
    synced_scripts: u64,
    scripts_topo: u64,
    synced_flow: u64,
}

impl Default for LiveState {
    fn default() -> Self {
        LiveState {
            session: None,
            backend: Backend::Simulator,
            engine: Engine::default(),
            program_running: false,
            epoch: Instant::now(),
            live_output: None,
            ports: available_ports(),
            selected_port: 0,
            tcp_addr: String::new(),
            discovery: None,
            wifi_ssid: String::new(),
            wifi_pass: String::new(),
            synced_topo: 0,
            synced_state: 0,
            setup_sent: false,
            telemetry_levels: 0,
            last_rgb: None,
            uart_buf: String::new(),
            lcd_ops: None,
            next_hello_ms: 0,
            outbox: Vec::new(),
            scripts: ScriptHost::new(),
            http: crate::http_fetch::HttpPool::default(),
            synced_scripts: 0,
            scripts_topo: 0,
            synced_flow: 0,
        }
    }
}

impl LiveState {
    pub fn connected(&self) -> bool {
        self.session.is_some()
    }

    pub fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    pub fn refresh_ports(&mut self) {
        self.ports = available_ports();
        self.selected_port = self.selected_port.min(self.ports.len().saturating_sub(1));
    }

    pub fn connect_sim(
        &mut self,
        lib: &Library,
        circuit: &Circuit,
        log: &mut Vec<String>,
    ) {
        let Some(board) = lib.board(&circuit.board_id) else {
            log.push(format!("unknown board '{}'", circuit.board_id));
            return;
        };
        let dev = SimDevice::new(board.clone(), lib.clone(), circuit.clone());
        match Session::new(Box::new(dev)) {
            Ok(session) => {
                self.session = Some(session);
                self.backend = Backend::Simulator;
                self.setup_sent = false;
                self.synced_topo = 0;
                log.push("connected to simulator".into());
            }
            Err(e) => log.push(format!("simulator connect failed: {e}")),
        }
    }

    pub fn connect_serial(&mut self, log: &mut Vec<String>) {
        let Some(port) = self.ports.get(self.selected_port).cloned() else {
            log.push("no serial port selected".into());
            return;
        };
        match SerialDevice::open(&port, DEFAULT_BAUD) {
            Ok(dev) => match Session::new(Box::new(dev)) {
                Ok(session) => {
                    self.session = Some(session);
                    self.backend = Backend::Serial;
                    self.setup_sent = false;
                    log.push(format!("opened {port}, waiting for hello..."));
                }
                Err(e) => log.push(format!("handshake failed: {e}")),
            },
            Err(e) => log.push(format!("open {port} failed: {e}")),
        }
    }

    pub fn connect_tcp(&mut self, log: &mut Vec<String>) {
        let mut addr = self.tcp_addr.trim().to_string();
        if addr.is_empty() {
            log.push("enter or pick a board address first".into());
            return;
        }
        if !addr.contains(':') {
            addr = format!("{addr}:{}", wirelab_link::tcp::DEFAULT_TCP_PORT);
        }
        match TcpDevice::connect(&addr) {
            Ok(dev) => match Session::new(Box::new(dev)) {
                Ok(session) => {
                    self.session = Some(session);
                    self.backend = Backend::Tcp;
                    self.setup_sent = false;
                    log.push(format!("connected to {addr}, waiting for hello..."));
                }
                Err(e) => log.push(format!("handshake failed: {e}")),
            },
            Err(e) => log.push(format!("connect {addr} failed: {e}")),
        }
    }

    /// Send Wi-Fi credentials to the connected board; it answers WifiStatus.
    pub fn provision_wifi(&mut self, log: &mut Vec<String>) {
        let Some(session) = &mut self.session else {
            log.push("connect over serial first, then send Wi-Fi credentials".into());
            return;
        };
        let (Ok(ssid), Ok(pass)) = (
            wirelab_proto::heapless::String::try_from(self.wifi_ssid.trim()),
            wirelab_proto::heapless::String::try_from(self.wifi_pass.as_str()),
        ) else {
            log.push("SSID is limited to 32 chars, password to 64".into());
            return;
        };
        let _ = session.send(&wirelab_proto::HostMsg::WifiConfig { ssid, pass });
        log.push(format!("wifi: joining '{}'…", self.wifi_ssid.trim()));
    }

    /// Deliver a cross-board message to every scripted component here; any
    /// replies (`send_board` from the handlers) go back into the outbox.
    pub fn deliver_board_msg(&mut self, from: &str, text: &str, log: &mut Vec<String>) {
        if self.session.is_none() {
            return;
        }
        let mut actions: Vec<Action> = Vec::new();
        for comp in self.scripts.scripted() {
            actions.extend(self.scripts.on_board_msg(comp, from, text));
        }
        actions.retain(|a| match a {
            Action::BoardMsg { to, text } => {
                self.outbox.push((to.clone(), text.clone()));
                false
            }
            Action::HttpGet { url } => {
                if !self.http.spawn(url.clone()) {
                    log.push(format!("http_get dropped (too many in flight): {url}"));
                }
                false
            }
            _ => true,
        });
        for line in self.scripts.take_logs() {
            log.push(line);
        }
        let now = self.now_ms();
        let msgs = self.engine.run_script_actions(actions, now);
        if let Some(session) = &mut self.session
            && let Err(e) = session.send_all(&msgs)
        {
            log.push(format!("board-msg send failed: {e}"));
        }
    }

    pub fn disconnect(&mut self, log: &mut Vec<String>) {
        if let Some(mut s) = self.session.take() {
            let _ = s.send(&wirelab_proto::HostMsg::Reset);
        }
        self.program_running = false;
        self.live_output = None;
        self.setup_sent = false;
        self.last_rgb = None;
        self.lcd_ops = None;
        log.push("disconnected".into());
    }

    pub fn start_program(&mut self, program: &Program, bindings: &Bindings, log: &mut Vec<String>) {
        // Reuse the engine: its output shadow, behavior slots and any script
        // continuations must survive a program (re)start.
        self.engine.program = program.clone();
        self.engine.set_bindings(bindings.clone());
        let cmds = self.engine.start(self.now_ms());
        if let Some(session) = &mut self.session
            && let Err(e) = session.send_all(&cmds) {
                log.push(format!("send failed: {e}"));
            }
        self.program_running = true;
        log.push(format!("program started ({} rules)", program.rules.len()));
    }

    pub fn stop_program(&mut self, log: &mut Vec<String>) {
        self.engine.stop();
        self.program_running = false;
        log.push("program stopped".into());
    }

    /// The on-board RESET button: pulse EN over serial, or reset the sim.
    pub fn board_reset(&mut self, log: &mut Vec<String>) {
        let Some(session) = &mut self.session else {
            log.push("connect first to use the reset button".into());
            return;
        };
        match self.backend {
            Backend::Serial | Backend::Tcp => {
                if session.control(ControlRequest::PulseReset) {
                    session.phase = SessionPhase::AwaitingHello;
                    session.info = None;
                    self.setup_sent = false;
                    self.program_running = false;
                    // Give the ROM boot log a moment before re-handshaking.
                    self.next_hello_ms = self.now_ms() + 700;
                    log.push("board reset (EN pulsed) — waiting for hello…".into());
                } else {
                    log.push("this transport cannot pulse reset".into());
                }
            }
            Backend::Simulator => {
                let _ = session.send(&wirelab_proto::HostMsg::Reset);
                self.setup_sent = false;
                log.push("simulator reset — pin setup will re-apply".into());
            }
        }
    }

    /// The on-board BOOT button: download mode on hardware, a simulated
    /// press of the boot GPIO in the simulator.
    pub fn board_boot_mode(&mut self, boot_gpio: Option<u8>, log: &mut Vec<String>) {
        let Some(session) = &mut self.session else {
            log.push("connect first to use the boot button".into());
            return;
        };
        match self.backend {
            Backend::Serial | Backend::Tcp => {
                if session.control(ControlRequest::EnterBootloader) {
                    session.phase = SessionPhase::AwaitingHello;
                    session.info = None;
                    self.setup_sent = false;
                    self.program_running = false;
                    log.push(
                        "entered ROM download mode — flash with espflash, then click RESET"
                            .into(),
                    );
                } else {
                    log.push("this transport cannot enter download mode".into());
                }
            }
            Backend::Simulator => match boot_gpio {
                Some(gpio) => {
                    if let Some(dev) =
                        session.device.as_any_mut().downcast_mut::<SimDevice>()
                    {
                        dev.press_pin(gpio, 150);
                        log.push(format!("BOOT pressed (GPIO{gpio} low for 150 ms)"));
                    }
                }
                None => log.push("this board has no BOOT button".into()),
            },
        }
    }

    /// One frame of the live loop; returns true when a repaint should follow.
    #[allow(clippy::too_many_arguments)]
    pub fn tick(
        &mut self,
        lib: &Library,
        circuit: &Circuit,
        netlist: &Netlist,
        bindings: &Bindings,
        topo_rev: u64,
        state_rev: u64,
        script_rev: u64,
        flow_rev: u64,
        flow_code: Option<&str>,
        log: &mut Vec<String>,
    ) -> bool {
        let now = self.epoch.elapsed().as_millis() as u64;
        let Some(session) = &mut self.session else { return false };
        let board = match lib.board(&circuit.board_id) {
            Some(b) => b.clone(),
            None => return false,
        };

        if session.phase == SessionPhase::Dead {
            log.push("device disconnected".into());
            self.session = None;
            self.program_running = false;
            return false;
        }

        // Boards reset when the port opens; repeat Hello until answered.
        if session.phase == SessionPhase::AwaitingHello && now >= self.next_hello_ms {
            self.next_hello_ms = now + 500;
            let _ = session.send(&wirelab_proto::HostMsg::Hello {
                proto: wirelab_proto::PROTO_VERSION,
            });
        }

        // Push wiring edits into the simulator.
        if self.backend == Backend::Simulator
            && (self.synced_topo != topo_rev || self.synced_state != state_rev)
        {
            if let Some(dev) = session.device.as_any_mut().downcast_mut::<SimDevice>() {
                dev.sync_circuit(circuit);
            }
            self.synced_state = state_rev;
        }

        // Scripts follow circuit edits; on_start fires for fresh compiles.
        let mut script_actions: Vec<Action> = Vec::new();
        if self.synced_scripts != script_rev || self.scripts_topo != topo_rev {
            self.scripts.set_board(
                board.chip.name(),
                &board.specs,
                board.features.rgb_led_gpio,
            );
            let fresh = self.scripts.sync(circuit, lib);
            self.synced_scripts = script_rev;
            self.scripts_topo = topo_rev;
            if session.phase == SessionPhase::Ready {
                for c in fresh {
                    script_actions.extend(self.scripts.on_start(c));
                }
            }
        }

        // The flow graph rides the same script pipeline as a synthetic
        // instance; on_start re-initializes its node state on every recompile.
        if self.synced_flow != flow_rev {
            self.synced_flow = flow_rev;
            if self.scripts.set_flow_script(flow_code) && session.phase == SessionPhase::Ready {
                script_actions.extend(self.scripts.on_start(wirelab_core::script::FLOW_ID));
            }
        }

        // (Re)apply auto pin setup once ready and whenever the wiring changes.
        if session.phase == SessionPhase::Ready && (!self.setup_sent || self.synced_topo != topo_rev)
        {
            let (msgs, _) = plan_setup(circuit, &board, lib, netlist);
            if let Err(e) = session.send_all(&msgs) {
                log.push(format!("setup send failed: {e}"));
            } else {
                log.push(format!("pin setup applied ({} commands)", msgs.len()));
            }
            self.engine.set_bindings(bindings.clone());
            if !self.setup_sent {
                for c in self.scripts.scripted() {
                    script_actions.extend(self.scripts.on_start(c));
                }
            }
            self.setup_sent = true;
            self.synced_topo = topo_rev;
        }

        // Pump device -> engine + scripts -> device.
        let msgs = session.update();
        let mut out = Vec::new();
        if self.program_running {
            for m in &msgs {
                out.extend(self.engine.handle_device(now, m));
            }
        }

        // Snapshot for script-side reads (is_on, is_pressed, millivolts...).
        let mut world = World { levels: session.levels, now_ms: now, ..Default::default() };
        for (comp, b) in &self.engine.bindings.outputs {
            world.outputs_on.insert(*comp, self.engine.out_high(b.gpio) == b.active_high);
        }
        for (gpio, b) in &self.engine.bindings.inputs {
            let level = session.levels & (1u64 << (*gpio).min(63)) != 0;
            world.inputs_on.insert(b.comp, level != b.active_low);
        }
        for (comp, gpio) in &self.engine.bindings.analog {
            if let Some(mv) = session.analog.get(gpio) {
                world.analog_mv.insert(*comp, *mv);
            }
        }
        world.pin_analog_mv = session.analog.clone();
        self.scripts.set_world(world);

        for m in &msgs {
            match m {
                DeviceMsg::Event { pin, edge, .. } => {
                    if let Some(b) = self.engine.bindings.inputs.get(pin).copied() {
                        let logical = (*edge == EventEdge::Rising) != b.active_low;
                        if b.kind == InKind::Button {
                            script_actions.extend(if logical {
                                self.scripts.on_press(b.comp)
                            } else {
                                self.scripts.on_release(b.comp)
                            });
                        }
                        script_actions.extend(self.scripts.on_change(b.comp, logical));
                    }
                    // Raw edge for every scripted component (e.g. BOOT button).
                    let high = *edge == EventEdge::Rising;
                    for comp in self.scripts.scripted() {
                        script_actions.extend(self.scripts.on_pin(comp, *pin, high));
                    }
                }
                DeviceMsg::Telemetry { analog, .. } => {
                    for s in analog.iter() {
                        let bound: Vec<_> = self
                            .engine
                            .bindings
                            .analog
                            .iter()
                            .filter(|(_, g)| **g == s.pin)
                            .map(|(c, _)| *c)
                            .collect();
                        for comp in bound {
                            script_actions.extend(self.scripts.on_reading(comp, s.millivolts));
                        }
                    }
                }
                DeviceMsg::SpiData { data } => {
                    for comp in self.scripts.scripted() {
                        script_actions.extend(self.scripts.on_spi(comp, data));
                    }
                }
                DeviceMsg::I2cData { addr, data } => {
                    for comp in self.scripts.scripted() {
                        script_actions.extend(self.scripts.on_i2c(comp, *addr, data));
                    }
                }
                DeviceMsg::UartData { data } => {
                    self.uart_buf.push_str(&String::from_utf8_lossy(data));
                    while let Some(nl) = self.uart_buf.find('\n') {
                        let line: String =
                            self.uart_buf.drain(..=nl).collect::<String>().trim_end().to_string();
                        for comp in self.scripts.scripted() {
                            script_actions.extend(self.scripts.on_uart(comp, &line));
                        }
                    }
                    if self.uart_buf.len() > 4096 {
                        self.uart_buf.clear();
                    }
                }
                DeviceMsg::AnalogValue { pin, millivolts } => {
                    let bound: Vec<_> = self
                        .engine
                        .bindings
                        .analog
                        .iter()
                        .filter(|(_, g)| *g == pin)
                        .map(|(c, _)| *c)
                        .collect();
                    for comp in bound {
                        script_actions.extend(self.scripts.on_reading(comp, *millivolts));
                    }
                }
                _ => {}
            }
        }
        // Finished http_get requests broadcast to every scripted component;
        // their handlers' actions join this frame's batch below.
        for (status, body) in self.http.drain_done() {
            for comp in self.scripts.scripted() {
                script_actions.extend(self.scripts.on_http(comp, i64::from(status), &body));
            }
        }
        script_actions.extend(self.scripts.tick(now));
        if !script_actions.is_empty() {
            // Cross-board messages and HTTP fetches run host-side, not on the device.
            script_actions.retain(|a| match a {
                Action::BoardMsg { to, text } => {
                    self.outbox.push((to.clone(), text.clone()));
                    false
                }
                Action::HttpGet { url } => {
                    if !self.http.spawn(url.clone()) {
                        log.push(format!("http_get dropped (too many in flight): {url}"));
                    }
                    false
                }
                _ => true,
            });
            out.extend(self.engine.run_script_actions(script_actions, now));
        }
        out.extend(self.engine.tick(now));

        for m in &out {
            use wirelab_core::sim::{LcdOp, rgb888};
            match m {
                wirelab_proto::HostMsg::SetRgb { r, g, b, .. } => {
                    self.last_rgb = Some([*r, *g, *b]);
                }
                wirelab_proto::HostMsg::LcdInit { .. } => {
                    self.lcd_ops = Some(vec![LcdOp::Clear([0, 0, 0])]);
                }
                wirelab_proto::HostMsg::LcdClear { rgb565 } => {
                    if let Some(ops) = &mut self.lcd_ops {
                        ops.clear();
                        ops.push(LcdOp::Clear(rgb888(*rgb565)));
                    }
                }
                wirelab_proto::HostMsg::LcdRect { x, y, w, h, rgb565 } => {
                    if let Some(ops) = &mut self.lcd_ops {
                        ops.push(LcdOp::Rect { x: *x, y: *y, w: *w, h: *h, rgb: rgb888(*rgb565) });
                        if ops.len() > 512 {
                            ops.drain(..256);
                        }
                    }
                }
                wirelab_proto::HostMsg::LcdText { x, y, rgb565, text } => {
                    if let Some(ops) = &mut self.lcd_ops {
                        ops.push(LcdOp::Text {
                            x: *x,
                            y: *y,
                            rgb: rgb888(*rgb565),
                            text: text.to_string(),
                        });
                        if ops.len() > 512 {
                            ops.drain(..256);
                        }
                    }
                }
                _ => {}
            }
        }
        if !out.is_empty()
            && let Err(e) = session.send_all(&out) {
                log.push(format!("send failed: {e}"));
            }
        for line in session.log.drain(..) {
            log.push(line);
        }
        for line in self.engine.log.drain(..) {
            log.push(line);
        }
        for line in self.scripts.take_logs() {
            log.push(line);
        }

        self.telemetry_levels = session.levels;

        // Visualization state: simulator output is authoritative; for serial,
        // solve locally against telemetry-backed pin state.
        self.live_output = if self.backend == Backend::Simulator {
            session
                .device
                .as_any_mut()
                .downcast_mut::<SimDevice>()
                .map(|d| d.last_output.clone())
        } else {
            let bank = session.effective_bank();
            let mut out = solve(circuit, &board, lib, netlist, &bank);
            out.rgb = self.last_rgb;
            out.lcd = self.lcd_ops.clone();
            Some(out)
        };
        true
    }

    /// Pin bank used for painting pin states.
    pub fn effective_bank(&self) -> Option<PinBank> {
        self.session.as_ref().map(|s| s.effective_bank())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wirelab_core::circuit::{CompId, PlacedComponent};
    use wirelab_core::component::{CompState, SimModel};

    fn test_lib() -> Library {
        let assets = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        Library::load(&assets.join("boards"), &assets.join("components")).expect("assets")
    }

    fn circuit_with_button_script(script: &str) -> Circuit {
        let mut c = Circuit::new("esp32-c5-devkitc-1");
        c.add_component(PlacedComponent {
            id: CompId(0),
            def_id: "push-button".into(),
            pos: [0.0, 0.0],
            rotation: 0,
            label: "btn".into(),
            props: Default::default(),
            state: CompState::initial(&SimModel::PushButton),
            script: Some(script.into()),
        });
        c
    }

    /// Bring a sim session up and run its scripts' on_start.
    fn live_for(lib: &Library, circuit: &Circuit) -> (LiveState, Netlist, Bindings) {
        let board = lib.board(&circuit.board_id).expect("board").clone();
        let netlist = Netlist::build(circuit, &board, lib);
        let (_msgs, bindings) = plan_setup(circuit, &board, lib, &netlist);
        let mut live = LiveState::default();
        let mut log = Vec::new();
        live.connect_sim(lib, circuit, &mut log);
        for _ in 0..5 {
            live.tick(lib, circuit, &netlist, &bindings, 1, 1, 1, 1, None, &mut log);
        }
        (live, netlist, bindings)
    }

    /// The exact mechanics the app's router relies on: a script's send_board
    /// lands in the outbox during tick, and deliver_board_msg dispatches
    /// on_board_msg on the receiving board's scripts.
    #[test]
    fn send_board_routes_between_two_live_sims() {
        let lib = test_lib();
        let ca = circuit_with_button_script(r#"fn on_start() { send_board("b", "ping"); }"#);
        let cb = circuit_with_button_script(
            r#"fn on_board_msg(from, text) { log(`${from} -> ${text}`); }"#,
        );
        let (mut la, _nla, _ba) = live_for(&lib, &ca);
        let (mut lb, _nlb, _bb) = live_for(&lib, &cb);
        assert!(la.connected() && lb.connected());

        // A's on_start queued the message for the router, not the device.
        let mail: Vec<_> = la.outbox.drain(..).collect();
        assert_eq!(mail, vec![("b".to_string(), "ping".to_string())]);

        // Deliver to B exactly like the app does; its handler logs.
        let mut log = Vec::new();
        lb.deliver_board_msg("a", "ping", &mut log);
        assert!(
            log.iter().any(|l| l.contains("a -> ping")),
            "receiver handler ran: {log:?}"
        );
    }
}
