//! Built-in reference for component scripts: WireLab API + Rhai language.

use egui::RichText;
use egui_extras::syntax_highlighting::{CodeTheme, code_view_ui};

/// (title, body) sections; code blocks are fenced with ``` lines.
const SECTIONS: &[(&str, &str)] = &[
    (
        "Multi-board projects & cross-board messages",
        "A project can hold several boards — the chip strip under the toolbar \
         switches between them (➕ board adds one; double-click renames). Every \
         connected board keeps running when you switch away: its dot stays \
         green and its scripts stay live.\n\
         Boards talk to each other by name:\n\
         ```\n\
         // on the board named \"house\":\n\
         fn on_press() { send_board(\"garage\", \"open\"); }\n\
         \n\
         // on the board named \"garage\":\n\
         fn on_board_msg(from, text) {\n\
             if text == \"open\" { servo.set_angle(90); }\n\
         }\n\
         ```\n\
         Messages route host-side (through WireLab), so they work across any \
         mix of simulator, USB and Wi-Fi sessions. Both boards must be \
         connected; the console reports drops. The name is the tab's name, \
         case-insensitive; send_board(\"*\", ...) broadcasts to every other \
         live board. The Flow tab has matching nodes: 'on board message', \
         'text equals' and 'send to board'.",
    ),
    (
        "Flow graphs (no-code scripts)",
        "The IDE's Flow tab is a node-graph editor: wire EVENT nodes (press, \
         level, analog reading, uart line, every-N-ms) through LOGIC nodes \
         (compare, threshold, toggle, gate, delay, counter, map-range) into \
         ACTION nodes (set/toggle a component, pwm, rgb, uart, lcd, log).\n\
         Right-click the canvas to add nodes; drag pins to connect. Pin colors \
         are types: orange ▶ pulse, green ● bool, blue ● number, purple ● text.\n\
         The graph compiles to a normal Rhai script that runs exactly like \
         hand-written ones (see it via the </> code button). A `script` node \
         embeds a Rhai expression over inputs a, b, c when the built-in \
         nodes aren't enough. Flows and per-component scripts run side by side.\n\
         Try the shipped example: 10-flow-blink.",
    ),
    (
        "How scripts run",
        "Every placed component can carry one script — attach it from the Script tab \
         or the inspector. Scripts are live whenever a device (simulator or real board) \
         is connected; the rules program's Run/Stop does not affect them.\n\
         Hit ▶ Apply (Ctrl+Enter) to hot-swap: the script recompiles instantly, \
         no reflash, and `on_start` fires again.\n\
         Callbacks WireLab invokes on the owning component:\n\
         • on_start()        — after connect / after Apply\n\
         • on_press() / on_release()  — push buttons\n\
         • on_change(on)     — any input: buttons, switches, digital sensors (bool)\n\
         • on_reading(mv)    — analog parts, on each meaningful new sample (int, millivolts)\n\
         • on_tick(dt_ms)    — every frame while connected (int, elapsed ms)\n\
         • on_pin(gpio, high) — raw pin edge anywhere, e.g. the BOOT button\n\
         The Examples menu in the toolbar ships board-only starters showing \
         all of this in action.",
    ),
    (
        "Driving components",
        "Other components are addressed by their sanitized label — `Red LED!` becomes \
         `red_led` (the inspector and Script tab header show each part's name). \
         `me` is the component the script is attached to; `comp(\"name\")` looks one up \
         dynamically.\n```\nfn on_press() {\n    red_led.on();\n    red_led.off();\n    red_led.toggle();\n    red_led.blink(250);      // firmware-side, keeps running on its own\n    red_led.breathe(2000);   // ditto\n    red_led.dim(35);         // percent, PWM\n    servo.set_angle(120);    // degrees 0..180\n    buzzer.beep(200);        // ms\n    buzzer.tone(880, 300);   // Hz, ms\n}\n```\nVerbs map to the same engine actions the Program rules use, so anything that \
         works in a rule works from a script.",
    ),
    (
        "Reading state",
        "```\nfn on_tick(dt_ms) {\n    if btn.is_pressed() { }      // logical: true while held\n    if red_led.is_on() { }       // commanded output state\n    let mv = pot.millivolts();   // last analog sample, int\n    if pin(4).is_high() { }      // raw GPIO level from telemetry\n}\n```\nReads come from the latest telemetry snapshot (50 ms cadence), not a \
         round-trip — cheap to call every tick.",
    ),
    (
        "Raw pins, PWM & the RGB LED",
        "```\npin(2).high();\npin(2).low();\npin(2).set(true);\npin(2).toggle();\npin(8).pwm(1000, 500);       // freq Hz, duty in permille (0..1000)\npin(28).input_pullup();      // reconfigure, e.g. to watch BOOT\npin(28).is_high();\n\nrgb(255, 40, 0);             // the board's WS2812, real color via RMT\n```\nMode changes (`input_pullup`, `input_pulldown`, `input`, `output`) let a \
         script watch pins the wiring didn't configure — the BOOT button being \
         the classic case: configure it in `on_start`, react in `on_pin`.",
    ),
    (
        "Timers, time & logging",
        "```\nfn on_press() {\n    after(500, || red_led.off());     // run a closure later (ms)\n    let t = millis();                 // session clock, int ms\n    log(`held at ${t}`);              // -> Console tab, prefixed [name]\n}\n```\n`after` timers belong to the component; recompiling its script cancels them. \
         Up to 64 pending timers per component. Note: `this` is not available \
         inside an `after` closure — capture what you need into a local first:\n```\nfn on_press() {\n    let n = this.count ?? 0;\n    after(300, || log(n));\n}\n```",
    ),
    (
        "Who is who: me, this, and names",
        "Three different things, easy to mix up:\n\
         • `me` — the component this script is attached to. A button script \
         reads itself with `me.is_pressed()`; an LED script drives itself with \
         `me.on()`.\n\
         • bare names — every OTHER component, addressed by its script name \
         (shown in the header and the inspector): `red_led.toggle()`.\n\
         • `this` — NOT the component. It is your script's private state map:\n```\nfn on_press() {\n    this.count = (this.count ?? 0) + 1;   // ?? gives a default when unset\n    if this.count > 3 { this.count = 0; }\n    log(`count ${this.count}, held: ${me.is_pressed()}`);\n}\n```\nState survives between events but resets when the script is re-applied. \
         One caveat: `this` is unavailable inside `after(ms, || ...)` closures — \
         capture what you need into a `let` first.",
    ),
    (
        "Board info",
        "The connected board's identity and capabilities are queryable:\n```\nfn on_start() {\n    log(chip());                     // \"ESP32-C5\"\n    if board_has(\"wifi\") { }         // matches the board's spec lines\n    if board_has(\"zigbee\") { }\n    if board_has(\"5 ghz\") { }\n}\n```\n`board_has` does a case-insensitive substring match over the board \
         profile's spec list (see the palette's \"capabilities\" section). \
         Radio control (Wi-Fi/BLE/802.15.4) from scripts needs firmware-side \
         support and is not available yet — today this is for feature \
         detection so one script can adapt to different boards.",
    ),
    (
        "Rhai: variables & types",
        "```\nlet x = 42;            // int (i64)\nlet y = 1.5;           // float (f64)\nlet s = \"text\";        // string\nlet ok = true;         // bool\nlet a = [1, 2, 3];     // array\nlet m = #{ a: 1 };     // object map\nconst LIMIT = 2000;    // constant\n```\nIntegers and floats do not mix implicitly: `1 + 1.5` is an error — write \
         `1.0 + 1.5` or `x.to_float()`. Missing map properties read as `()` \
         (unit), which is what `??` tests for.",
    ),
    (
        "Rhai: strings",
        "```\nlet name = \"world\";\nlet s = `hello ${name}, 2 + 2 = ${2 + 2}`;   // backtick interpolation\nlet n = s.len;\nlet up = s.to_upper();\nif s.contains(\"hello\") { }\n```",
    ),
    (
        "Rhai: control flow",
        "```\nif mv > 2000 {\n    // ...\n} else if mv > 1000 {\n} else {\n}\n\nlet level = if on { \"high\" } else { \"low\" };   // if is an expression\n\nswitch state {\n    0 => log(\"idle\"),\n    1 | 2 => log(\"busy\"),\n    _ => log(\"other\"),\n}\n\nfor i in 0..5 { log(i); }\nfor item in [10, 20, 30] { }\nwhile x < 10 { x += 1; }\nloop { break; }\n```",
    ),
    (
        "Rhai: functions & closures",
        "```\nfn scaled(mv, max) {\n    mv * 100 / max        // last expression is the return value\n}\n\nfn on_reading(mv) {\n    let pct = scaled(mv, 3300);\n    let f = |x| x * 2;    // closure; captures by sharing\n    log(f(pct));\n}\n```\nScript functions are pure: they see only their arguments and `this`. \
         Arguments pass by value.",
    ),
    (
        "Rhai: arrays & maps",
        "```\nlet a = [1, 2, 3];\na.push(4);\nlet n = a.len;\nlet doubled = a.map(|x| x * 2);\nlet big = a.filter(|x| x > 2);\nlet total = a.reduce(|sum, x| sum + x, 0);\n\nlet m = #{ name: \"led\", pin: 2 };\nm.pin = 4;\nif \"name\" in m { }\n```",
    ),
    (
        "Rhai: operators & errors",
        "```\nlet v = maybe ?? 0;       // default when () / missing\nlet l = obj?.len;         // safe access\nx += 1; x *= 2;           // compound assignment\n1 == 1.0;                 // false! types differ\n\ntry {\n    throw \"boom\";\n} catch (e) {\n    log(e);\n}\n```",
    ),
    (
        "Limits & safety",
        "Each callback run is capped at 200 000 operations — an accidental \
         `loop {}` aborts with an error instead of freezing WireLab. `eval` is \
         disabled. Compile and runtime errors show up in the Script tab header, \
         the inspector badge, and the Console (prefixed with the component name). \
         Errors clear on the next successful run.",
    ),
];

