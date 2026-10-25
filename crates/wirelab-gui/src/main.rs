#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai_import;
mod app;
mod canvas;
mod console;
mod dock;
mod draw;
mod editor;
mod explorer;
mod flash;
mod flow_ui;
mod http_fetch;
mod icons;
mod ide_snippets;
mod lcd;
mod live;
mod mcp;
mod panels;
mod panels_info;
mod rhai_docs;
mod rhai_lint;
mod script_docs;
mod theme;
mod ui_kit;
mod wiring_guide;

use egui_theme_kit::chrome;
use egui_theme_kit::title_bar::windows::client_area;

fn main() -> eframe::Result {
    env_logger::init();
    let chrome = chrome::resolve_from_process(theme::APP_ID);
    let viewport = chrome::viewport(
        egui::ViewportBuilder::default()
            .with_inner_size([1480.0, 940.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("WireLab — live ESP32 circuit builder"),
        &chrome,
    );
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "WireLab",
        options,
        Box::new(move |cc| {
            if !client_area::install(cc) {
                log::warn!("title bar client-area fix not installed");
            }
            Ok(Box::new(app::WireLabApp::new(cc, chrome)))
        }),
    )
}
