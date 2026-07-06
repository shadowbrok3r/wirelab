//! Project files: one or more boards, each with its own circuit, program and
//! flow graph, saved as JSON.
//!
//! The active board's data lives in the flat `circuit` / `program` / `flow`
//! fields so the rest of the app reads it without knowing about tabs. The full
//! set lives in `boards`; the active entry is synced from the flat fields on
//! every switch and before every save.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::circuit::Circuit;
use crate::flow::FlowGraph;
use crate::program::Program;

pub const PROJECT_EXTENSION: &str = "wirelab.json";

/// One board tab: a named circuit with its program and flow graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardTab {
    /// Stable identity across renames/reorders; unique within the project.
    #[serde(default)]
    pub id: u64,
    pub name: String,
    pub circuit: Circuit,
    #[serde(default)]
    pub program: Program,
    #[serde(default)]
    pub flow: FlowGraph,
}

#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
    /// Working copy of the active board (what the app reads and edits).
    pub circuit: Circuit,
    pub program: Program,
    pub flow: FlowGraph,
    /// Every board in tab order. `boards[active]` is kept in sync with the
    /// working copy above via [`Project::sync_active`]. Always non-empty.
    pub boards: Vec<BoardTab>,
    pub active: usize,
}

/// On-disk shape. New files carry `boards`; legacy single-board files carry the
/// flat `circuit` / `program` / `flow` and no `boards`.
#[derive(Serialize, Deserialize)]
struct SavedProject {
    name: String,
    #[serde(default)]
    active: usize,
    #[serde(default)]
    boards: Vec<BoardTab>,
    #[serde(default)]
    circuit: Option<Circuit>,
    #[serde(default)]
    program: Option<Program>,
    #[serde(default)]
    flow: Option<FlowGraph>,
}

impl Project {
    pub fn new(name: &str, board_id: &str) -> Self {
        let tab = BoardTab {
            id: 1,
            name: "Board 1".to_string(),
            circuit: Circuit::new(board_id),
            program: Program::default(),
            flow: FlowGraph::default(),
        };
        Project {
            name: name.to_string(),
            circuit: tab.circuit.clone(),
            program: tab.program.clone(),
            flow: tab.flow.clone(),
            boards: vec![tab],
            active: 0,
        }
    }

    /// Name of the active board tab.
    pub fn active_name(&self) -> &str {
        &self.boards[self.active].name
    }

    /// Fold the working copy back into the active tab. Call before switching,
    /// adding/removing a board, or saving.
    pub fn sync_active(&mut self) {
        let tab = &mut self.boards[self.active];
        tab.circuit = self.circuit.clone();
        tab.program = self.program.clone();
        tab.flow = self.flow.clone();
    }

    fn load_working(&mut self, i: usize) {
        let tab = &self.boards[i];
        self.circuit = tab.circuit.clone();
        self.program = tab.program.clone();
        self.flow = tab.flow.clone();
        self.active = i;
    }

    /// Make board `i` active; no-op if already active or out of range.
    pub fn switch_to(&mut self, i: usize) {
        if i == self.active || i >= self.boards.len() {
            return;
        }
        self.sync_active();
        self.load_working(i);
    }

    /// Append a fresh board targeting `board_id` and switch to it.
    pub fn add_board(&mut self, board_id: &str) {
        self.sync_active();
        let n = self.boards.len() + 1;
        let id = self.boards.iter().map(|b| b.id).max().unwrap_or(0) + 1;
        self.boards.push(BoardTab {
            id,
            name: format!("Board {n}"),
            circuit: Circuit::new(board_id),
            program: Program::default(),
            flow: FlowGraph::default(),
        });
        self.load_working(self.boards.len() - 1);
    }

    /// Remove board `i`; refuses to remove the last remaining board.
    pub fn remove_board(&mut self, i: usize) {
        if self.boards.len() <= 1 || i >= self.boards.len() {
            return;
        }
        self.sync_active();
        self.boards.remove(i);
        let next = if self.active > i {
            self.active - 1
        } else {
            self.active.min(self.boards.len() - 1)
        };
        self.active = usize::MAX; // force load_working to reload
        self.load_working(next);
    }

    pub fn rename_active(&mut self, name: String) {
        self.boards[self.active].name = name;
    }

