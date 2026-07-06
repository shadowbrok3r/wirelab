//! Full host stack against the simulated device: session handshake,
//! auto pin setup, rule engine, button press -> LED toggle.

use wirelab_core::board::{BoardPin, BoardProfile, PinCaps, PinKind, Side};
use wirelab_core::circuit::{Circuit, CompId, Endpoint, PlacedComponent};
use wirelab_core::component::{
    CompState, ComponentDef, Shape, SimModel, TerminalDef, TerminalRole, Visual, VisualState,
};
use wirelab_core::engine::{Engine, plan_setup};
use wirelab_core::library::Library;
use wirelab_core::netlist::Netlist;
use wirelab_core::program::{Action, Program, Rule, Trigger};
use wirelab_link::sim::SimDevice;
use wirelab_link::{Session, SessionPhase};

fn board() -> BoardProfile {
    let gpio = |n: u8, idx| BoardPin {
        key: format!("GPIO{n}"),
        label: format!("IO{n}"),
        kind: PinKind::Gpio(n),
        side: Side::Left,
        index: idx,
        caps: PinCaps::DIGITAL_IN | PinCaps::DIGITAL_OUT | PinCaps::PWM,
        adc: None,
        warning: None,
        tags: vec![],
    };
    BoardProfile {
        id: "sim-board".into(),
        name: "Sim Board".into(),
        chip: wirelab_proto::ChipKind::Esp32C3,
        description: String::new(),
        width_mm: 26.0,
        height_mm: 50.0,
        notes: vec![],
        specs: vec![],
        features: Default::default(),
        pins: vec![
            BoardPin {
                key: "GND1".into(),
                label: "GND".into(),
                kind: PinKind::Gnd,
                side: Side::Right,
                index: 0,
                caps: PinCaps::empty(),
                adc: None,
                warning: None,
                tags: vec![],
            },
            gpio(2, 0),
            gpio(4, 1),
        ],
    }
}

fn library() -> Library {
    let mut lib = Library::default();
    let term = |id: &str, role| TerminalDef { id: id.into(), name: id.into(), role };
    lib.add_component(ComponentDef {
        id: "led".into(),
        name: "LED".into(),
        category: "o".into(),
        description: String::new(),
        terminals: vec![term("anode", TerminalRole::Anode), term("cathode", TerminalRole::Cathode)],
        visual: Visual { shape: Shape::Led, color: [255, 0, 0], width_mm: 5.0, height_mm: 5.0 },
        sim: SimModel::Led { forward_mv: 1900 },
        actions: vec![],
        events: vec![],
        props: vec![],
    });
    lib.add_component(ComponentDef {
        id: "r220".into(),
        name: "220R".into(),
        category: "p".into(),
        description: String::new(),
        terminals: vec![term("a", TerminalRole::A), term("b", TerminalRole::B)],
        visual: Visual { shape: Shape::Resistor, color: [200, 180, 120], width_mm: 8.0, height_mm: 3.0 },
        sim: SimModel::Resistor { ohms: 220.0 },
        actions: vec![],
        events: vec![],
        props: vec![],
    });
    lib.add_component(ComponentDef {
        id: "button".into(),
        name: "Button".into(),
        category: "i".into(),
        description: String::new(),
        terminals: vec![term("a", TerminalRole::A), term("b", TerminalRole::B)],
        visual: Visual { shape: Shape::PushButton, color: [60, 60, 60], width_mm: 6.0, height_mm: 6.0 },
        sim: SimModel::PushButton,
        actions: vec![],
        events: vec![],
        props: vec![],
    });
    lib
}

