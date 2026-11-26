#![no_std]
#![no_main]

use esp_backtrace as _;
use esp_hal::analog::adc::{Adc, AdcConfig, Attenuation};
use esp_hal::delay::Delay;
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::main;
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::time::Rate;
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

#[main]
fn main() -> ! {
    let p = esp_hal::init(esp_hal::Config::default());
    let mut relay = Output::new(p.GPIO19, Level::Low, OutputConfig::default());
    let mut adc_cfg = AdcConfig::new();
    let mut sense = adc_cfg.enable_pin(p.GPIO1, Attenuation::_11dB);
    let mut adc = Adc::new(p.ADC1, adc_cfg);
    let mut spi = Spi::new(p.SPI2, SpiConfig::default().with_frequency(Rate::from_mhz(10)))
        .unwrap()
        .with_sck(p.GPIO7)
        .with_mosi(p.GPIO6);
    let mut cs = Output::new(p.GPIO14, Level::High, OutputConfig::default());
    let delay = Delay::new();
    println!("probe boot");
    cs.set_low();
    let _ = spi.write(&[0x2A, 0x00, 0x22, 0x00, 0xCD]);
    cs.set_high();
    let mut last = None;
    loop {
        let raw = nb::block!(adc.read_oneshot(&mut sense)).unwrap_or(u16::MAX);
        let charge = raw < 1600;
        if charge {
            relay.set_high();
        } else {
            relay.set_low();
        }
        if last != Some(charge) {
            println!("adc {} relay {}", raw, relay.is_set_high());
            last = Some(charge);
        }
        delay.delay_millis(2);
    }
}
