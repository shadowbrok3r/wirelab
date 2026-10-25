//! egui-theme-kit wiring: startup theme, on-disk theme store and the picker window.

use std::path::Path;
use std::sync::Arc;

use egui_theme_kit::picker::{self, PickerState};
use egui_theme_kit::store::JsonDirStore;
use egui_theme_kit::{FontConfig, KitConfig, debug_panel, persist};

/// Names `<config_dir>/wirelab/` for the theme library and the title-bar choice.
pub const APP_ID: &str = "wirelab";

pub struct ThemeState {
    pub picker: PickerState,
    store: Arc<JsonDirStore>,
}

impl ThemeState {
    /// Installs the kit with `default_theme` (a kit theme or bare `egui::Style` file) as the fallback look.
    pub fn new(cc: &eframe::CreationContext<'_>, default_theme: &Path) -> (Self, String) {
        let store = Arc::new(open_store());
        let mut cfg = KitConfig::new()
            .fonts(FontConfig::new().installer(|fonts| {
                egui_phosphor::add_to_fonts(fonts, egui_phosphor::Variant::Regular);
            }))
            .store(store.clone());
        let note = match persist::load_file(default_theme) {
            Ok(theme) => {
                cfg = cfg.default_theme(theme);
                format!("default theme: {}", default_theme.display())
            }
            Err(e) => format!("default theme not loaded ({e}); using the kit default"),
        };
        egui_theme_kit::init(cc, cfg);
        debug_panel::set_store(&cc.egui_ctx, store.clone());
        (Self { picker: PickerState::default(), store }, note)
    }

    /// The picker window and the Ctrl+Alt+T theme debug panel.
    pub fn show(&mut self, ctx: &egui::Context) {
        picker::window(ctx, &mut self.picker, &*self.store);
        debug_panel::show(ctx);
    }

    pub fn flush(&self) {
        self.store.flush();
    }
}

fn open_store() -> JsonDirStore {
    let dir =
        JsonDirStore::default_dir(APP_ID).unwrap_or_else(|| std::env::temp_dir().join(APP_ID));
    JsonDirStore::open(&dir)
        .or_else(|e| {
            log::warn!("theme store at {} unavailable ({e}); using a temp dir", dir.display());
            JsonDirStore::open(std::env::temp_dir().join(APP_ID))
        })
        .expect("open a theme directory")
}
