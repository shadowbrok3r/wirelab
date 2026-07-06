//! Per-component behavior scripts (Rhai), Godot-style.
//!
//! Each placed component may carry a script defining callbacks (`on_press`,
//! `on_change`, `on_reading`, `on_tick`...). Scripts refer to sibling
//! components by their sanitized label (`led1.toggle()`) and queue
//! [`Action`]s that the rules engine turns into device commands, so the same
//! script drives the simulator and real hardware.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use rhai::{AST, CallFnOptions, Dynamic, Engine as RhaiEngine, FnPtr, Scope};

use crate::circuit::{Circuit, CompId};
use crate::component::{ComponentDef, SimModel};
use crate::library::Library;
use crate::program::Action;

/// Snapshot of live component/pin state readable from scripts.
#[derive(Debug, Clone, Default)]
pub struct World {
    /// Logical on/off per output-bound component (polarity corrected).
    pub outputs_on: HashMap<CompId, bool>,
    /// Logical pressed/on per input-bound component.
    pub inputs_on: HashMap<CompId, bool>,
    /// Latest analog reading per analog-bound component.
    pub analog_mv: HashMap<CompId, u16>,
    /// Raw digital levels, bit N = GPIO N.
    pub levels: u64,
    /// Latest ADC sample per watched GPIO, millivolts.
    pub pin_analog_mv: HashMap<u8, u16>,
    pub now_ms: u64,
}

#[derive(Default)]
struct Fx {
    actions: Vec<Action>,
    logs: Vec<String>,
}

struct Timer {
    comp: CompId,
    due_ms: u64,
    f: FnPtr,
}

#[derive(Clone)]
struct Shared {
    fx: Rc<RefCell<Fx>>,
    world: Rc<RefCell<World>>,
    names: Rc<RefCell<HashMap<String, CompId>>>,
    current: Rc<RefCell<CompId>>,
    timers: Rc<RefCell<Vec<Timer>>>,
    /// Chip name, lowercased spec lines and RGB LED pin of the active board.
    board: Rc<RefCell<BoardInfo>>,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            fx: Rc::new(RefCell::new(Fx::default())),
            world: Rc::new(RefCell::new(World::default())),
            names: Rc::new(RefCell::new(HashMap::new())),
            current: Rc::new(RefCell::new(CompId(0))),
            timers: Rc::new(RefCell::new(Vec::new())),
            board: Rc::new(RefCell::new(BoardInfo::default())),
        }
    }
}

fn v_dim(v: i64) -> u8 {
    v.clamp(0, 128) as u8
}

#[derive(Default)]
struct BoardInfo {
    chip: String,
    specs_lc: Vec<String>,
    rgb_gpio: Option<u8>,
}

/// Script-side handle to a placed component.
#[derive(Clone)]
struct CompHandle {
    id: CompId,
    sh: Shared,
}

impl CompHandle {
    fn act(&mut self, verb: &str, params: &[(&str, f64)]) {
        let mut map = crate::component::PropMap::new();
        for (k, v) in params {
            map.insert((*k).into(), *v);
        }
        self.sh.fx.borrow_mut().actions.push(Action::CompAction {
            comp: self.id,
            action: verb.into(),
            params: map,
        });
    }
}

/// Script-side handle to a raw GPIO.
#[derive(Clone)]
struct PinHandle {
    gpio: u8,
    sh: Shared,
}

const MAX_TIMERS_PER_COMP: usize = 64;

fn sanitize_ident(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_end_matches('_');
    if out.is_empty() {
        return "comp".into();
    }
    let reserved = [
        "fn", "let", "const", "if", "else", "switch", "while", "loop", "for", "do", "until",
        "in", "true", "false", "return", "throw", "try", "catch", "import", "export", "as",
        "global", "private", "break", "continue", "this", "me", "pin", "log", "millis",
        "after", "comp",
    ];
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) || reserved.contains(&out) {
        format!("c_{out}")
    } else {
        out.to_string()
    }
}

/// Short, overlay-friendly rendering of a script value.
fn fmt_dynamic(v: &Dynamic) -> String {
    if let Ok(b) = v.as_bool() {
        return if b { "on".into() } else { "off".into() };
    }
    if let Ok(f) = v.as_float() {
        return if f.fract().abs() < 1e-9 { format!("{f:.0}") } else { format!("{f:.1}") };
    }
    let mut s = v.to_string();
    if s.len() > 14 {
        s.truncate(13);
        s.push('…');
    }
    s
}

