//! The dockable workspace: tab kinds, the default layout, queued tab requests and the tab viewer.

use std::collections::HashSet;

use egui::{Id, Ui, WidgetText};
use egui_dock::tab_viewer::OnCloseResponse;
use egui_dock::{DockArea, DockState, NodeIndex, NodePath, SurfaceIndex, TabViewer};
use serde::{Deserialize, Serialize};
use wirelab_core::circuit::CompId;

use crate::app::WireLabApp;
use crate::ui_kit::{self, ph};

/// eframe storage key of the saved layout.
const STORAGE_KEY: &str = "wirelab.dock";
const EJECT_LABEL: &str = "Undock";

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tab {
    Schematic,
    Parts,
    Explorer,
    Inspector,
    Checks,
    Console,
    Problems,
    Script(CompId),
    Flow,
    Rules,
    Info(String),
    Reference,
    WiringGuide,
}

impl Tab {
    /// Tabs the View menu and the add popup offer, in menu order.
    pub const PANELS: [Tab; 11] = [
        Tab::Schematic,
        Tab::Parts,
        Tab::Explorer,
        Tab::Inspector,
        Tab::Checks,
        Tab::Console,
        Tab::Problems,
        Tab::Flow,
        Tab::Rules,
        Tab::Reference,
        Tab::WiringGuide,
    ];

    pub fn icon(&self) -> egui_icon_pack::Icon {
        match self {
            Tab::Schematic => ph::CIRCUITRY,
            Tab::Parts => ph::PUZZLE_PIECE,
            Tab::Explorer => ph::TREE_STRUCTURE,
            Tab::Inspector => ph::SLIDERS_HORIZONTAL,
            Tab::Checks => ph::SHIELD_CHECK,
            Tab::Console => ph::TERMINAL_WINDOW,
            Tab::Problems => ph::BUG,
            Tab::Script(_) => ph::FILE_CODE,
            Tab::Flow => ph::FLOW_ARROW,
            Tab::Rules => ph::LIST_CHECKS,
            Tab::Info(_) => ph::INFO,
            Tab::Reference => ph::BOOK_OPEN,
            Tab::WiringGuide => ph::PLUGS_CONNECTED,
        }
    }

    /// Fixed title; script and info tabs are named by the app.
    pub fn label(&self) -> &'static str {
        match self {
            Tab::Schematic => "Schematic",
            Tab::Parts => "Parts",
            Tab::Explorer => "Explorer",
            Tab::Inspector => "Inspector",
            Tab::Checks => "Checks",
            Tab::Console => "Console",
            Tab::Problems => "Problems",
            Tab::Script(_) => "Script",
            Tab::Flow => "Flow graph",
            Tab::Rules => "Rules",
            Tab::Info(_) => "Info",
            Tab::Reference => "Rhai reference",
            Tab::WiringGuide => "Wiring guide",
        }
    }

    /// Opens next to the schematic rather than in the focused panel.
    fn is_document(&self) -> bool {
        matches!(
            self,
            Tab::Script(_)
                | Tab::Flow
                | Tab::Rules
                | Tab::Info(_)
                | Tab::Reference
                | Tab::WiringGuide
        )
    }
}

/// A change to the dock, applied after the dock pass.
#[derive(Clone, Debug)]
pub enum DockRequest {
    /// Focus the tab, opening it first when absent.
    Open(Tab),
    /// Like `Open`, but a new tab goes into the given leaf.
    OpenAt(Tab, NodePath),
    Close(Tab),
    /// Drop every script tab (their component ids belong to another circuit).
    CloseScripts,
    Reset,
}

/// Parts, explorer on the left; inspector over checks on the right; console under the schematic.
pub fn default_layout() -> DockState<Tab> {
    let mut dock = DockState::new(vec![Tab::Schematic]);
    dock.translations.tab_context_menu.eject_button = EJECT_LABEL.to_owned();
    let tree = dock.main_surface_mut();
    // Each fraction is the share of the left or top child.
    let [docs, _left] = tree.split_left(NodeIndex::root(), 0.19, vec![Tab::Parts, Tab::Explorer]);
    let [docs, right] = tree.split_right(docs, 0.76, vec![Tab::Inspector]);
    tree.split_below(right, 0.62, vec![Tab::Checks]);
    tree.split_below(docs, 0.74, vec![Tab::Console, Tab::Problems]);
    dock.set_focused_node_and_surface(NodePath::new(SurfaceIndex::main(), docs));
    dock
}

/// The saved layout without script tabs, or the default.
pub fn load_layout(storage: Option<&dyn eframe::Storage>) -> DockState<Tab> {
    restore(storage.and_then(|s| eframe::get_value::<DockState<Tab>>(s, STORAGE_KEY)))
}

