//! AI board-profile importer: manufacturer spec text -> BoardProfile JSON
//! via the Anthropic Messages API (structured outputs).

use std::sync::mpsc::{Receiver, channel};

use serde_json::json;
use wirelab_core::board::BoardProfile;
use wirelab_core::library::lint_board;

pub struct AiImportState {
    pub open: bool,
    pub board_name: String,
    pub spec_text: String,
    pub api_key: String,
    pub busy: bool,
    pub rx: Option<Receiver<Result<BoardProfile, String>>>,
    pub preview: Option<BoardProfile>,
    pub problems: Vec<String>,
    pub error: Option<String>,
}

impl Default for AiImportState {
    fn default() -> Self {
        AiImportState {
            open: false,
            board_name: String::new(),
            spec_text: String::new(),
            api_key: std::env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
            busy: false,
            rx: None,
            preview: None,
            problems: Vec::new(),
            error: None,
        }
    }
}

impl AiImportState {
    pub fn start(&mut self) {
        let (tx, rx) = channel();
        self.rx = Some(rx);
        self.busy = true;
        self.error = None;
        self.preview = None;
        let key = self.api_key.clone();
        let name = self.board_name.clone();
        let spec = self.spec_text.clone();
        std::thread::spawn(move || {
            let _ = tx.send(extract_profile(&key, &name, &spec));
        });
    }

    /// Poll the background request; true when something changed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        match rx.try_recv() {
            Ok(Ok(profile)) => {
                self.problems = lint_board(&profile);
                self.preview = Some(profile);
                self.busy = false;
                self.rx = None;
                true
            }
            Ok(Err(e)) => {
                self.error = Some(e);
                self.busy = false;
                self.rx = None;
                true
            }
            Err(_) => false,
        }
    }
}

fn board_profile_schema() -> serde_json::Value {
    let pin_kind = json!({
        "anyOf": [
            {"type": "string", "enum": ["Gnd", "V3_3", "V5", "En", "NotConnected", "Other"]},
            {
                "type": "object",
                "properties": {"Gpio": {"type": "integer"}},
                "required": ["Gpio"],
                "additionalProperties": false
            }
        ]
    });
    json!({
        "type": "object",
        "properties": {
            "id": {"type": "string", "description": "kebab-case identifier, e.g. esp32-devkit-x"},
            "name": {"type": "string"},
            "chip": {"type": "string", "enum": ["Esp32", "Esp32S2", "Esp32S3", "Esp32C3", "Esp32C6", "Esp32H2", "Other"]},
            "description": {"type": "string"},
            "width_mm": {"type": "number"},
            "height_mm": {"type": "number"},
            "notes": {"type": "array", "items": {"type": "string"}},
            "pins": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "key": {"type": "string"},
                        "label": {"type": "string"},
                        "kind": pin_kind,
                        "side": {"type": "string", "enum": ["Left", "Right", "Top", "Bottom"]},
                        "index": {"type": "integer"},
                        "caps": {"type": "string"},
                        "adc": {"anyOf": [{"type": "null"}, {"type": "array", "items": {"type": "integer"}}]},
                        "warning": {"anyOf": [{"type": "null"}, {"type": "string"}]}
                    },
                    "required": ["key", "label", "kind", "side", "index", "caps", "adc", "warning"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["id", "name", "chip", "description", "width_mm", "height_mm", "notes", "pins"],
        "additionalProperties": false
    })
}

const RULES: &str = r#"You are extracting a WireLab board profile from an ESP32 dev-board specification.
Rules:
- Every physical header pin appears exactly once. "key" is unique: GPIO pins are "GPIOn"; power pins GND1/GND2/3V3/5V/EN. "label" is the silkscreen text.
- "side" is Left/Right/Top/Bottom viewed from above with the USB connector at the bottom; "index" counts from the USB end, 0-based, unique per side.
- "caps" is a pipe-separated flag string using only: DIGITAL_IN DIGITAL_OUT PWM ADC DAC TOUCH INPUT_ONLY STRAPPING FLASH_RESERVED USB_JTAG UART0 RTC. Power pins use "".
- "adc" is [unit, channel] (1 = ADC1, 2 = ADC2) whenever the ADC flag is set, else null.
- Flag every strapping pin STRAPPING, flash pins FLASH_RESERVED, USB D+/D- USB_JTAG, UART0 pins UART0, and give each of those a one-line human "warning". ESP32 classic GPIO34-39 are INPUT_ONLY (no DIGITAL_OUT/PWM).
- If the spec is incomplete, use authoritative knowledge of the chip family and note assumptions in "notes"."#;

fn extract_profile(api_key: &str, name: &str, spec: &str) -> Result<BoardProfile, String> {
    if api_key.trim().is_empty() {
        return Err("no API key: set ANTHROPIC_API_KEY or paste a key".into());
    }
    let body = json!({
        "model": "claude-opus-4-8",
        "max_tokens": 16000,
        "thinking": {"type": "adaptive"},
        "output_config": {"format": {"type": "json_schema", "schema": board_profile_schema()}},
        "messages": [{
            "role": "user",
            "content": format!(
                "{RULES}\n\nBoard name: {name}\n\nSpecification / pinout source material:\n{spec}"
            )
        }]
    });

    let config = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(600)))
        .build();
    let agent: ureq::Agent = config.into();
    let mut response = agent
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key.trim())
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .send_json(&body)
        .map_err(|e| format!("request failed: {e}"))?;
    let value: serde_json::Value = response
        .body_mut()
        .read_json()
        .map_err(|e| format!("bad response: {e}"))?;

    let stop = value["stop_reason"].as_str().unwrap_or("");
    if stop == "refusal" {
        return Err("the model declined this request".into());
    }
    if stop == "max_tokens" {
        return Err("response truncated (max_tokens); trim the spec text".into());
    }
    let text = value["content"]
        .as_array()
        .and_then(|blocks| {
            blocks
                .iter()
                .find(|b| b["type"] == "text")
                .and_then(|b| b["text"].as_str())
        })
        .ok_or_else(|| format!("no text in response: {value}"))?;

    serde_json::from_str::<BoardProfile>(text).map_err(|e| format!("profile does not parse: {e}"))
}

/// Persist an imported profile into the boards library directory.
pub fn save_profile(dir: &std::path::Path, profile: &BoardProfile) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.json", profile.id));
    let text = serde_json::to_string_pretty(profile).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(path)
}