/// The whole reference as plain text (served over MCP).
pub fn reference_text() -> String {
    SECTIONS
        .iter()
        .map(|(title, body)| format!("# {title}\n{}\n", body.replace("```", "")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Floating reference window, filterable.
pub fn show_docs_window(ctx: &egui::Context, open: &mut bool, filter: &mut String) {
    show_reference_window(ctx, "📖 Script reference", SECTIONS, open, filter);
}

/// Shared renderer for filterable, sectioned reference windows.
pub fn show_reference_window(
    ctx: &egui::Context,
    title: &str,
    sections: &[(&str, &str)],
    open: &mut bool,
    filter: &mut String,
) {
    if !*open {
        return;
    }
    let mut still_open = *open;
    egui::Window::new(title)
        .open(&mut still_open)
        .default_width(560.0)
        .default_height(520.0)
        .vscroll(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("filter");
                ui.add(egui::TextEdit::singleline(filter).desired_width(220.0));
                if !filter.is_empty() && ui.small_button("✖").clicked() {
                    filter.clear();
                }
            });
            ui.separator();
            let theme = CodeTheme::from_memory(ui.ctx(), ui.style());
            let needle = filter.to_lowercase();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (i, (title, body)) in sections.iter().enumerate() {
                    if !needle.is_empty()
                        && !title.to_lowercase().contains(&needle)
                        && !body.to_lowercase().contains(&needle)
                    {
                        continue;
                    }
                    egui::CollapsingHeader::new(RichText::new(*title).strong())
                        .default_open(i == 0 || !needle.is_empty())
                        .show(ui, |ui| {
                            for (j, chunk) in body.split("```").enumerate() {
                                if chunk.trim().is_empty() {
                                    continue;
                                }
                                if j % 2 == 0 {
                                    ui.label(chunk.trim_matches('\n'));
                                } else {
                                    code_view_ui(ui, &theme, chunk.trim_matches('\n'), "rs");
                                }
                            }
                        });
                }
            });
        });
    *open = still_open;
}