/// Stable script name for every placed component, derived from labels.
pub fn component_names(circuit: &Circuit, lib: &Library) -> HashMap<CompId, String> {
    let mut used: HashSet<String> = HashSet::new();
    let mut map = HashMap::new();
    for comp in circuit.components.values() {
        let base = if comp.label.trim().is_empty() {
            lib.component(&comp.def_id)
                .map(|d| sanitize_ident(&d.name))
                .unwrap_or_else(|| "comp".into())
        } else {
            sanitize_ident(&comp.label)
        };
        let mut name = base.clone();
        let mut n = 2;
        while !used.insert(name.clone()) {
            name = format!("{base}_{n}");
            n += 1;
        }
        map.insert(comp.id, name);
    }
    map
}

/// Starter script for a component, tailored to what it can do.
pub fn script_template(def: &ComponentDef, own: &str, peers: &[String]) -> String {
    let others = if peers.is_empty() {
        "(none yet — place & label more components, they appear here by name)".to_string()
    } else {
        peers.join(", ")
    };
    let peer = peers.first().cloned().unwrap_or_else(|| "some_led".into());
    let mut s = format!(
        "// You are `{own}` — {name}.\n\
         //\n\
         // How to refer to things:\n\
         //   me      = THIS component's handle      me.is_on(), me.toggle()\n\
         //   this    = your private state map       this.count = 0  (NOT the component!)\n\
         //   by name = every other component        {others}\n\
         //\n\
         // WireLab calls these when they exist:\n\
         //   on_start()  on_press()  on_release()  on_change(on)\n\
         //   on_reading(mv)  on_tick(dt_ms)  on_pin(gpio, high)\n\
         //\n\
         // The 📖 Reference button lists every verb; the editor autocompletes\n\
         // after `.` and shows docs on hover.\n\n",
        name = def.name,
    );
    match def.sim {
        SimModel::PushButton => {
            s += &format!(
                "fn on_start() {{\n    this.count = 0;\n}}\n\n\
                 fn on_press() {{\n    this.count += 1;\n    log(`pressed ${{this.count}} times`);\n    \
                 // {peer}.toggle();\n    // after(500, || {peer}.off());\n}}\n\n\
                 fn on_release() {{\n}}\n"
            );
        }
        SimModel::ToggleSwitch | SimModel::SlideSwitchSpdt => {
            s += &format!(
                "fn on_change(on) {{\n    log(`switch: ${{on}}`);\n    \
                 // Mirror it: if on {{ {peer}.on(); }} else {{ {peer}.off(); }}\n}}\n"
            );
        }
        SimModel::DigitalSensor => {
            s += &format!(
                "fn on_change(active) {{\n    if active {{\n        log(\"triggered\");\n        \
                 // {peer}.on(); after(3000, || {peer}.off());\n    }}\n}}\n"
            );
        }
        SimModel::Potentiometer { .. }
        | SimModel::Photoresistor { .. }
        | SimModel::AnalogSensor { .. } => {
            s += &format!(
                "fn on_reading(mv) {{\n    // 0..3300 millivolts, sent when it changes.\n    \
                 // {peer}.dim(mv * 100 / 3300);\n    log(mv);\n}}\n"
            );
        }
        SimModel::Led { .. } | SimModel::Buzzer { .. } | SimModel::RelayModule
        | SimModel::Servo => {
            s += "fn on_start() {\n    // Drive yourself: me.blink(500); me.breathe(2000);\n    \
                  // me.dim(30); me.set_angle(90); me.beep(150);\n    me.on();\n}\n\n\
                  // fn on_tick(dt_ms) {\n//     // animate here\n// }\n";
        }
        SimModel::Resistor { .. } | SimModel::Generic => {
            s += "// A routing dot makes a great board-level controller:\n\
                  fn on_start() {\n    pin(28).input_pullup();   // e.g. watch the BOOT button\n    \
                  log(\"controller ready\");\n}\n\n\
                  fn on_tick(dt_ms) {\n    // rgb(255, 0, 64);      // on-board RGB LED\n    \
                  // pin(2).pwm(1000, 500);\n}\n\n\
                  fn on_pin(gpio, high) {\n    if gpio == 28 && !high {\n        log(\"BOOT pressed\");\n    }\n}\n";
        }
    }
    s
}

