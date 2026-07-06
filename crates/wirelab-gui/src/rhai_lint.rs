//! Live script diagnostics powered by the vendored rhai-lsp crates (`lsp/`):
//! rhai-rowan for syntax, rhai-hir for semantic checks. The WireLab host API
//! and component names are declared through a generated Rhai definition
//! module so references to them resolve.

use rhai_hir::Hir;
use rhai_hir::error::ErrorKind;
use rhai_rowan::parser::Parser;
use url::Url;

/// One hoverable / completable API entry.
pub struct ApiDoc {
    pub name: &'static str,
    pub sig: &'static str,
    pub doc: &'static str,
    /// Method on a component/pin handle (offered after `.`).
    pub member: bool,
    /// Takes arguments: completion leaves the cursor between the parens.
    pub args: bool,
}

pub const API_DOCS: &[ApiDoc] = &[
    ApiDoc { name: "log", sig: "log(data)", doc: "Print to the Console tab, prefixed with this component's name.", member: false, args: true },
    ApiDoc { name: "millis", sig: "millis() -> int", doc: "Milliseconds since the session started.", member: false, args: false },
    ApiDoc { name: "after", sig: "after(ms, || ...)", doc: "Run a closure later. Belongs to this component; recompiling cancels it. `this` is unavailable inside — capture locals first.", member: false, args: true },
    ApiDoc { name: "pin", sig: "pin(gpio) -> Pin", doc: "Raw GPIO handle: .high() .low() .set(b) .toggle() .pwm(hz, permille) .is_high() .input_pullup() .input_pulldown() .input() .output()", member: false, args: true },
    ApiDoc { name: "comp", sig: "comp(name) -> Component", doc: "Look a component up by its script name; errors when missing.", member: false, args: true },
    ApiDoc { name: "chip", sig: "chip() -> string", doc: "The connected board's chip name, e.g. \"ESP32-C5\".", member: false, args: false },
    ApiDoc { name: "board_has", sig: "board_has(what) -> bool", doc: "Case-insensitive substring match over the board's capability lines.", member: false, args: true },
    ApiDoc { name: "uart", sig: "uart(tx, rx, baud)", doc: "Claim UART1 on any free pins (baud 0 releases it). Lines arrive via on_uart(line); the simulator echoes writes back.", member: false, args: true },
    ApiDoc { name: "uart_send", sig: "uart_send(text | [bytes])", doc: "Transmit on UART1.", member: false, args: true },
    ApiDoc { name: "send_board", sig: "send_board(board, text)", doc: "Send text to another board tab's scripts; they receive it in on_board_msg(from, text). Both boards must be connected.", member: false, args: true },
    ApiDoc { name: "spi_setup", sig: "spi_setup(sck, mosi, miso, freq_khz)", doc: "Generic SPI bus on SPI2 (replaces the LCD if configured). CS pins are plain GPIOs per transfer.", member: false, args: true },
    ApiDoc { name: "spi_xfer", sig: "spi_xfer(cs, [bytes])", doc: "Full-duplex transfer; the clocked-back bytes arrive in on_spi([bytes]). Sim echoes the written bytes.", member: false, args: true },
    ApiDoc { name: "i2c_setup", sig: "i2c_setup(sda, scl, freq_khz)", doc: "I2C master on any pins (typ. 100 or 400 kHz).", member: false, args: true },
    ApiDoc { name: "i2c_write", sig: "i2c_write(addr, [bytes])", doc: "Write bytes to a 7-bit address.", member: false, args: true },
    ApiDoc { name: "i2c_read", sig: "i2c_read(addr, reg, len)", doc: "Read len bytes (optionally after selecting reg; pass 256 for none). Data arrives in on_i2c(addr, [bytes]).", member: false, args: true },
    ApiDoc { name: "lcd_init", sig: "lcd_init(sck, mosi, cs, dc, rst)", doc: "Bring up an ST7735 SPI display (128x128). The simulator renders it on the component.", member: false, args: true },
    ApiDoc { name: "lcd_clear", sig: "lcd_clear(r, g, b)", doc: "Fill the whole screen.", member: false, args: true },
    ApiDoc { name: "lcd_rect", sig: "lcd_rect(x, y, w, h, r, g, b)", doc: "Fill a rectangle; repaint regions instead of clearing for smooth updates.", member: false, args: true },
    ApiDoc { name: "lcd_text", sig: "lcd_text(x, y, text, r, g, b)", doc: "Draw 6x10 text at a pixel position.", member: false, args: true },
    ApiDoc { name: "rgb", sig: "rgb(r, g, b)", doc: "Drive the board's addressable RGB LED (0..255 each). Real color on hardware via the RMT driver; colored marker in the simulator.", member: false, args: true },
    ApiDoc { name: "me", sig: "me", doc: "Handle to the component this script is attached to.", member: false, args: false },
    ApiDoc { name: "on", sig: ".on()", doc: "Switch the output on (polarity-aware).", member: true, args: false },
    ApiDoc { name: "off", sig: ".off()", doc: "Switch the output off.", member: true, args: false },
    ApiDoc { name: "toggle", sig: ".toggle()", doc: "Invert the output's current state.", member: true, args: false },
    ApiDoc { name: "blink", sig: ".blink(period_ms)", doc: "Firmware-side blink; keeps running with zero round-trips.", member: true, args: true },
    ApiDoc { name: "breathe", sig: ".breathe(period_ms)", doc: "Firmware-side sine fade.", member: true, args: true },
    ApiDoc { name: "dim", sig: ".dim(percent)", doc: "PWM brightness, 0..100.", member: true, args: true },
    ApiDoc { name: "set_angle", sig: ".set_angle(degrees)", doc: "Servo position, 0..180.", member: true, args: true },
    ApiDoc { name: "beep", sig: ".beep(ms)", doc: "Buzzer on, then off after `ms`.", member: true, args: true },
    ApiDoc { name: "tone", sig: ".tone(hz, ms)", doc: "PWM tone at `hz` for `ms`.", member: true, args: true },
    ApiDoc { name: "act", sig: ".act(verb)", doc: "Run any component verb by name.", member: true, args: true },
    ApiDoc { name: "is_on", sig: ".is_on() -> bool", doc: "Commanded output state, polarity-corrected.", member: true, args: false },
    ApiDoc { name: "is_pressed", sig: ".is_pressed() -> bool", doc: "Logical input state from the latest telemetry.", member: true, args: false },
    ApiDoc { name: "millivolts", sig: ".millivolts() -> int", doc: "Last analog sample for this component.", member: true, args: false },
    ApiDoc { name: "high", sig: ".high()", doc: "Drive the pin high.", member: true, args: false },
    ApiDoc { name: "low", sig: ".low()", doc: "Drive the pin low.", member: true, args: false },
    ApiDoc { name: "set", sig: ".set(high)", doc: "Drive the pin to a level.", member: true, args: true },
    ApiDoc { name: "pwm", sig: ".pwm(hz, permille)", doc: "PWM output; duty is 0..1000.", member: true, args: true },
    ApiDoc { name: "is_high", sig: ".is_high() -> bool", doc: "Raw level from telemetry.", member: true, args: false },
    ApiDoc { name: "watch_analog", sig: ".watch_analog(interval_ms)", doc: "Turn the pin into a sampled ADC input; read with .millivolts(). Basis of the ohmmeter example.", member: true, args: true },
    ApiDoc { name: "input_pullup", sig: ".input_pullup()", doc: "Reconfigure as input with pull-up (e.g. the BOOT button).", member: true, args: false },
    ApiDoc { name: "input_pulldown", sig: ".input_pulldown()", doc: "Reconfigure as input with pull-down.", member: true, args: false },
    ApiDoc { name: "input", sig: ".input()", doc: "Reconfigure as floating input.", member: true, args: false },
    ApiDoc { name: "output", sig: ".output()", doc: "Reconfigure as push-pull output.", member: true, args: false },
];

