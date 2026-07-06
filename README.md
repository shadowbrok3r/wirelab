# WireLab

A live circuit builder + IDE for ESP32 dev boards. Think Fritzing, except the
board is real: place components on a canvas, draw wires to GPIO pins, and the
attached ESP32 — running the WireLab runtime firmware — is reconfigured on the
fly. Press the on-screen button, the real pin fires. No hardware handy? The
built-in electrical simulator runs the identical protocol, so everything works
offline too.

```
┌───────────────┐   HostMsg / DeviceMsg (postcard + CRC16 + COBS over serial)
│  wirelab-gui  │ ─────────────────────────────────────────────┐
│  (eframe/egui)│                                              ▼
│   canvas ·    │   ┌──────────────┐  same trait   ┌───────────────────────┐
│   rules ·     │──▶│ wirelab-link │◀─────────────▶│ real ESP32 running    │
│   AI import   │   │  Session     │               │ wirelab-fw (esp-hal)  │
└───────┬───────┘   │  ┌─────────┐ │               └───────────────────────┘
        │           │  │SimDevice│ │  in-process simulator (nodal analysis)
        ▼           │  └─────────┘ │
┌───────────────┐   └──────────────┘
│ wirelab-core  │  board profiles · component library · netlist (union-find)
│               │  electrical solver · lints · rules engine · auto pin setup
└───────────────┘
```

## Crates

| Crate | What it is |
|---|---|
| `crates/wirelab-proto` | `no_std` wire protocol shared verbatim by host and firmware: pin modes, PWM, telemetry, debounced events, hot-swappable behaviors. Framing = postcard + CRC16 + COBS. |
| `crates/wirelab-core` | Board profiles (JSON), component library (JSON), circuit graph, netlist extraction + wire-short verdicts, resistive-nodal-analysis simulator, static lints, the rules engine, the Rhai script host, and `plan_setup` — which derives the entire pin configuration from the wiring itself. |
| `crates/wirelab-link` | Host-side device layer: `SerialDevice` (real hardware) and `SimDevice` (full in-process simulation) behind one `Device` trait, plus the `Session` handshake/telemetry state machine. |
| `crates/wirelab-gui` | The desktop app (`wirelab` binary). |
| `firmware/wirelab-fw` | The ESP32 runtime firmware (own workspace; esp-hal 1.x, `no_std`). |

## Running the app

```sh
cargo run -p wirelab-gui --release
```

Workflow inside the app:

1. Pick a board (ESP32 DevKitC v4 and C3/C5/C6/S3 devkits ship in `assets/boards/`).
2. Click a component in the palette, click the canvas to place it.
3. Click a board pin, then a component terminal, to draw a wire (GND/3V3
   wires auto-color). While dragging, valid targets glow and electrical
   shorts are crossed out and refused — GND↔3V3/5V, 3.3 V↔5 V, or tying a
   driven output pin straight to a rail. `R` rotates, `Del` deletes, scroll
   zooms, right-drag pans. Drag on empty canvas to box-select several
   components, then **⚡ Auto wire** (toolbar or right-click) hooks them to
   free board pins by role — LEDs pick up a selected series resistor,
   photoresistors pair with one as a divider, pots land on ADC pins.
   Right-click anywhere for the context menu: attach scripts, rotate,
   delete, add any library component in place, or drop a **routing dot** —
   an electrically transparent junction for tidying long wire runs,
   ComfyUI-style. Hover a wire while placing a two-terminal part and the
   leads preview snapping in — click to **splice it in series** (the wire
   splits through the component); hold **Ctrl** to place without wiring.
   With a serial port picked, **⚡ Flash firmware** builds the runtime for
   the board's chip and espflashes it, streaming progress to the console.
   New to electronics? The **🔌 wiring guide** (palette header) explains the
   why — current loops, LED resistor math, pull-ups, voltage dividers — and
   the **Checks** panel turns mistakes into lessons: hovering a warning
   highlights the components involved on the canvas, and warnings WireLab
   knows how to remedy carry a **🔧 fix button** (a resistor-less LED gets
   the value computed from its forward voltage and spliced in with one
   click).