struct Instance {
    src: String,
    ast: AST,
    state: Dynamic,
    handlers: Vec<(String, usize)>,
    last_tick_ms: u64,
}

/// Compiles, hot-swaps and dispatches all component scripts.
/// Reserved instance id for the compiled flow graph; never a real component.
pub const FLOW_ID: CompId = CompId(u32::MAX);

pub struct ScriptHost {
    engine: RhaiEngine,
    shared: Shared,
    instances: HashMap<CompId, Instance>,
    names: HashMap<CompId, String>,
    /// Latest compile/runtime error per component, cleared on success.
    pub errors: HashMap<CompId, String>,
    last_reading: HashMap<CompId, u16>,
}

impl Default for ScriptHost {
    fn default() -> Self {
        ScriptHost::new()
    }
}

impl ScriptHost {
    pub fn new() -> ScriptHost {
        let shared = Shared::new();
        let mut engine = RhaiEngine::new();
        engine.set_max_operations(200_000);
        engine.set_max_call_levels(32);
        // Default depth trips on idiomatic else-if chains (HSV pickers...).
        engine.set_max_expr_depths(128, 128);
        engine.disable_symbol("eval");

        engine.register_type_with_name::<CompHandle>("Component");
        engine.register_type_with_name::<PinHandle>("Pin");

        engine.register_fn("on", |h: &mut CompHandle| h.act("on", &[]));
        engine.register_fn("off", |h: &mut CompHandle| h.act("off", &[]));
        engine.register_fn("toggle", |h: &mut CompHandle| h.act("toggle", &[]));
        engine.register_fn("blink", |h: &mut CompHandle, ms: i64| {
            h.act("blink", &[("period_ms", ms as f64)]);
        });
        engine.register_fn("breathe", |h: &mut CompHandle, ms: i64| {
            h.act("breathe", &[("period_ms", ms as f64)]);
        });
        engine.register_fn("dim", |h: &mut CompHandle, pct: i64| {
            h.act("dim", &[("percent", pct as f64)]);
        });
        engine.register_fn("dim", |h: &mut CompHandle, pct: f64| {
            h.act("dim", &[("percent", pct)]);
        });
        engine.register_fn("set_angle", |h: &mut CompHandle, deg: i64| {
            h.act("set_angle", &[("degrees", deg as f64)]);
        });
        engine.register_fn("set_angle", |h: &mut CompHandle, deg: f64| {
            h.act("set_angle", &[("degrees", deg)]);
        });
        engine.register_fn("beep", |h: &mut CompHandle, ms: i64| {
            h.act("beep", &[("ms", ms as f64)]);
        });
        engine.register_fn("tone", |h: &mut CompHandle, hz: i64, ms: i64| {
            h.act("tone", &[("freq_hz", hz as f64), ("ms", ms as f64)]);
        });
        engine.register_fn("act", |h: &mut CompHandle, verb: &str| h.act(verb, &[]));
        engine.register_fn("is_on", |h: &mut CompHandle| -> bool {
            let w = h.sh.world.borrow();
            w.outputs_on
                .get(&h.id)
                .or_else(|| w.inputs_on.get(&h.id))
                .copied()
                .unwrap_or(false)
        });
        engine.register_fn("is_pressed", |h: &mut CompHandle| -> bool {
            h.sh.world.borrow().inputs_on.get(&h.id).copied().unwrap_or(false)
        });
        engine.register_fn("millivolts", |h: &mut CompHandle| -> i64 {
            i64::from(h.sh.world.borrow().analog_mv.get(&h.id).copied().unwrap_or(0))
        });

        engine.register_fn("high", |p: &mut PinHandle| {
            p.sh.fx.borrow_mut().actions.push(Action::SetPin { gpio: p.gpio, high: true });
        });
        engine.register_fn("low", |p: &mut PinHandle| {
            p.sh.fx.borrow_mut().actions.push(Action::SetPin { gpio: p.gpio, high: false });
        });
        engine.register_fn("set", |p: &mut PinHandle, high: bool| {
            p.sh.fx.borrow_mut().actions.push(Action::SetPin { gpio: p.gpio, high });
        });
        engine.register_fn("toggle", |p: &mut PinHandle| {
            p.sh.fx.borrow_mut().actions.push(Action::TogglePin { gpio: p.gpio });
        });
        engine.register_fn("pwm", |p: &mut PinHandle, freq_hz: i64, duty_permille: i64| {
            p.sh.fx.borrow_mut().actions.push(Action::SetPwm {
                gpio: p.gpio,
                freq_hz: freq_hz.clamp(1, 40_000) as u32,
                duty_permille: duty_permille.clamp(0, 1000) as u16,
            });
        });
        engine.register_fn("is_high", |p: &mut PinHandle| -> bool {
            p.sh.world.borrow().levels & (1u64 << p.gpio.min(63)) != 0
        });
        engine.register_fn("watch_analog", |p: &mut PinHandle, interval_ms: i64| {
            let mut fx = p.sh.fx.borrow_mut();
            fx.actions.push(Action::SetPinMode {
                gpio: p.gpio,
                mode: wirelab_proto::PinMode::Analog,
            });
            fx.actions.push(Action::WatchAnalog {
                gpio: p.gpio,
                interval_ms: interval_ms.clamp(0, 60_000) as u16,
            });
        });
        engine.register_fn("millivolts", |p: &mut PinHandle| -> i64 {
            i64::from(p.sh.world.borrow().pin_analog_mv.get(&p.gpio).copied().unwrap_or(0))
        });
        let mode_fn = |mode: wirelab_proto::PinMode| {
            move |p: &mut PinHandle| {
                p.sh.fx.borrow_mut().actions.push(Action::SetPinMode { gpio: p.gpio, mode });
            }
        };
        engine.register_fn("input_pullup", mode_fn(wirelab_proto::PinMode::InputPullUp));
        engine.register_fn("input_pulldown", mode_fn(wirelab_proto::PinMode::InputPullDown));
        engine.register_fn("input", mode_fn(wirelab_proto::PinMode::Input));
        engine.register_fn("output", mode_fn(wirelab_proto::PinMode::Output));

        let sh = shared.clone();
        engine.register_fn("pin", move |gpio: i64| -> PinHandle {
            PinHandle { gpio: gpio.clamp(0, 63) as u8, sh: sh.clone() }
        });
        let sh = shared.clone();
        engine.register_fn("log", move |v: Dynamic| {
            let cur = *sh.current.borrow();
            let names = sh.names.borrow();
            let line = match names.iter().find(|(_, id)| **id == cur) {
                Some((name, _)) => format!("[{name}] {v}"),
                None => v.to_string(),
            };
            sh.fx.borrow_mut().logs.push(line);
        });
        let sh = shared.clone();
        engine.register_fn("millis", move || -> i64 { sh.world.borrow().now_ms as i64 });
        let sh = shared.clone();
        engine.register_fn("uart", move |tx: i64, rx: i64, baud: i64| {
            sh.fx.borrow_mut().actions.push(Action::UartConfig {
                tx: tx.clamp(0, 63) as u8,
                rx: rx.clamp(0, 63) as u8,
                baud: baud.clamp(0, 5_000_000) as u32,
            });
        });
        let sh = shared.clone();
        engine.register_fn("uart_send", move |text: &str| {
            sh.fx.borrow_mut().actions.push(Action::UartWrite { data: text.bytes().collect() });
        });
        fn bytes_of(arr: &rhai::Array) -> Vec<u8> {
            arr.iter()
                .filter_map(|v| v.as_int().ok())
                .map(|i| i.clamp(0, 255) as u8)
                .collect()
        }
        let sh = shared.clone();
        engine.register_fn("spi_setup", move |sck: i64, mosi: i64, miso: i64, khz: i64| {
            let p = |v: i64| v.clamp(0, 63) as u8;
            sh.fx.borrow_mut().actions.push(Action::SpiConfig {
                sck: p(sck),
                mosi: p(mosi),
                miso: p(miso),
                freq_khz: khz.clamp(1, 40_000) as u32,
            });
        });
        let sh = shared.clone();
        engine.register_fn("spi_xfer", move |cs: i64, data: rhai::Array| {
            sh.fx.borrow_mut().actions.push(Action::SpiTransfer {
                cs: cs.clamp(0, 63) as u8,
                data: bytes_of(&data),
            });
        });
        let sh = shared.clone();
        engine.register_fn("i2c_setup", move |sda: i64, scl: i64, khz: i64| {
            let p = |v: i64| v.clamp(0, 63) as u8;
            sh.fx.borrow_mut().actions.push(Action::I2cConfig {
                sda: p(sda),
                scl: p(scl),
                freq_khz: khz.clamp(1, 1_000) as u32,
            });
        });
        let sh = shared.clone();
        engine.register_fn("i2c_write", move |addr: i64, data: rhai::Array| {
            sh.fx.borrow_mut().actions.push(Action::I2cWrite {
                addr: addr.clamp(0, 127) as u8,
                data: bytes_of(&data),
            });
        });
        let sh = shared.clone();
        engine.register_fn("i2c_read", move |addr: i64, reg: i64, len: i64| {
            sh.fx.borrow_mut().actions.push(Action::I2cRead {
                addr: addr.clamp(0, 127) as u8,
                reg: if (0..=255).contains(&reg) { reg as u16 } else { 256 },
                len: len.clamp(1, 48) as u8,
            });
        });
        let sh = shared.clone();
        engine.register_fn("lcd_init", move |sck: i64, mosi: i64, cs: i64, dc: i64, rst: i64| {
            let p = |v: i64| v.clamp(0, 63) as u8;
            sh.fx.borrow_mut().actions.push(Action::LcdInit {
                sck: p(sck),
                mosi: p(mosi),
                cs: p(cs),
                dc: p(dc),
                rst: p(rst),
                bl: 255,
            });
        });
        let sh = shared.clone();
        engine.register_fn("lcd_clear", move |r: i64, g: i64, b: i64| {
            let c = |v: i64| v.clamp(0, 255) as u8;
            sh.fx.borrow_mut().actions.push(Action::LcdClear { rgb: [c(r), c(g), c(b)] });
        });
        let sh = shared.clone();
        engine.register_fn(
            "lcd_rect",
            move |x: i64, y: i64, w: i64, h: i64, r: i64, g: i64, b: i64| {
                let c = |v: i64| v.clamp(0, 255) as u8;
                let p = |v: i64| v.clamp(0, 127) as u8;
                sh.fx.borrow_mut().actions.push(Action::LcdRect {
                    x: p(x),
                    y: p(y),
                    w: v_dim(w),
                    h: v_dim(h),
                    rgb: [c(r), c(g), c(b)],
                });
            },
        );
        let sh = shared.clone();
        engine.register_fn(
            "lcd_text",
            move |x: i64, y: i64, text: &str, r: i64, g: i64, b: i64| {
                let c = |v: i64| v.clamp(0, 255) as u8;
                let p = |v: i64| v.clamp(0, 127) as u8;
                sh.fx.borrow_mut().actions.push(Action::LcdText {
                    x: p(x),
                    y: p(y),
                    rgb: [c(r), c(g), c(b)],
                    text: text.chars().take(32).collect(),
                });
            },
        );
        let sh = shared.clone();
        engine.register_fn("uart_send", move |bytes: rhai::Array| {
            let data: Vec<u8> = bytes
                .iter()
                .filter_map(|v| v.as_int().ok())
                .map(|i| i.clamp(0, 255) as u8)
                .collect();
            sh.fx.borrow_mut().actions.push(Action::UartWrite { data });
        });
        let sh = shared.clone();
        engine.register_fn("chip", move || -> String { sh.board.borrow().chip.clone() });
        let sh = shared.clone();
        engine.register_fn("board_has", move |what: &str| -> bool {
            let needle = what.to_lowercase();
            sh.board.borrow().specs_lc.iter().any(|s| s.contains(&needle))
        });
        let sh = shared.clone();
        engine.register_fn("rgb", move |r: i64, g: i64, b: i64| {
            match sh.board.borrow().rgb_gpio {
                Some(gpio) => sh.fx.borrow_mut().actions.push(Action::SetRgb {
                    gpio,
                    r: r.clamp(0, 255) as u8,
                    g: g.clamp(0, 255) as u8,
                    b: b.clamp(0, 255) as u8,
                }),
                None => sh
                    .fx
                    .borrow_mut()
                    .logs
                    .push("rgb(): this board has no addressable LED".into()),
            }
        });
        let sh = shared.clone();
        engine.register_fn("send_board", move |board: &str, text: &str| {
            sh.fx.borrow_mut().actions.push(Action::BoardMsg {
                to: board.to_string(),
                text: text.to_string(),
            });
        });
        let sh = shared.clone();
        engine.register_fn("after", move |ms: i64, f: FnPtr| {
            let comp = *sh.current.borrow();
            let mut timers = sh.timers.borrow_mut();
            if timers.iter().filter(|t| t.comp == comp).count() < MAX_TIMERS_PER_COMP {
                let due_ms = sh.world.borrow().now_ms + ms.max(0) as u64;
                timers.push(Timer { comp, due_ms, f });
            }
        });
        let sh = shared.clone();
        engine.register_fn("comp", move |name: &str| -> Result<CompHandle, Box<rhai::EvalAltResult>> {
            match sh.names.borrow().get(name) {
                Some(&id) => Ok(CompHandle { id, sh: sh.clone() }),
                None => Err(format!("no component named '{name}'").into()),
            }
        });

        // Bare `led1` resolves to a component handle; `me` is the script owner.
        let sh = shared.clone();
        #[allow(deprecated)] // rhai marks on_var "volatile", not deprecated.
        engine.on_var(move |name, _, _| {
            if name == "me" {
                let id = *sh.current.borrow();
                return Ok(Some(Dynamic::from(CompHandle { id, sh: sh.clone() })));
            }
            match sh.names.borrow().get(name) {
                Some(&id) => Ok(Some(Dynamic::from(CompHandle { id, sh: sh.clone() }))),
                None => Ok(None),
            }
        });

        ScriptHost {
            engine,
            shared,
            instances: HashMap::new(),
            names: HashMap::new(),
            errors: HashMap::new(),
            last_reading: HashMap::new(),
        }
    }

