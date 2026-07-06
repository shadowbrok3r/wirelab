//! On-disk libraries of board profiles and component definitions.

use std::collections::BTreeMap;
use std::path::Path;

use crate::board::BoardProfile;
use crate::component::ComponentDef;

#[derive(Debug, Default, Clone)]
pub struct Library {
    pub boards: BTreeMap<String, BoardProfile>,
    pub components: BTreeMap<String, ComponentDef>,
}

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("io error reading {path}: {source}")]
    Io { path: String, source: std::io::Error },
    #[error("bad json in {path}: {source}")]
    Json { path: String, source: serde_json::Error },
    #[error("duplicate id {id} in {path}")]
    Duplicate { id: String, path: String },
}

impl Library {
    pub fn board(&self, id: &str) -> Option<&BoardProfile> {
        self.boards.get(id)
    }

    pub fn component(&self, id: &str) -> Option<&ComponentDef> {
        self.components.get(id)
    }

    pub fn add_board(&mut self, board: BoardProfile) {
        self.boards.insert(board.id.clone(), board);
    }

    pub fn add_component(&mut self, def: ComponentDef) {
        self.components.insert(def.id.clone(), def);
    }

    /// Load every `*.json` under `boards_dir` and `components_dir`.
    pub fn load(boards_dir: &Path, components_dir: &Path) -> Result<Library, LibraryError> {
        let mut lib = Library::default();
        for path in json_files(boards_dir)? {
            let board: BoardProfile = read_json(&path)?;
            if lib.boards.contains_key(&board.id) {
                return Err(LibraryError::Duplicate {
                    id: board.id,
                    path: path.display().to_string(),
                });
            }
            lib.add_board(board);
        }
        for path in json_files(components_dir)? {
            let def: ComponentDef = read_json(&path)?;
            if lib.components.contains_key(&def.id) {
                return Err(LibraryError::Duplicate {
                    id: def.id,
                    path: path.display().to_string(),
                });
            }
            lib.add_component(def);
        }
        Ok(lib)
    }
}

/// Semantic checks for a board profile; returns human-readable problems.
pub fn lint_board(b: &crate::board::BoardProfile) -> Vec<String> {
    use crate::board::{PinCaps, PinKind};
    use wirelab_proto::ChipKind;
    let mut problems = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut seen_gpio = std::collections::HashSet::new();
    let mut seen_slot = std::collections::HashSet::new();
    for pin in &b.pins {
        if !seen.insert(&pin.key) {
            problems.push(format!("duplicate pin key {}", pin.key));
        }
        if !seen_slot.insert((pin.side, pin.index)) {
            problems.push(format!("{}: duplicate position {:?}/{}", pin.key, pin.side, pin.index));
        }
        if let PinKind::Gpio(g) = pin.kind {
            if !seen_gpio.insert(g) {
                problems.push(format!("GPIO{g} appears on more than one pin"));
            }
            if g > 48 {
                problems.push(format!("{}: GPIO{g} out of range", pin.key));
            }
            if !pin.caps.intersects(PinCaps::DIGITAL_IN | PinCaps::DIGITAL_OUT) {
                problems.push(format!("{}: GPIO with no digital capability", pin.key));
            }
            if pin.caps.contains(PinCaps::ADC) && pin.adc.is_none() {
                problems.push(format!("{}: ADC capability without adc unit/channel", pin.key));
            }
            if b.chip == ChipKind::Esp32
                && (34..=39).contains(&g)
                && !pin.caps.contains(PinCaps::INPUT_ONLY)
            {
                problems.push(format!("{}: ESP32 GPIO{g} must be INPUT_ONLY", pin.key));
            }
        }
    }
    if !b.pins.iter().any(|p| p.kind == PinKind::Gnd) {
        problems.push("no GND pin".to_string());
    }
    if !b.pins.iter().any(|p| p.kind == PinKind::V3_3) {
        problems.push("no 3V3 pin".to_string());
    }
    problems
}

/// Semantic checks for a component definition.
pub fn lint_component(c: &crate::component::ComponentDef) -> Vec<String> {
    use crate::component::{SimModel, TerminalRole};
    let mut problems = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for t in &c.terminals {
        if !seen.insert(&t.id) {
            problems.push(format!("duplicate terminal id {}", t.id));
        }
    }
    let mut action_ids = std::collections::HashSet::new();
    for a in &c.actions {
        if !action_ids.insert(&a.id) {
            problems.push(format!("duplicate action id {}", a.id));
        }
    }
    let need: &[TerminalRole] = match &c.sim {
        SimModel::Led { .. } => &[TerminalRole::Anode, TerminalRole::Cathode],
        SimModel::Resistor { .. } | SimModel::Photoresistor { .. } => {
            &[TerminalRole::A, TerminalRole::B]
        }
        SimModel::PushButton | SimModel::ToggleSwitch => &[TerminalRole::A, TerminalRole::B],
        SimModel::SlideSwitchSpdt => &[TerminalRole::Common, TerminalRole::A, TerminalRole::B],
        SimModel::Potentiometer { .. } => {
            &[TerminalRole::EndA, TerminalRole::EndB, TerminalRole::Wiper]
        }
        SimModel::Buzzer { .. } => &[TerminalRole::Signal, TerminalRole::Gnd],
        SimModel::Servo => &[TerminalRole::Vcc, TerminalRole::Gnd, TerminalRole::Signal],
        SimModel::RelayModule => &[
            TerminalRole::Vcc,
            TerminalRole::Gnd,
            TerminalRole::Signal,
            TerminalRole::Common,
            TerminalRole::NormallyOpen,
            TerminalRole::NormallyClosed,
        ],
        SimModel::DigitalSensor | SimModel::AnalogSensor { .. } => {
            &[TerminalRole::Vcc, TerminalRole::Gnd, TerminalRole::Signal]
        }
        SimModel::Generic => &[],
    };
    for role in need {
        if c.terminal_by_role(*role).is_none() {
            problems.push(format!("sim model {:?} needs a {:?} terminal", c.sim, role));
        }
    }
    problems
}

fn json_files(dir: &Path) -> Result<Vec<std::path::PathBuf>, LibraryError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(dir)
        .map_err(|e| LibraryError::Io { path: dir.display().to_string(), source: e })?;
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    Ok(files)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, LibraryError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| LibraryError::Io { path: path.display().to_string(), source: e })?;
    serde_json::from_str(&text)
        .map_err(|e| LibraryError::Json { path: path.display().to_string(), source: e })
}