/// Callback names WireLab invokes, for hover docs.
pub const CALLBACK_DOCS: &[(&str, &str)] = &[
    ("on_start", "Runs after connect and after every Apply."),
    ("on_press", "Push button pressed (this component)."),
    ("on_release", "Push button released."),
    ("on_change", "Any input changed; argument is the logical state."),
    ("on_reading", "New analog sample; argument is millivolts."),
    ("on_tick", "Every frame while connected; argument is elapsed ms."),
    ("on_pin", "Raw pin edge anywhere on the board: (gpio, high)."),
    ("on_uart", "A complete line arrived on UART1."),
    ("on_spi", "An SPI transfer finished; argument is the [bytes] clocked back."),
    ("on_i2c", "An I2C read finished: (addr, [bytes])."),
    ("on_board_msg", "Text sent by another board tab via send_board: (from, text)."),
];

#[derive(Debug, Clone, PartialEq)]
pub struct LintDiag {
    /// Byte range into the script source.
    pub start: usize,
    pub end: usize,
    /// 1-based position of `start`.
    pub line: usize,
    pub col: usize,
    pub message: String,
}

pub struct Linter {
    hir: Hir,
    script_url: Url,
    defs_url: Url,
    defs_src: String,
}

impl Default for Linter {
    fn default() -> Self {
        Linter::new()
    }
}

impl Linter {
    pub fn new() -> Linter {
        Linter {
            hir: Hir::default(),
            script_url: Url::parse("wirelab:///script.rhai").expect("static url"),
            defs_url: Url::parse("wirelab:///api.d.rhai").expect("static url"),
            defs_src: String::new(),
        }
    }

