//! Component library definitions loaded from JSON.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalRole {
    Anode,
    Cathode,
    A,
    B,
    Common,
    NormallyOpen,
    NormallyClosed,
    EndA,
    EndB,
    Wiper,
    Vcc,
    Gnd,
    Signal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalDef {
    /// Unique key within the component, e.g. "anode".
    pub id: String,
    pub name: String,
    pub role: TerminalRole,
}

/// Electrical model used by the simulator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SimModel {
    Led { forward_mv: u16 },
    Resistor { ohms: f32 },
    PushButton,
    ToggleSwitch,
    SlideSwitchSpdt,
    Potentiometer { ohms: f32 },
    Photoresistor { dark_ohms: f32, bright_ohms: f32 },
    Buzzer { active: bool },
    Servo,
    RelayModule,
    /// Powered sensor driving `Signal` high/low from a GUI toggle.
    DigitalSensor,
    /// Powered sensor driving `Signal` with a GUI slider voltage.
    AnalogSensor { min_mv: u16, max_mv: u16 },
    Generic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    Led,
    PushButton,
    ToggleSwitch,
    SlideSwitch,
    Resistor,
    Potentiometer,
    Photoresistor,
    Buzzer,
    Servo,
    Relay,
    SensorModule,
    Chip,
    Generic,
    /// Wire-routing junction: electrically transparent, purely visual.
    Dot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Visual {
    pub shape: Shape,
    /// Base body colour, RGB.
    pub color: [u8; 3],
    pub width_mm: f32,
    pub height_mm: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamDef {
    pub key: String,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
}

/// A verb the rules engine can apply to this component, e.g. "on", "blink".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionDef {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub params: Vec<ParamDef>,
}

/// An event this component can emit, e.g. "pressed".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventDef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentDef {
    /// Stable identifier, e.g. "led-red-5mm".
    pub id: String,
    pub name: String,
    pub category: String,
    #[serde(default)]
    pub description: String,
    pub terminals: Vec<TerminalDef>,
    pub visual: Visual,
    pub sim: SimModel,
    #[serde(default)]
    pub actions: Vec<ActionDef>,
    #[serde(default)]
    pub events: Vec<EventDef>,
    /// Per-instance tweakable properties, e.g. resistance override.
    #[serde(default)]
    pub props: Vec<ParamDef>,
}

impl ComponentDef {
    pub fn terminal(&self, id: &str) -> Option<&TerminalDef> {
        self.terminals.iter().find(|t| t.id == id)
    }

    pub fn terminal_by_role(&self, role: TerminalRole) -> Option<&TerminalDef> {
        self.terminals.iter().find(|t| t.role == role)
    }

    pub fn action(&self, id: &str) -> Option<&ActionDef> {
        self.actions.iter().find(|a| a.id == id)
    }
}

/// Runtime, user-pokeable state of one placed component.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompState {
    None,
    Button { pressed: bool },
    Toggle { on: bool },
    /// 0.0..=1.0 wiper position or sensor level.
    Fraction { value: f32 },
}

impl CompState {
    pub fn initial(model: &SimModel) -> CompState {
        match model {
            SimModel::PushButton => CompState::Button { pressed: false },
            SimModel::ToggleSwitch | SimModel::SlideSwitchSpdt | SimModel::DigitalSensor => {
                CompState::Toggle { on: false }
            }
            SimModel::Potentiometer { .. }
            | SimModel::Photoresistor { .. }
            | SimModel::AnalogSensor { .. } => CompState::Fraction { value: 0.5 },
            _ => CompState::None,
        }
    }
}

/// What the sim reports back for drawing a component "live".
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VisualState {
    Inert,
    /// 0.0..=1.0 emitted light.
    LedBrightness(f32),
    /// Degrees, 0..=180.
    ServoAngle(f32),
    RelayClosed(bool),
    BuzzerOn { freq_hz: f32 },
}

pub type PropMap = BTreeMap<String, f64>;