    pub fn save(&mut self, path: &Path) -> std::io::Result<()> {
        self.sync_active();
        let saved = SavedProject {
            name: self.name.clone(),
            active: self.active,
            boards: self.boards.clone(),
            circuit: None,
            program: None,
            flow: None,
        };
        let text = serde_json::to_string_pretty(&saved)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, text)
    }

    pub fn load(path: &Path) -> std::io::Result<Project> {
        let text = std::fs::read_to_string(path)?;
        let saved: SavedProject = serde_json::from_str(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Project::from_saved(saved))
    }

    #[cfg(test)]
    fn from_saved_pub(saved_json: &str) -> Project {
        Project::from_saved(serde_json::from_str(saved_json).unwrap())
    }

    fn from_saved(saved: SavedProject) -> Project {
        let mut boards = saved.boards;
        if boards.is_empty() {
            // Legacy single-board file.
            let circuit = saved
                .circuit
                .unwrap_or_else(|| Circuit::new("esp32-c5-devkitc-1"));
            boards.push(BoardTab {
                id: 0, // assigned below
                name: "Board 1".to_string(),
                circuit,
                program: saved.program.unwrap_or_default(),
                flow: saved.flow.unwrap_or_default(),
            });
        }
        // Legacy files carry no ids (serde default 0); assign unique ones.
        let mut next = boards.iter().map(|b| b.id).max().unwrap_or(0);
        let mut seen = std::collections::HashSet::new();
        for b in &mut boards {
            if b.id == 0 || !seen.insert(b.id) {
                next += 1;
                b.id = next;
                seen.insert(b.id);
            }
        }
        let active = saved.active.min(boards.len() - 1);
        let tab = &boards[active];
        Project {
            name: saved.name,
            circuit: tab.circuit.clone(),
            program: tab.program.clone(),
            flow: tab.flow.clone(),
            boards,
            active,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_single_board_file_loads_as_one_board() {
        // A pre-multi-board file: flat circuit, no `boards`.
        let json = r#"{"name":"old","circuit":{"board_id":"esp32-c3-devkitc-02",
            "board_pos":[0.0,0.0],"components":{},"wires":{},"next_comp":1,"next_wire":1}}"#;
        let p = Project::from_saved_pub(json);
        assert_eq!(p.boards.len(), 1);
        assert_eq!(p.active, 0);
        assert_eq!(p.circuit.board_id, "esp32-c3-devkitc-02");
        assert_eq!(p.active_name(), "Board 1");
    }

    #[test]
    fn multi_board_add_switch_and_roundtrip() {
        let mut p = Project::new("lab", "esp32-c5-devkitc-1");
        p.add_board("esp32-c3-devkitc-02");
        assert_eq!(p.boards.len(), 2);
        assert_eq!(p.active, 1);
        assert_eq!(p.circuit.board_id, "esp32-c3-devkitc-02");

        // Edits to the active board must survive a switch away and back.
        p.circuit.board_pos = [12.0, 34.0];
        p.switch_to(0);
        assert_eq!(p.circuit.board_id, "esp32-c5-devkitc-1");
        p.switch_to(1);
        assert_eq!(p.circuit.board_pos, [12.0, 34.0]);

        // Save → load round-trips every board and the active index.
        let dir = std::env::temp_dir();
        let path = dir.join("wirelab-multiboard-test.wirelab.json");
        p.save(&path).unwrap();
        let q = Project::load(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(q.boards.len(), 2);
        assert_eq!(q.active, 1);
        assert_eq!(q.boards[0].circuit.board_id, "esp32-c5-devkitc-1");
        assert_eq!(q.boards[1].circuit.board_id, "esp32-c3-devkitc-02");
        assert_eq!(q.circuit.board_pos, [12.0, 34.0]);
    }

    #[test]
    fn board_ids_are_stable_and_unique() {
        let mut p = Project::new("lab", "esp32-c5-devkitc-1");
        p.add_board("esp32-c5-devkitc-1");
        p.add_board("esp32-c5-devkitc-1");
        let ids: Vec<u64> = p.boards.iter().map(|b| b.id).collect();
        assert_eq!(ids.len(), 3);
        assert!(ids.iter().all(|&i| i != 0));
        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            3,
            "ids must be unique: {ids:?}"
        );
        // Removing the middle board never renumbers the others.
        p.remove_board(1);
        assert_eq!(p.boards.iter().map(|b| b.id).collect::<Vec<_>>(), vec![ids[0], ids[2]]);
        // Legacy files (id 0) get unique ids assigned on load.
        let json = r#"{"name":"old","circuit":{"board_id":"esp32-c3-devkitc-02",
            "board_pos":[0.0,0.0],"components":{},"wires":{},"next_comp":1,"next_wire":1}}"#;
        let q = Project::from_saved_pub(json);
        assert!(q.boards[0].id != 0);
    }

    #[test]
    fn remove_board_keeps_at_least_one() {
        let mut p = Project::new("lab", "esp32-c5-devkitc-1");
        p.add_board("esp32-c3-devkitc-02");
        p.remove_board(1);
        assert_eq!(p.boards.len(), 1);
        assert_eq!(p.active, 0);
        // Refuses to remove the last board.
        p.remove_board(0);
        assert_eq!(p.boards.len(), 1);
    }
}