4. Hit **Connect** (Simulator or Serial). Pin modes are derived from the
   wiring automatically — a button wired to GND becomes `InputPullUp`, an LED
   becomes `Output`, a pot wiper becomes `Analog` — and applied live.
5. Poke things: click-and-hold buttons, flip switches, drag pot sliders.
   LEDs glow, servos sweep, analog pins read out in millivolts.
6. Open the **Program** tab and build rules: *when button pressed → toggle
   LED*, *every 1000 ms → servo set_angle*, *analog above 2000 mV → relay on*.
   **▶ Run program** executes them against whatever is connected.
7. Hover a board pin to light up its whole function group — all GND pins,
   the ADC bank, strapping pins, UART0, FSPI/SDIO/LP-* buses (profiles carry
   per-pin `tags`; caps like input-only and USB-JTAG group automatically).
   Boards can also declare on-board **features**: the C5 profile draws its
   WS2812 RGB LED plus clickable **RESET** and **BOOT** buttons — RESET
   pulses EN through the UART bridge's DTR/RTS circuit (esptool-style) and
   re-handshakes, BOOT drops the chip into ROM download mode for flashing.
   Board capabilities (Wi-Fi 6, BLE, 802.15.4…) show under the board picker
   and are queryable from scripts via `board_has("wifi")` / `chip()`.
8. Select a component and open the **Script** section to attach a
   per-component script, Godot-style (see below).
9. The **Examples** menu ships board-only starters: a WS2812 rainbow whose
   speed the BOOT button controls, a PWM breather on GPIO2, and a
   rules-engine blinker — open one, hit Connect, and read the script to see
   how it's done.
8. **✨ AI board import** turns a pasted manufacturer pinout/spec into a new
   board profile (uses the Anthropic API; needs `ANTHROPIC_API_KEY`).

Rules run on the host, but repeating patterns (blink, breathe, mirror) are
pushed into firmware **behavior slots** so they keep running with zero 
round-trips — reconfigure them at runtime, no reflash.

## MCP: let an AI build with you

The **🤖 MCP** toolbar button (or `WIRELAB_MCP=1`) starts an embedded MCP
server (`http://127.0.0.1:4517/mcp`, streamable HTTP via rmcp). Connect any
MCP client — e.g. `claude mcp add -t http wirelab http://127.0.0.1:4517/mcp` —
and the AI can work the live app: `get_circuit` / `list_library` /
`validate_circuit` to inspect, `add_component` / `add_wire` (short-circuits
refused with reasons) / `auto_wire` / `fix_lints` to build, `set_script`
(with analyzer diagnostics) to program, and `connect_simulator` +
`set_component_state` + `read_live` to actually press the buttons and watch
the LEDs. Everything the AI does appears on your canvas in real time.

## Roadmap: radios, buses, displays

The C5's Wi-Fi/BLE/802.15.4 and the SPI/I2C/UART buses are physically
there but not yet driven by the firmware — scripts can't reach them until
it does. The IDE's hardware tree has an info sheet per capability with the
concrete plan; the short version: **UART1 first** (unlocks the RDM6300 and
serial modules), **SPI + an ST7735 driver in firmware** next (scripts send
`lcd.text(...)`-style commands; pixels never cross the serial link), then
**esp-wifi** — whose first payoff is the WireLab link itself over TCP with
mDNS discovery, which is also the doorway to editing from an iPad.

## Component scripts

Every placed component can carry a script (Rhai — Rust-like syntax), exactly
like attaching a script to a Godot node. Scripts hot-swap on **▶ Apply**: no
recompile, no reflash, and they are always live while connected — no Run
needed. Callbacks fire from real hardware events and simulator events alike:

```rust
// Script on the push button `btn1`. State lives on `this`.
fn on_press() {
    this.count = (this.count ?? 0) + 1;
    led1.toggle();                      // other components, by label
    if this.count % 5 == 0 {
        buzzer.beep(120);
        after(300, || servo.set_angle(this.count * 10));
    }
    log(`pressed ${this.count} times`); // lands in the Console tab
}
```

