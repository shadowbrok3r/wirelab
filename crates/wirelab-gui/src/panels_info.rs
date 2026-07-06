//! Capability / roadmap sheets shown as IDE info tabs: what each piece of
//! hardware can do today from scripts, and what is planned.

use crate::app::WireLabApp;

pub fn ide_info(app: &WireLabApp, key: &str) -> String {
    match key {
        "board" => {
            let Some(board) = app.lib.board(&app.project.circuit.board_id) else {
                return "no board selected".into();
            };
            let specs = board
                .specs
                .iter()
                .map(|s| format!("  • {s}"))
                .collect::<Vec<_>>()
                .join("\n");
            let mut extras = String::new();
            if let Some(g) = board.features.rgb_led_gpio {
                extras += &format!(
                    "RGB LED (GPIO{g})   rgb(r, g, b)          — real color over RMT\n"
                );
            }
            if let Some(g) = board.features.boot_button_gpio {
                extras += &format!(
                    "BOOT button        pin({g}).input_pullup() then on_pin({g}, high)\n"
                );
            }
            if board.features.reset_button {
                extras += "RESET button       click it on the canvas (EN pulse over serial)\n";
            }
            format!(
                "{} — {}\n\n{}\n\nScriptable today:\n{}\nAny GPIO:           pin(n).high()/.low()/.pwm(hz, permille)/.watch_analog(ms)\nBoard queries:      chip(), board_has(\"wifi\")\n\nThe radios and buses in the tree are physically present but need firmware\nsupport before scripts can drive them — click them for the plan.",
                board.name,
                board.chip.name(),
                specs,
                extras
            )
        }
        "wifi" => "Wi-Fi (2.4 & 5 GHz) — AVAILABLE ✔ (the link itself)\n\n\
            The board can carry the whole WireLab protocol over TCP:\n\n\
            1. Connect over serial, click the Wi-Fi icon in the device bar,\n\
               enter your network's SSID + password, Join.\n\
            2. The board reports its IP and starts broadcasting discovery\n\
               beacons (UDP 4519) every 2 s.\n\
            3. Pick 'Wi-Fi (TCP)' as the backend — discovered boards appear\n\
               in the dropdown — or use 'Switch link to Wi-Fi' right from the\n\
               Wi-Fi menu. After that the USB cable is only power.\n\n\
            Everything works over TCP exactly like serial: telemetry, scripts,\n\
            UART/SPI/I2C/LCD, the RGB LED. Both links can even run at once\n\
            (frames go to whichever is attached).\n\n\
            Credentials live in board RAM — a reset or power cycle forgets\n\
            them; re-join over serial. Script APIs (http_get host-mediated,\n\
            wifi_rssi) are the next step. The iPad host rides this link.".into(),
        "ble" => "Bluetooth LE — PLANNED\n\n\
            Needs `esp-wifi`'s BLE controller + a host stack (trouble/bleps).\n\
            Realistic first script APIs:\n\
              ble_advertise(name, data)      — beacon mode\n\
              on_ble_scan(name, rssi, data)  — observer mode\n\
            GATT services later. BLE and Wi-Fi share the radio; both can run.".into(),
        "zigbee" => "802.15.4 (Zigbee / Thread) — PLANNED (far)\n\n\
            The C5 radio speaks 802.15.4, but Zigbee/Thread are heavy protocol stacks.\n\
            Raw 802.15.4 frames (esp-ieee802154) would come first — enough for\n\
            board-to-board WireLab links. Full Zigbee needs a dedicated effort.".into(),
        "uart" => "UART / serial — AVAILABLE ✔\n\n\
            UART1, routable to any broken-out pin (C3 & C5 firmware):\n\
              uart(tx, rx, baud)      — claim it (baud 0 releases)\n\
              uart_send(\"AT\\r\\n\")     — strings or [bytes]\n\
              on_uart(line)           — complete lines back to every script\n\n\
            The SIMULATOR loops writes straight back — try the UART echo\n\
            example with zero wiring. On hardware, jumper TX→RX for the same\n\
            echo, or wire an RDM6300's TX to your RX pin at 9600 baud and\n\
            read tag ids in on_uart.\n\
            (UART0 stays reserved — it IS the WireLab link on the C5.)".into(),
        "spi" => "SPI — ST7735 DISPLAY AVAILABLE ✔ (generic SPI planned)\n\n\
            The display path shipped first, driven by drawing COMMANDS (pixels\n\
            never cross the serial link):\n\
              lcd_init(sck, mosi, cs, dc, rst)\n\
              lcd_clear(r, g, b)\n\
              lcd_rect(x, y, w, h, r, g, b)\n\
              lcd_text(x, y, \"hi\", r, g, b)      — 6x10 font, 128x128 canvas\n\n\
            The SIMULATOR renders the screen on any placed ST7735 component —\n\
            develop with zero wiring, then flash and wire to see it for real.\n\
            Try the LCD clock example. Panel offsets assume the common\n\
            128x128 green-tab glass; a shifted image means different offsets.\n\n\
            Generic SPI is ALSO available now:\n\
              spi_setup(sck, mosi, miso, freq_khz)\n\
              spi_xfer(cs, [0x9f, 0, 0])   →  on_spi([bytes])\n\
            One SPI2 bus: configuring generic SPI replaces the LCD and vice\n\
            versa (share the bus with different CS pins when both are needed\n\
            — LCD mode owns it for now). Sim echoes transfers back.".into(),
        "i2c" => "I2C — AVAILABLE ✔\n\n\
              i2c_setup(sda, scl, freq_khz)      — any pins; 100/400 kHz typical\n\
              i2c_write(addr, [bytes])\n\
              i2c_read(addr, reg, len)           — reg 256 = no register select\n\
              on_i2c(addr, [bytes])              — replies land here\n\n\
            Example: read a BME280's chip-id: i2c_setup(0, 1, 400);\n\
            i2c_read(0x76, 0xd0, 1) → on_i2c gets [0x60]. A failed read\n\
            (no device / wrong address) reports a device error in the console.\n\
            The simulator emulates a BME280 at 0x76 and an SHT31 at 0x44 for\n\
            offline testing; other addresses read as zeros.".into(),
        _ => "unknown info sheet".into(),
    }
}
