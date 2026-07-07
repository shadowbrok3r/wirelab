//! Rhai host-API metadata plus wasm-safe completion and hover, shared by the
//! desktop editor and the iPad plugin. Pure std: no egui, rowan, hir, or url.

/// One hoverable / completable API entry.
#[derive(Clone, Copy)]
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
    ApiDoc { name: "http_get", sig: "http_get(url)", doc: "Fetch a URL over the host's network (GET); the reply lands in on_http(status, body). Runs on the computer, not the chip.", member: false, args: true },
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
    ("on_http", "An http_get finished: (status, body). Status 0 means the request failed and body holds the error."),
];

/// One completion candidate.
#[derive(Clone)]
pub struct CompletionItem {
    pub label: String,
    pub insert: String,
    pub back: usize,
    pub detail: String,
}

/// A completion result anchored at the start of the word being completed.
#[derive(Clone)]
pub struct Completions {
    pub word_start: usize,
    pub items: Vec<CompletionItem>,
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Completion candidates at `cursor_char` (a char index) in `buf`.
pub fn completions(buf: &str, cursor_char: usize, comp_names: &[String]) -> Option<Completions> {
    let chars: Vec<char> = buf.chars().collect();
    let ci = cursor_char.min(chars.len());
    let mut ws = ci;
    while ws > 0 && is_ident(chars[ws - 1]) {
        ws -= 1;
    }
    let prefix: String = chars[ws..ci].iter().collect();
    let member = ws > 0 && chars[ws - 1] == '.';
    if !member && prefix.is_empty() {
        return None;
    }

    let mut items: Vec<CompletionItem> = Vec::new();
    for d in API_DOCS {
        if d.member != member || d.name == "me" || !d.name.starts_with(&prefix) {
            continue;
        }
        let (insert, back) = if d.args {
            (format!("{}()", d.name), 1)
        } else if d.member || d.sig.contains("()") {
            (format!("{}()", d.name), 0)
        } else {
            (d.name.to_string(), 0)
        };
        items.push(CompletionItem {
            label: d.name.to_string(),
            insert,
            back,
            detail: format!("{} — {}", d.sig, d.doc),
        });
    }
    if !member {
        if "me".starts_with(&prefix) && !prefix.is_empty() {
            items.push(CompletionItem {
                label: "me".into(),
                insert: "me".into(),
                back: 0,
                detail: "this component".into(),
            });
        }
        let mut names: Vec<&String> = comp_names.iter().collect();
        names.sort();
        for n in names {
            if n.starts_with(&prefix) {
                items.push(CompletionItem {
                    label: n.clone(),
                    insert: n.clone(),
                    back: 0,
                    detail: "component".into(),
                });
            }
        }
    }

    let exact_only =
        items.len() == 1 && (items[0].label == prefix || items[0].insert == prefix);
    if items.is_empty() || exact_only {
        return None;
    }
    items.truncate(10);
    Some(Completions { word_start: ws, items })
}

/// Hover text for `word`, treated as a member method when `member`.
pub fn hover(word: &str, member: bool) -> Option<String> {
    if let Some(d) = API_DOCS.iter().find(|d| d.name == word && d.member == member) {
        return Some(format!("{} — {}", d.sig, d.doc));
    }
    CALLBACK_DOCS
        .iter()
        .find(|(n, _)| *n == word)
        .map(|(_, doc)| (*doc).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_globals_by_prefix() {
        // Completion draws from API_DOCS globals, not callbacks.
        let c = completions("if board_", 9, &[]).expect("some");
        assert!(c.items.iter().any(|i| i.label == "board_has"));
        assert_eq!(c.items[0].insert, "board_has()");
        assert_eq!(c.items[0].back, 1);
    }

    #[test]
    fn suggests_member_after_dot() {
        let c = completions("led.tog", 7, &[]).expect("some");
        assert!(c.items.iter().any(|i| i.label == "toggle"));
        assert!(c.items.iter().all(|i| i.detail != "component"));
    }

    #[test]
    fn suggests_component_name() {
        let c = completions("red_l", 5, &["red_led".into()]).expect("some");
        assert!(c.items.iter().any(|i| i.label == "red_led" && i.detail == "component"));
    }

    #[test]
    fn exact_match_returns_none() {
        assert!(completions("log", 3, &[]).is_none());
    }

    #[test]
    fn hover_callback() {
        assert_eq!(
            hover("on_reading", false).as_deref(),
            Some("New analog sample; argument is millivolts."),
        );
    }

    #[test]
    fn hover_member() {
        assert_eq!(
            hover("toggle", true).as_deref(),
            Some(".toggle() — Invert the output's current state."),
        );
    }
}