#[test]
fn button_press_toggles_led_through_full_stack() {
    let board = board();
    let lib = library();

    let mut circuit = Circuit::new("sim-board");
    let mk = |def_id: &str, sim: &SimModel| PlacedComponent {
        id: CompId(0),
        def_id: def_id.into(),
        pos: [0.0, 0.0],
        rotation: 0,
        label: String::new(),
        props: Default::default(),
        state: CompState::initial(sim),
                    script: None,
    };
    let led = circuit.add_component(mk("led", &SimModel::Led { forward_mv: 1900 }));
    let res = circuit.add_component(mk("r220", &SimModel::Resistor { ohms: 220.0 }));
    let btn = circuit.add_component(mk("button", &SimModel::PushButton));
    let bp = |key: &str| Endpoint::BoardPin { key: key.into() };
    let tp = |c: CompId, t: &str| Endpoint::Terminal { comp: c, terminal: t.into() };
    circuit.add_wire(bp("GPIO2"), tp(res, "a"), [255, 0, 0]);
    circuit.add_wire(tp(res, "b"), tp(led, "anode"), [255, 0, 0]);
    circuit.add_wire(tp(led, "cathode"), bp("GND1"), [30, 30, 30]);
    circuit.add_wire(bp("GPIO4"), tp(btn, "a"), [0, 180, 0]);
    circuit.add_wire(tp(btn, "b"), bp("GND1"), [30, 30, 30]);

    let netlist = Netlist::build(&circuit, &board, &lib);
    let (setup, bindings) = plan_setup(&circuit, &board, &lib, &netlist);

    let sim = SimDevice::new(board.clone(), lib.clone(), circuit.clone());
    let mut session = Session::new(Box::new(sim)).expect("session");

    // Handshake.
    let _ = session.update();
    assert_eq!(session.phase, SessionPhase::Ready);
    let info = session.info.expect("hello ack");
    assert_eq!(info.gpio_mask, board.gpio_mask());

    session.send_all(&setup).expect("setup");

    let program = Program {
        rules: vec![Rule {
            name: "toggle".into(),
            enabled: true,
            trigger: Trigger::CompEvent { comp: btn, event: "pressed".into() },
            actions: vec![Action::CompAction {
                comp: led,
                action: "toggle".into(),
                params: Default::default(),
            }],
        }],
    };
    let mut engine = Engine::new(program, bindings);
    let cmds = engine.start(0);
    assert!(cmds.is_empty());

    fn sim_dev(session: &mut Session) -> &mut SimDevice {
        session.device.as_any_mut().downcast_mut::<SimDevice>().expect("sim device")
    }

    // Seed input levels (pull-up reads high while released).
    sim_dev(&mut session).tick(50);
    let msgs = session.update();
    assert!(msgs.iter().all(|m| !matches!(m, wirelab_proto::DeviceMsg::Event { .. })));

    // Press the button.
    {
        let dev = sim_dev(&mut session);
        let mut c2 = circuit.clone();
        c2.components.get_mut(&btn).unwrap().state = CompState::Button { pressed: true };
        dev.sync_circuit(&c2);
        dev.tick(100);
    }
    let msgs = session.update();
    let mut to_send = Vec::new();
    for m in &msgs {
        to_send.extend(engine.handle_device(100, m));
    }
    assert!(
        to_send.contains(&wirelab_proto::HostMsg::WriteDigital { pin: 2, high: true }),
        "engine should light the LED: {to_send:?}"
    );
    session.send_all(&to_send).expect("send");

    // LED actually lit in the electrical solve.
    {
        let dev = sim_dev(&mut session);
        dev.tick(150);
        match dev.last_output.visuals.get(&led) {
            Some(&VisualState::LedBrightness(b)) => assert!(b > 0.5, "brightness {b}"),
            v => panic!("unexpected visual {v:?}"),
        }
    }

    // Telemetry carries the lit pin.
    {
        let dev = sim_dev(&mut session);
        dev.tick(220);
    }
    let _ = session.update();
    assert!(session.levels & (1 << 2) != 0, "levels {:#b}", session.levels);
}

#[test]
fn sim_uart_loopback() {
    use wirelab_link::Device;
    use wirelab_proto::{DeviceMsg as DM, HostMsg as HM};
    let circuit = Circuit::new("sim-board");
    let mut dev = SimDevice::new(board(), library(), circuit);
    dev.send(&HM::UartConfig { tx: 4, rx: 5, baud: 115_200 }).unwrap();
    dev.send(&HM::UartWrite {
        data: wirelab_proto::heapless::Vec::from_slice(b"ping\n").unwrap(),
    })
    .unwrap();
    let echoed = dev
        .poll()
        .into_iter()
        .any(|m| matches!(m, DM::UartData { data } if data.as_slice() == b"ping\n"));
    assert!(echoed, "simulator echoes UART writes");
}