/// A saved layout without script tabs, or the default when there is none or it lost the schematic.
fn restore(saved: Option<DockState<Tab>>) -> DockState<Tab> {
    let Some(mut dock) = saved else {
        return default_layout();
    };
    dock.retain_tabs(|t| !matches!(t, Tab::Script(_)));
    if dock.find_tab(&Tab::Schematic).is_none() {
        return default_layout();
    }
    dock.translations.tab_context_menu.eject_button = EJECT_LABEL.to_owned();
    dock
}

pub fn save_layout(dock: &DockState<Tab>, storage: &mut dyn eframe::Storage) {
    eframe::set_value(storage, STORAGE_KEY, dock);
}

fn apply(dock: &mut DockState<Tab>, requests: Vec<DockRequest>) {
    for request in requests {
        match request {
            DockRequest::Open(tab) => {
                if let Some(path) = dock.find_tab(&tab) {
                    let _ = dock.set_active_tab(path);
                    dock.set_focused_node_and_surface(NodePath::new(path.surface, path.node));
                    continue;
                }
                if tab.is_document()
                    && let Some(path) = dock.find_tab(&Tab::Schematic)
                {
                    dock.set_focused_node_and_surface(NodePath::new(path.surface, path.node));
                }
                dock.push_to_focused_leaf(tab);
            }
            DockRequest::OpenAt(tab, leaf) => {
                if let Some(path) = dock.find_tab(&tab) {
                    let _ = dock.set_active_tab(path);
                    continue;
                }
                dock.set_focused_node_and_surface(leaf);
                dock.push_to_focused_leaf(tab);
            }
            DockRequest::Close(tab) => {
                if let Some(path) = dock.find_tab(&tab) {
                    dock.remove_tab(path);
                }
            }
            DockRequest::CloseScripts => dock.retain_tabs(|t| !matches!(t, Tab::Script(_))),
            DockRequest::Reset => *dock = default_layout(),
        }
    }
}

impl WireLabApp {
    pub fn open_tab(&mut self, tab: Tab) {
        self.dock_requests.push(DockRequest::Open(tab));
    }

    pub fn is_tab_open(&self, tab: &Tab) -> bool {
        self.dock_open.contains(tab)
    }

    /// Draws the dock filling `ui`, then applies the tab requests queued during the frame.
    pub fn show_dock(&mut self, ui: &mut Ui) {
        let requests = std::mem::take(&mut self.dock_requests);
        apply(&mut self.dock, requests);
        let parts = &self.project.circuit.components;
        let orphan = |t: &Tab| matches!(t, Tab::Script(id) if !parts.contains_key(id));
        if self.dock.iter_all_tabs().any(|(_, t)| orphan(t)) {
            self.dock.retain_tabs(|t| !orphan(t));
        }
        self.dock_open = self
            .dock
            .iter_all_tabs()
            .map(|(_, t)| t.clone())
            .collect::<HashSet<_>>();

        let style = egui_theme_kit::dock_style(ui.ctx());
        let mut dock = std::mem::replace(&mut self.dock, DockState::new(Vec::new()));
        DockArea::new(&mut dock)
            .style(style)
            .show_close_buttons(true)
            .show_add_buttons(true)
            .show_add_popup(true)
            .draggable_tabs(true)
            .show_inside(ui, self);
        self.dock = dock;

        let requests = std::mem::take(&mut self.dock_requests);
        apply(&mut self.dock, requests);
    }

    fn tab_name(&self, tab: &Tab) -> String {
        match tab {
            Tab::Script(id) => {
                let name = self.comp_name(*id);
                if self.docs.is_dirty(*id, &self.project.circuit) {
                    format!("{name} \u{2022}")
                } else {
                    name
                }
            }
            Tab::Info(key) => crate::panels_info::title(key).to_string(),
            other => other.label().to_string(),
        }
    }
}

impl TabViewer for WireLabApp {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> Id {
        Id::unique(("wirelab.tab", &*tab))
    }

    fn title(&mut self, tab: &mut Tab) -> WidgetText {
        ui_kit::tab_title(tab.icon(), &self.tab_name(tab))
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        match tab {
            Tab::Schematic => self.show_schematic(ui),
            Tab::Parts => self.show_parts(ui),
            Tab::Explorer => self.show_explorer(ui),
            Tab::Inspector => self.show_inspector_tab(ui),
            Tab::Checks => self.show_checks(ui),
            Tab::Console => self.show_console_tab(ui),
            Tab::Problems => self.show_problems(ui),
            Tab::Script(id) => self.show_script_tab(ui, *id),
            Tab::Flow => self.show_flow_tab(ui),
            Tab::Rules => self.show_rules_tab(ui),
            Tab::Info(key) => self.show_info_tab(ui, key),
            Tab::Reference => self.show_reference_tab(ui),
            Tab::WiringGuide => self.show_wiring_tab(ui),
        }
    }