    /// Script name other components use to address `comp`.
    pub fn name_of(&self, comp: CompId) -> Option<&str> {
        self.names.get(&comp).map(String::as_str)
    }

    pub fn has_script(&self, comp: CompId) -> bool {
        self.instances.contains_key(&comp)
    }

    /// Components with a compiled script instance (the flow included, so
    /// broadcast events reach it).
    pub fn scripted(&self) -> Vec<CompId> {
        self.instances.keys().copied().collect()
    }

    /// Diff circuit scripts against compiled instances; returns freshly
    /// (re)compiled components so the caller can fire `on_start`.
    pub fn sync(&mut self, circuit: &Circuit, lib: &Library) -> Vec<CompId> {
        self.names = component_names(circuit, lib);
        self.names.insert(FLOW_ID, "flow".into());
        *self.shared.names.borrow_mut() = self
            .names
            .iter()
            .filter(|(id, _)| **id != FLOW_ID)
            .map(|(id, n)| (n.clone(), *id))
            .collect();

        let live: HashSet<CompId> = circuit
            .components
            .values()
            .filter(|c| c.script.is_some())
            .map(|c| c.id)
            .collect();
        self.instances.retain(|id, _| live.contains(id) || *id == FLOW_ID);
        self.shared
            .timers
            .borrow_mut()
            .retain(|t| live.contains(&t.comp) || t.comp == FLOW_ID);
        self.errors.retain(|id, _| live.contains(id) || *id == FLOW_ID);

        let mut fresh = Vec::new();
        for comp in circuit.components.values() {
            let Some(src) = &comp.script else { continue };
            if self.instances.get(&comp.id).is_some_and(|i| &i.src == src) {
                continue;
            }
            match self.compile(comp.id, src) {
                Ok(inst) => {
                    self.instances.insert(comp.id, inst);
                    self.shared.timers.borrow_mut().retain(|t| t.comp != comp.id);
                    self.errors.remove(&comp.id);
                    fresh.push(comp.id);
                }
                Err(e) => {
                    self.instances.remove(&comp.id);
                    self.errors.insert(comp.id, e);
                }
            }
        }
        fresh
    }