/// Virtual I2C sensors: BME280 at 0x76, SHT31 at 0x44, zeros elsewhere.
#[test]
fn sim_i2c_virtual_sensors() {
    use wirelab_link::Device;
    use wirelab_proto::{DeviceMsg as DM, HostMsg as HM};

    fn read(dev: &mut SimDevice, addr: u8, reg: u16, len: u8) -> Vec<u8> {
        dev.send(&HM::I2cRead { addr, reg, len }).unwrap();
        dev.poll()
            .into_iter()
            .find_map(|m| match m {
                DM::I2cData { addr: a, data } if a == addr => Some(data.to_vec()),
                _ => None,
            })
            .expect("i2c reply")
    }

    let mut dev = SimDevice::new(board(), library(), Circuit::new("sim-board"));
    dev.send(&HM::I2cConfig { sda: 0, scl: 1, freq_khz: 400 }).unwrap();

    // BME280 chip id.
    dev.tick(10);
    assert_eq!(read(&mut dev, 0x76, 0xd0, 1), vec![0x60]);

    // Temperature stays in range and drifts with sim time.
    let t0 = read(&mut dev, 0x76, 0xfa, 2);
    dev.tick(15_000);
    let t1 = read(&mut dev, 0x76, 0xfa, 2);
    let decode = |b: &[u8]| i16::from_be_bytes([b[0], b[1]]);
    let (v0, v1) = (decode(&t0), decode(&t1));
    assert_ne!(v0, v1, "temperature should drift");
    for v in [v0, v1] {
        assert!((2000..=2800).contains(&v), "temp_c_x100 {v}");
    }

    // SHT31 frame decodes to plausible temperature and humidity.
    let f = read(&mut dev, 0x44, 256, 6);
    assert_eq!(f.len(), 6);
    let temp_c = f64::from(u16::from_be_bytes([f[0], f[1]])) / 65535.0 * 175.0 - 45.0;
    let rh = f64::from(u16::from_be_bytes([f[3], f[4]])) / 65535.0 * 100.0;
    assert!((15.0..35.0).contains(&temp_c), "temp {temp_c}");
    assert!((35.0..65.0).contains(&rh), "humidity {rh}");

    // Unknown address still zero-fills.
    assert_eq!(read(&mut dev, 0x23, 0, 4), vec![0, 0, 0, 0]);
}

/// A full Session over the TCP transport: a fake board on localhost answers
/// the handshake and pushes WifiStatus, which lands in `session.wifi`.
#[test]
fn session_runs_over_tcp() {
    use std::io::{Read, Write};
    use wirelab_proto::frame::{Decoder, encode};
    use wirelab_proto::{DeviceMsg, HostMsg, MAX_FRAME, PROTO_VERSION, WifiState};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        sock.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
        let mut dec: Decoder<HostMsg> = Decoder::new();
        let mut buf = [0u8; 256];
        let mut out = [0u8; MAX_FRAME];
        loop {
            let n = match sock.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            for &b in &buf[..n] {
                let Some(Ok(msg)) = dec.push(b) else { continue };
                let reply = match msg {
                    HostMsg::Hello { .. } => DeviceMsg::HelloAck {
                        proto: PROTO_VERSION,
                        fw_version: 1,
                        chip: wirelab_proto::ChipKind::Esp32C5,
                        gpio_mask: 0xff,
                        input_only_mask: 0,
                    },
                    HostMsg::WifiStatusReq => DeviceMsg::WifiStatus {
                        state: WifiState::Connected,
                        ip: [10, 0, 0, 7],
                    },
                    _ => continue,
                };
                let n = encode(&reply, &mut out).unwrap();
                sock.write_all(&out[..n]).unwrap();
            }
        }
    });

    let dev = wirelab_link::tcp::TcpDevice::connect(&addr.to_string()).unwrap();
    let mut session = Session::new(Box::new(dev)).unwrap();
    session.send(&wirelab_proto::HostMsg::WifiStatusReq).unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline
        && !(session.phase == SessionPhase::Ready && session.wifi.is_some())
    {
        session.update();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(session.phase, SessionPhase::Ready);
    assert_eq!(
        session.wifi,
        Some((wirelab_proto::WifiState::Connected, [10, 0, 0, 7]))
    );
    drop(session);
    let _ = server.join();
}