    /// Declare the host API plus the current component names.
    pub fn set_api(&mut self, comp_names: &[String]) {
        let mut s = String::from(
            "module static;\n\
             fn log(data: ?);\n\
             fn millis() -> int;\n\
             fn after(ms: int, callback: ?);\n\
             fn pin(gpio: int) -> ?;\n\
             fn comp(name: String) -> ?;\n\
             fn chip() -> String;\n\
             fn board_has(what: String) -> bool;\n\
             fn rgb(r: int, g: int, b: int);\n\
             fn uart(tx: int, rx: int, baud: int);\n\
             fn uart_send(data: ?);\n\
             fn send_board(board: ?, text: ?);\n\
             fn spi_setup(sck: int, mosi: int, miso: int, freq_khz: int);\n\
             fn spi_xfer(cs: int, data: ?);\n\
             fn i2c_setup(sda: int, scl: int, freq_khz: int);\n\
             fn i2c_write(addr: int, data: ?);\n\
             fn i2c_read(addr: int, reg: int, len: int);\n\
             fn lcd_init(sck: int, mosi: int, cs: int, dc: int, rst: int);\n\
             fn lcd_clear(r: int, g: int, b: int);\n\
             fn lcd_rect(x: int, y: int, w: int, h: int, r: int, g: int, b: int);\n\
             fn lcd_text(x: int, y: int, text: String, r: int, g: int, b: int);\n\
             const me: ?;\n",
        );
        for name in comp_names {
            if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                s += &format!("const {name}: ?;\n");
            }
        }
        if s == self.defs_src {
            return;
        }
        self.defs_src = s;
        let parse = Parser::new(&self.defs_src).parse_def();
        self.hir.add_source(&self.defs_url, &parse.clone_syntax());
        self.hir.resolve_all();
    }

    /// All diagnostics for one script: syntax first, semantics when clean.
    pub fn lint(&mut self, src: &str) -> Vec<LintDiag> {
        let mut out = Vec::new();
        let parse = Parser::new(src).parse_script();
        for e in &parse.errors {
            let start = u32::from(e.range.start()) as usize;
            let end = u32::from(e.range.end()) as usize;
            out.push(diag_at(src, start, end, e.kind.to_string()));
        }
        if !out.is_empty() {
            return out;
        }

        self.hir.add_source(&self.script_url, &parse.clone_syntax());
        self.hir.resolve_all();
        let Some(source) = self.hir.source_by_url(&self.script_url) else { return out };
        for err in self.hir.errors_for_source(source) {
            let symbol = match &err.kind {
                ErrorKind::UnresolvedReference { reference_symbol, .. } => *reference_symbol,
                ErrorKind::DuplicateFnParameter { duplicate_symbol, .. } => *duplicate_symbol,
                ErrorKind::UnresolvedImport { import } => *import,
                ErrorKind::NestedFunction { function } => *function,
            };
            let range = self.hir[symbol].selection_or_text_range().unwrap_or_default();
            let start = u32::from(range.start()) as usize;
            let end = u32::from(range.end()) as usize;
            // `this` is bound by the WireLab runtime, not the script scope.
            if src.get(start..end) == Some("this") {
                continue;
            }
            out.push(diag_at(src, start, end, err.kind.to_string()));
        }
        out
    }
}

fn diag_at(src: &str, start: usize, end: usize, message: String) -> LintDiag {
    let clamped = start.min(src.len());
    let line = src[..clamped].bytes().filter(|&b| b == b'\n').count() + 1;
    let col = clamped - src[..clamped].rfind('\n').map(|i| i + 1).unwrap_or(0) + 1;
    LintDiag { start, end: end.max(start + 1), line, col, message }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linted(src: &str) -> Vec<LintDiag> {
        let mut l = Linter::new();
        l.set_api(&["led1".into(), "btn".into()]);
        l.lint(src)
    }

    #[test]
    fn clean_script_has_no_diags() {
        let d = linted(
            "fn on_press() {\n    this.n = (this.n ?? 0) + 1;\n    led1.toggle();\n    \
             after(100, || led1.off());\n    log(millis());\n}\n",
        );
        assert!(d.is_empty(), "unexpected: {d:?}");
    }

    #[test]
    fn syntax_error_is_reported_with_position() {
        let d = linted("fn on_press( {\n}\n");
        assert!(!d.is_empty());
        assert_eq!(d[0].line, 1);
    }

    #[test]
    fn unresolved_reference_is_reported() {
        let d = linted("fn on_press() {\n    nosuch.on();\n}\n");
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].message.contains("cannot resolve"), "{}", d[0].message);
        assert_eq!(d[0].line, 2);
    }

    #[test]
    fn api_and_component_names_resolve() {
        let d = linted(
            "fn on_reading(mv) {\n    if board_has(\"wifi\") { log(chip()); }\n    \
             pin(4).high();\n    btn.is_pressed();\n    me.on();\n    comp(\"led1\").off();\n    \
             rgb(255, 0, 128);\n    pin(28).input_pullup();\n}\n",
        );
        assert!(d.is_empty(), "unexpected: {d:?}");
    }
}