    /// Snapshot of the flow instance's `this` map — node output values keyed
    /// `n<node>_<output>` — formatted for on-wire value overlays.
    pub fn flow_state(&self) -> Vec<(String, String)> {
        let Some(inst) = self.instances.get(&FLOW_ID) else { return Vec::new() };
        let Some(map) = inst.state.read_lock::<rhai::Map>() else { return Vec::new() };
        map.iter().map(|(k, v)| (k.to_string(), fmt_dynamic(v))).collect()
    }

    /// Install (or replace) the flow-graph script; returns true on a fresh
    /// compile so the caller can fire `on_start`.
    pub fn set_flow_script(&mut self, src: Option<&str>) -> bool {
        let Some(src) = src else {
            self.instances.remove(&FLOW_ID);
            self.errors.remove(&FLOW_ID);
            self.shared.timers.borrow_mut().retain(|t| t.comp != FLOW_ID);
            return false;
        };
        if self.instances.get(&FLOW_ID).is_some_and(|i| i.src == src) {
            return false;
        }
        match self.compile(FLOW_ID, src) {
            Ok(inst) => {
                self.instances.insert(FLOW_ID, inst);
                self.shared.timers.borrow_mut().retain(|t| t.comp != FLOW_ID);
                self.errors.remove(&FLOW_ID);
                true
            }
            Err(e) => {
                self.instances.remove(&FLOW_ID);
                self.errors.insert(FLOW_ID, e);
                false
            }
        }
    }

