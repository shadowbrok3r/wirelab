# LabWired ESP32-C6 test (2026-11-25)

Can labwired-core (github.com/w1ne/labwired-core, MIT) run esp-hal firmware for the ESP32-C6
and close a loop with an analog circuit, as WireLab would?

- `probe/`: esp-hal 1.1.1 firmware (Rust 2024) for riscv32imac. It keeps GPIO19 (relay) high
  while the GPIO1 ADC reads below 1600 counts and prints each change over UART0.
- `welder-c6.yaml`: the labwired esp32c6-devkitc board plus an `adapter: analog` co-sim model:
  300 V source -> relay switch (board.gpio.gpio19) -> 190 uF bank -> 300k/2k divider -> ADC1 ch1
  (`adc.apb_saradc.1_volts`). Replace LABWIRED/ with the labwired checkout path.
- `test.yaml`: `labwired test` script asserting the firmware prints.

Run from the labwired checkout (built with `cargo build --release -p labwired-cli` on stable):

    LABWIRED_ESP32C6_ROM=$PWD/crates/core/roms/esp32c6/esp32c6_rom.bin \
      ./target/release/labwired test --script test.yaml --output-dir out

Results:
- Boots via ELF fast-boot with the C6 mask ROM; UART0, GPIO19 output and the SAR ADC work.
- Closed loop works: relay on at 0 counts, off at ~1606 (bank ~195 V), back on at ~1599.
- `labwired run` (no co-sim): ~67 M instr/s, ~2.4x slower than real time at 160 MHz.
  `labwired test` with co-sim: ~3.7 M instr/s (~43x slower); co-sim step size is not the cause.
- SPI writes produced no bus-trace events; the ST7789 device model was not attached.
- The ADC model is linear 0-3.3 V -> 0-4095 and ignores attenuation and eFuse calibration.
- Gotchas: the `test` path needs LABWIRED_ESP32C6_ROM pinned; max_steps above 50M needs
  `limits.wall_time_ms`; the release CLI is 538 MB unstripped, 44 MB stripped.
- Embedding API: `labwired_core::system::builder::build_machine` -> `SessionMachine`
  (`advance`, `bind_cosim`/`advance_cosim`, `set_gpio_input`, `bus_read_u32`, logic capture).

Real welder firmware (pulse-arc-welder/firmware, esp-hal 1.2.2), 2026-11-25:
- `welder-real.yaml` adds a BOOT long-press (PULSE on `board.gpio_in.gpio9`, 0.3-1.6 s) and uses
  `usb_serial_jtag` as the console. It needs a copy of chips/esp32c6.yaml with
  `io_voltage_v: 3.3` and `gpio_input_thresholds: {vil: 0.25, vih: 0.75}` (named esp32c6-thr.yaml),
  with its `../peripherals/` paths made absolute.
- Results:
  - With the RWDT enabled it panics in esp-hal `clock::us_to_rtc_ticks` (divide by zero):
    LabWired's C6 does not model the RTC slow-clock calibration counter. Emulator gap.
  - With the 4 rwdt lines commented out (scratch copy only) the full cycle runs in 3.5 s of sim
    time (~154 s wall): banner, BOOT long-press at 0.6-1.9 s -> ARMED, IDLE->CHARGING,
    CHARGING->READY at 159.5 V for a 150 V setpoint, then a slow bleeder sag with the relay open.
  - A press that starts before the firmware's init finishes (~0.3 s) is ignored by design
    (button held at boot), so the PULSE must start later.
  - Console input can't be injected: `uart_injections` only accepts real UARTs, not usb_serial_jtag.
  - Finding for the bench: READY lands ~10 V over the stop point at ~800 V/s charge rate, because
    samples are ~10+ ms apart while a screen region draws. A slope-predictive stop would fix it.