| | |
|---|---|
| Callbacks | `on_start()`, `on_press()`, `on_release()`, `on_change(on)`, `on_reading(mv)`, `on_tick(dt_ms)`, `on_pin(gpio, high)` |
| Component verbs | `x.on()` `.off()` `.toggle()` `.blink(ms)` `.breathe(ms)` `.dim(pct)` `.set_angle(deg)` `.beep(ms)` `.tone(hz, ms)` |
| Reads | `x.is_on()`, `x.is_pressed()`, `x.millivolts()`, `pin(n).is_high()` |
| Raw pins | `pin(n).high()` `.low()` `.toggle()` `.pwm(hz, permille)` `.input_pullup()` `.output()` … |
| Board | `rgb(r, g, b)` — the on-board WS2812, real color over RMT · `chip()`, `board_has("wifi")` |
| Tools | `log(x)`, `millis()`, `after(ms, \|\| ...)`, `me` (own component) |

The editor completes as you type (component names, the API above, member
verbs after `.` — Tab/Enter accepts) and shows hover docs for identifiers
and diagnostics, all driven by the same tables that feed the analyzer.

Components are addressed by their sanitized label (`Red LED!` → `red_led`;
the inspector shows each component's script name). The editor lints **as
you type** using the vendored [rhai-lsp](lsp/) crates — rhai-rowan parses
for syntax errors, rhai-hir resolves references semantically (typo'd
component names get "did you mean …?") — with red underlines and a
diagnostics list; the WireLab API and your component names are declared to
the analyzer through a generated Rhai definition module. Compile and
runtime errors additionally surface on Apply and in the console; a runaway
script is cut off by an operation limit rather than freezing the app.
Scripts run on the host and drive the device through the same command path
as the rules engine, so the identical script works against the simulator
and the real board.

## Firmware

```sh
cd firmware/wirelab-fw

# ESP32-C3 (default) — USB-Serial-JTAG transport
cargo build --release
cargo run --release                # espflash flash --monitor

# ESP32-C5 — UART0 transport (the UART USB port), riscv32imac
cargo build --release --no-default-features --features esp32c5 \
    --target riscv32imac-unknown-none-elf
espflash flash --port /dev/ttyUSB0 \
    target/riscv32imac-unknown-none-elf/release/wirelab-fw

# ESP32-S3: --features esp32s3 (USB-Serial-JTAG)
# ESP32 classic: --features esp32 (UART0; needs the espup Xtensa toolchain)
```

Smoke-test a flashed board end to end (handshake → telemetry → GPIO write →
on-device blink behavior):

```sh
cargo run -p wirelab-link --example hil_check -- /dev/ttyUSB0
```

### Firmware capabilities

- Digital in/out with pull-up/down on every safe GPIO (flash pins excluded)
- Software PWM (LED dimming, servo pulses, passive-buzzer tones)
- Debounced input edge events (5 ms default, tunable per pin via `Watch`)
- Digital telemetry snapshots at a host-set interval
- 8 hot-swappable behavior slots (`Blink`, `Breathe`, `Mirror`, `Watch`)
- ADC one-shot reads + watched sampling on ESP32-C3 (GPIO0-4) and
  ESP32-C5 (GPIO1-6); other chips return `Unsupported` for `Analog` mode
- WS2812 addressable-LED writes over RMT (`SetRgb`) on ESP32-C5 (GPIO27)
  and ESP32-C3 (GPIO8) — drives the devkits' on-board RGB LED
  (hardware-verified on the C5)

## Asset libraries

- `assets/boards/*.json` — board profiles: every header pin with GPIO
  capabilities, ADC channels, strapping/flash warnings. Validated by
  `cargo test -p wirelab-core --test assets`.
- `assets/components/*.json` — the component library (LEDs, buttons,
  switches, pots, LDR, buzzers, servo, relay, PIR, touch, soil moisture...),
  each with terminals, an electrical sim model, actions and events.

Add your own with a text editor, or let the AI importer write board profiles
for you. Single-file validation:

```sh
cargo run -p wirelab-core --example validate_asset -- assets/boards/my-board.json
```

## Tests

```sh
cargo test --workspace       # protocol roundtrips, netlist/sim/engine, e2e sim loop, assets
```

The flagship test (`crates/wirelab-link/tests/live_loop.rs`) runs the entire
stack — session handshake, auto pin setup, button press event, rules engine,
LED lit in the electrical solve, telemetry — against the simulator, no
hardware required.