    fn compile(&mut self, comp: CompId, src: &str) -> Result<Instance, String> {
        let ast = self.engine.compile(src).map_err(|e| e.to_string())?;
        let handlers: Vec<(String, usize)> = ast
            .iter_functions()
            .map(|f| (f.name.to_string(), f.params.len()))
            .collect();
        // Top-level statements run once at (re)load.
        *self.shared.current.borrow_mut() = comp;
        let mut scope = Scope::new();
        self.engine
            .run_ast_with_scope(&mut scope, &ast)
            .map_err(|e| e.to_string())?;
        let now = self.shared.world.borrow().now_ms;
        Ok(Instance {
            src: src.to_string(),
            ast,
            state: Dynamic::from(rhai::Map::new()),
            handlers,
            last_tick_ms: now,
        })
    }

    /// Update the live snapshot scripts read from.
    pub fn set_world(&mut self, world: World) {
        *self.shared.world.borrow_mut() = world;
    }

    /// Board identity, capability lines and RGB LED pin.
    pub fn set_board(&mut self, chip: &str, specs: &[String], rgb_gpio: Option<u8>) {
        *self.shared.board.borrow_mut() = BoardInfo {
            chip: chip.to_string(),
            specs_lc: specs.iter().map(|s| s.to_lowercase()).collect(),
            rgb_gpio,
        };
    }