    fn is_closeable(&self, tab: &Tab) -> bool {
        *tab != Tab::Schematic
    }

    fn on_close(&mut self, _tab: &mut Tab) -> OnCloseResponse {
        OnCloseResponse::Close
    }

    fn scroll_bars(&self, _tab: &Tab) -> [bool; 2] {
        [false, false]
    }

    fn context_menu(&mut self, ui: &mut Ui, tab: &mut Tab, _path: NodePath) {
        match tab {
            Tab::Script(id) => {
                let id = *id;
                let dirty = self.docs.is_dirty(id, &self.project.circuit);
                if ui_kit::menu_item(ui, ph::PLAY, "Apply").clicked() && dirty {
                    self.apply_script(id);
                }
                if ui_kit::menu_item(ui, ph::ARROW_COUNTER_CLOCKWISE, "Revert").clicked() {
                    self.docs.revert(id, &self.project.circuit);
                }
            }
            Tab::Console if ui_kit::menu_item(ui, ph::ERASER, "Clear").clicked() => {
                self.console.clear()
            }
            _ => {}
        }
    }

    fn add_popup(&mut self, ui: &mut Ui, path: NodePath) {
        egui_theme_kit::controls::menu_body(ui, |ui| {
            for tab in Tab::PANELS {
                let open = self.dock_open.contains(&tab);
                let row = egui_theme_kit::controls::menu_check_item(
                    ui,
                    ui_kit::kit(tab.icon()),
                    tab.label(),
                    open,
                );
                if row.clicked() {
                    let request = if open {
                        DockRequest::Close(tab)
                    } else {
                        DockRequest::OpenAt(tab, path)
                    };
                    self.dock_requests.push(request);
                    ui.close();
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// eframe storage in memory: the same RON path the app persists through.
    #[derive(Default)]
    struct MemoryStorage(std::collections::HashMap<String, String>);

    impl eframe::Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }

        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_string(), value);
        }

        fn remove_string(&mut self, key: &str) {
            self.0.remove(key);
        }

        fn flush(&mut self) {}
    }

    fn count(dock: &DockState<Tab>, tab: &Tab) -> usize {
        dock.iter_all_tabs().filter(|(_, t)| *t == tab).count()
    }

    #[test]
    fn default_layout_holds_each_panel_once() {
        let dock = default_layout();
        for tab in [Tab::Schematic, Tab::Parts, Tab::Explorer, Tab::Inspector, Tab::Checks, Tab::Console, Tab::Problems] {
            assert_eq!(count(&dock, &tab), 1, "{tab:?}");
        }
        assert_eq!(dock.iter_all_tabs().count(), 7);
    }

    #[test]
    fn documents_open_beside_the_schematic_once() {
        let mut dock = default_layout();
        let script = Tab::Script(CompId(3));
        apply(&mut dock, vec![DockRequest::Open(Tab::Console), DockRequest::Open(script.clone())]);
        let (s, schematic) = (dock.find_tab(&script).unwrap(), dock.find_tab(&Tab::Schematic).unwrap());
        assert_eq!((s.surface, s.node), (schematic.surface, schematic.node));

        apply(&mut dock, vec![DockRequest::Open(script.clone()), DockRequest::Open(Tab::Parts)]);
        assert_eq!(count(&dock, &script), 1);
        assert_eq!(count(&dock, &Tab::Parts), 1);

        apply(&mut dock, vec![DockRequest::Close(Tab::Parts), DockRequest::CloseScripts]);
        assert_eq!(count(&dock, &script), 0);
        assert_eq!(count(&dock, &Tab::Parts), 0);
        apply(&mut dock, vec![DockRequest::Reset]);
        assert_eq!(count(&dock, &Tab::Parts), 1);
    }

    #[test]
    fn a_saved_layout_restores_without_script_tabs() {
        let mut dock = default_layout();
        apply(&mut dock, vec![DockRequest::Open(Tab::Script(CompId(1))), DockRequest::Open(Tab::Info("uart".into()))]);
        let mut storage = MemoryStorage::default();
        save_layout(&dock, &mut storage);
        let restored = load_layout(Some(&storage));
        assert_eq!(count(&restored, &Tab::Script(CompId(1))), 0);
        assert_eq!(count(&restored, &Tab::Info("uart".into())), 1);
        assert_eq!(restored.translations.tab_context_menu.eject_button, EJECT_LABEL);

        let mut lost = default_layout();
        lost.retain_tabs(|t| *t != Tab::Schematic);
        assert_eq!(count(&restore(Some(lost)), &Tab::Schematic), 1);
    }
}
