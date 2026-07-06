//! Circuits 101: a beginner-friendly wiring reference. No prior electronics
//! knowledge assumed — this is the "why", the Checks panel is the "hey, fix
//! this", and ⚡ Auto wire is the "just do it for me".

/// (title, body) sections; code fences render monospaced.
const SECTIONS: &[(&str, &str)] = &[
    (
        "The one rule: current flows in loops",
        "Electricity only does something when it can flow in a complete loop: \
         out of a supply pin (a GPIO driven high, 3V3, or 5V), through your \
         component, and back to **GND**. No loop — nothing happens; that's why \
         every circuit here ends at a GND pin.\n\
         A GPIO pin is just a tiny switch the chip controls: driven high it \
         acts like a weak 3.3 V supply, driven low it acts like a connection \
         to GND. All GND pins on the board are the same wire internally — use \
         whichever is closest.",
    ),
    (
        "LEDs always need a resistor",
        "An LED is a diode: below its 'forward voltage' (~2 V for red) almost \
         no current flows; above it, current rises almost without limit — the \
         LED does NOT protect itself. Something else must limit the current, \
         and that's the series resistor's whole job.\n\
         How to size it (Ohm's law, R = V / I):\n```\nsupply        3.3 V   (a GPIO driven high)\nLED drop     -2.0 V   (its forward voltage)\nleft over     1.3 V   across the resistor\n\ntarget ~6 mA:  R = 1.3 V / 0.006 A ≈ 217 Ω  →  220 Ω stock part\n```\nMore ohms = dimmer and safer; fewer = brighter and hotter. 220–330 Ω \
         is the classic range on 3.3 V boards. The resistor can sit on either \
         side of the LED — the loop current is the same everywhere.\n\
         Polarity matters: current enters the **anode (+)** and leaves the \
         **cathode (−)** (the flat edge on the canvas). Backwards = simply \
         dark, not damaged.\n\
         Wiring: `GPIO → resistor → LED anode`, `LED cathode → GND`. Hover an \
         existing wire while placing the resistor and it splices itself in. \
         The Checks panel computes the value and offers a 🔧 fix button when \
         you forget.",
    ),
    (
        "Buttons: why a pull-up?",
        "A push button is just two pieces of metal. Wire one side to a GPIO \
         and the other to GND:\n```\nGPIO4 ── button ── GND\n```\nPressed: the pin is connected to GND and reads LOW. Released: the pin \
         is connected to… nothing. A disconnected ('floating') pin picks up \
         electrical noise and reads randomly — that's the classic beginner trap.\n\
         The cure is a **pull-up**: a weak internal resistor to 3.3 V that \
         holds the pin HIGH whenever nothing stronger (the button) pulls it \
         LOW. Every ESP32 pin has one built in, and WireLab enables it \
         automatically when it sees this wiring (that's the `InputPullUp` in \
         the console).\n\
         Note the logic comes out inverted — pressed reads LOW. WireLab hides \
         that: `on_press` and `me.is_pressed()` are already the right way up.",
    ),
    (
        "Switches",
        "A toggle switch wires exactly like a button (`GPIO → switch → GND`, \
         pull-up on) — it just stays where you leave it.\n\
         A slide switch (SPDT) has three pins: the **COM**mon in the middle \
         connects to one side or the other:\n```\n3V3 ── A   COM ── GPIO   B ── GND\n```\nso the GPIO reads solid HIGH in one position and solid LOW in the \
         other — no floating, no pull-up needed.",
    ),
    (
        "Potentiometers & voltage dividers",
        "Two resistors in a row from 3.3 V to GND split the voltage at their \
         midpoint:\n```\nVout = 3.3 V × R_bottom / (R_top + R_bottom)\n```\nA potentiometer is both resistors in one part — the wiper is the \
         midpoint, so turning it sweeps 0 → 3.3 V. Wire ends to 3V3 and GND, \
         **wiper to an ADC pin** (GPIO1–6 on the C5).\n\
         A photoresistor (LDR) changes resistance with light, so pair it with \
         a fixed resistor to make the divider:\n```\n3V3 ── LDR ──●── 10k ── GND\n             │\n            ADC pin\n```\nBright light → LDR resistance drops → the midpoint rises. The \
         night-light example uses exactly this.",
    ),
    (
        "Buzzers, servos, relays: signal vs power",
        "Modules with V+/G/SIG pins split two jobs: the **power pins** carry \
         the real current (V+ → 3V3 or 5V, G → GND), while **SIG** only \
         carries information from a GPIO.\n\
         Servos want 5 V power and a PWM signal (`me.set_angle(deg)` handles \
         the pulses). Never try to power a motor or servo *from* a GPIO — \
         pins can source a few tens of mA at best; that's what the supply \
         pins are for. An active buzzer is the simple case: SIG high = noise.",
    ),
    (
        "Pins to treat with respect",
        "• **Strapping pins** (pink warnings): the chip reads them at power-on \
         to decide how to boot. Fine as outputs after boot; risky to hold \
         high/low through a reset.\n\
         • **UART0 pins** (GPIO11/12 on the C5): they ARE the WireLab link — \
         wiring them breaks the connection.\n\
         • **USB pins** (GPIO13/14): the native USB port.\n\
         • Keep any single GPIO under ~10 mA continuous.\n\
         Hover any pin: its function group lights up and the tooltip lists \
         caveats. The auto-wirer avoids all of these on its own.",
    ),
    (
        "Series vs parallel (the classic LED mistake)",
        "Wiring a resistor ACROSS an LED's + and − puts it in **parallel**: \
         both parts see the same voltage and each draws its own current — the \
         resistor does nothing to protect the LED. Current limiting only works \
         in **series**, where every electron must pass through the resistor \
         first:\n```\nparallel (wrong):   GPIO ──┬── LED ──┬── GND\n                           └── 220Ω ─┘\n\nseries (right):     GPIO ── 220Ω ── LED ── GND\n```\nWireLab warns about both problems: the parallel arrangement gets its \
         own lint, and the live simulator estimates real currents — a directly \
         driven LED shows a '~52 mA (rating ~20 mA)' warning while connected.",
    ),
    (
        "Measure an unknown resistor (ohmmeter)",
        "Got a mystery resistor? Use one KNOWN resistor and the ADC:\n```\n3V3 ── unknown R ──●── known 1k ── GND\n                   │\n                 GPIO1 (ADC)\n```\nThe two resistors divide 3.3 V; measuring the midpoint gives you the \
         unknown:\n```\nR_unknown = R_known × (3300 − mv) / mv\n```\nThe **Ohmmeter** entry in the Examples menu ships this ready to run — a \
         script watches GPIO1 with `pin(1).watch_analog(200)`, logs the \
         computed resistance and snaps it to the nearest standard (E12) value.\n\
         Two things limit the range: the ESP32 ADC **pegs near ~3.1 V**, and \
         it is inaccurate below ~0.1 V. So keep the reference within ~10× of \
         the unknown — a 1 k reference covers ≈130 Ω to 30 kΩ. The script \
         detects pegged readings and tells you whether to swap in a smaller \
         or bigger reference. (A few percent off is normal — good enough to \
         identify parts, not lab metrology.)",
    ),
    (
        "Let WireLab do the wiring",
        "Three levels of help, laziest first:\n\
         • **⚡ Auto wire** — box-select components (drag on empty canvas), \
         then the toolbar button or right-click wires them to sensible free \
         pins: buttons get pull-ups, LEDs pick up a selected resistor in \
         series, pots land on ADC pins.\n\
         • **Splice** — hover a wire while placing a two-terminal part: the \
         leads preview snapping in; click and it's inserted in series.\n\
         • **Checks fixes** — warnings in the Inspector highlight the parts \
         involved when hovered, and the 🔧 button applies the suggested \
         remedy (like adding that LED resistor, value pre-computed).",
    ),
];

pub fn show_wiring_window(ctx: &egui::Context, open: &mut bool, filter: &mut String) {
    crate::rhai_docs::show_reference_window(ctx, "🔌 Wiring guide", SECTIONS, open, filter);
}
