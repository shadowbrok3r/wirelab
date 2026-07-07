#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai_import;
mod app;
mod canvas;
mod draw;
mod flash;
mod flow_ui;
mod http_fetch;
mod ide;
mod ide_snippets;
mod live;
mod mcp;
mod panels;
mod panels_info;
mod rhai_docs;
mod rhai_lint;
mod wiring_guide;

fn main() -> eframe::Result {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1480.0, 940.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("WireLab — live ESP32 circuit builder"),
        ..Default::default()
    };
    eframe::run_native(
        "WireLab",
        options,
        Box::new(|cc| Ok(Box::new(app::WireLabApp::new(cc)))),
    )
}