    fn call(&mut self, comp: CompId, name: &str, args: Vec<Dynamic>) -> Vec<Action> {
        let Some(inst) = self.instances.get_mut(&comp) else { return Vec::new() };
        if !inst.handlers.iter().any(|(n, a)| n == name && *a == args.len()) {
            return Vec::new();
        }
        *self.shared.current.borrow_mut() = comp;
        let opts = CallFnOptions::new().eval_ast(false).bind_this_ptr(&mut inst.state);
        let mut scope = Scope::new();
        let result = self
            .engine
            .call_fn_with_options::<Dynamic>(opts, &mut scope, &inst.ast, name, args);
        match result {
            Ok(_) => {
                self.errors.remove(&comp);
            }
            Err(e) => {
                let who = self.names.get(&comp).cloned().unwrap_or_else(|| format!("#{}", comp.0));
                let msg = format!("{name}: {e}");
                self.shared.fx.borrow_mut().logs.push(format!("[{who}] error in {msg}"));
                self.errors.insert(comp, msg);
            }
        }
        self.drain_actions()
    }

    fn drain_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.shared.fx.borrow_mut().actions)
    }

    /// Console lines produced by `log()` calls and runtime errors.
    pub fn take_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.shared.fx.borrow_mut().logs)
    }

    pub fn on_start(&mut self, comp: CompId) -> Vec<Action> {
        self.call(comp, "on_start", Vec::new())
    }

    pub fn on_press(&mut self, comp: CompId) -> Vec<Action> {
        let mut out = self.call(comp, "on_press", Vec::new());
        out.extend(self.flow_call("on_any_press", comp, Vec::new()));
        out
    }

    pub fn on_release(&mut self, comp: CompId) -> Vec<Action> {
        let mut out = self.call(comp, "on_release", Vec::new());
        out.extend(self.flow_call("on_any_release", comp, Vec::new()));
        out
    }

    pub fn on_change(&mut self, comp: CompId, on: bool) -> Vec<Action> {
        let mut out = self.call(comp, "on_change", vec![Dynamic::from(on)]);
        out.extend(self.flow_call("on_any_change", comp, vec![Dynamic::from(on)]));
        out
    }

    /// Forward a component event to the flow script with the source's name.
    fn flow_call(&mut self, name: &str, comp: CompId, mut args: Vec<Dynamic>) -> Vec<Action> {
        if comp == FLOW_ID || !self.instances.contains_key(&FLOW_ID) {
            return Vec::new();
        }
        let Some(who) = self.names.get(&comp).cloned() else { return Vec::new() };
        args.insert(0, Dynamic::from(who));
        self.call(FLOW_ID, name, args)
    }

    /// Raw pin edge, dispatched to every scripted component.
    pub fn on_pin(&mut self, comp: CompId, gpio: u8, high: bool) -> Vec<Action> {
        self.call(comp, "on_pin", vec![Dynamic::from(i64::from(gpio)), Dynamic::from(high)])
    }

    /// A message sent by another board tab via `send_board`.
    pub fn on_board_msg(&mut self, comp: CompId, from: &str, text: &str) -> Vec<Action> {
        self.call(
            comp,
            "on_board_msg",
            vec![Dynamic::from(from.to_string()), Dynamic::from(text.to_string())],
        )
    }

    /// A complete line arrived on UART1.
    pub fn on_uart(&mut self, comp: CompId, line: &str) -> Vec<Action> {
        self.call(comp, "on_uart", vec![Dynamic::from(line.to_string())])
    }

    /// SPI transfer completed; `data` is what the peripheral clocked back.
    pub fn on_spi(&mut self, comp: CompId, data: &[u8]) -> Vec<Action> {
        let arr: rhai::Array = data.iter().map(|b| Dynamic::from(i64::from(*b))).collect();
        self.call(comp, "on_spi", vec![Dynamic::from(arr)])
    }

    /// I2C read completed.
    pub fn on_i2c(&mut self, comp: CompId, addr: u8, data: &[u8]) -> Vec<Action> {
        let arr: rhai::Array = data.iter().map(|b| Dynamic::from(i64::from(*b))).collect();
        self.call(comp, "on_i2c", vec![Dynamic::from(i64::from(addr)), Dynamic::from(arr)])
    }

    /// Analog sample; dispatches `on_reading` when it moved enough to matter.
    pub fn on_reading(&mut self, comp: CompId, mv: u16) -> Vec<Action> {
        let significant = match self.last_reading.get(&comp) {
            Some(&prev) => prev.abs_diff(mv) >= 8,
            None => true,
        };
        if !significant {
            return Vec::new();
        }
        self.last_reading.insert(comp, mv);
        let mut out = self.call(comp, "on_reading", vec![Dynamic::from(i64::from(mv))]);
        out.extend(self.flow_call("on_any_reading", comp, vec![Dynamic::from(i64::from(mv))]));
        out
    }

    /// Fire due `after` timers and `on_tick` handlers.
    pub fn tick(&mut self, now_ms: u64) -> Vec<Action> {
        self.shared.world.borrow_mut().now_ms = now_ms;
        let mut out = Vec::new();

        let due: Vec<Timer> = {
            let mut timers = self.shared.timers.borrow_mut();
            let mut due = Vec::new();
            timers.retain_mut(|t| {
                if t.due_ms <= now_ms {
                    due.push(Timer { comp: t.comp, due_ms: t.due_ms, f: t.f.clone() });
                    false
                } else {
                    true
                }
            });
            due
        };
        for t in due {
            let Some(inst) = self.instances.get(&t.comp) else { continue };
            *self.shared.current.borrow_mut() = t.comp;
            if let Err(e) = t.f.call::<Dynamic>(&self.engine, &inst.ast, ()) {
                let who = self.names.get(&t.comp).cloned().unwrap_or_else(|| format!("#{}", t.comp.0));
                self.shared.fx.borrow_mut().logs.push(format!("[{who}] error in timer: {e}"));
                self.errors.insert(t.comp, format!("timer: {e}"));
            }
            out.extend(self.drain_actions());
        }

        let tickers: Vec<(CompId, u64)> = self
            .instances
            .iter()
            .filter(|(_, i)| i.handlers.iter().any(|(n, a)| n == "on_tick" && *a == 1))
            .map(|(id, i)| (*id, i.last_tick_ms))
            .collect();
        for (comp, last) in tickers {
            let dt = now_ms.saturating_sub(last);
            if dt == 0 {
                continue;
            }
            if let Some(inst) = self.instances.get_mut(&comp) {
                inst.last_tick_ms = now_ms;
            }
            out.extend(self.call(comp, "on_tick", vec![Dynamic::from(dt as i64)]));
        }
        out
    }
}
