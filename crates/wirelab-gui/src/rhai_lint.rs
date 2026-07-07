//! Live script diagnostics powered by the vendored rhai-lsp crates (`lsp/`):
//! rhai-rowan for syntax, rhai-hir for semantic checks. The WireLab host API
//! and component names are declared through a generated Rhai definition
//! module so references to them resolve.

use rhai_hir::Hir;
use rhai_hir::error::ErrorKind;
use rhai_rowan::parser::Parser;
use url::Url;

#[allow(unused_imports)]
pub use wirelab_core::script_api::{API_DOCS, ApiDoc, CALLBACK_DOCS};

#[derive(Debug, Clone, PartialEq)]
pub struct LintDiag {
    /// Byte range into the script source.
    pub start: usize,
    pub end: usize,
    /// 1-based position of `start`.
    pub line: usize,
    pub col: usize,
    pub message: String,
}

pub struct Linter {
    hir: Hir,
    script_url: Url,
    defs_url: Url,
    defs_src: String,
}

impl Default for Linter {
    fn default() -> Self {
        Linter::new()
    }
}

impl Linter {
    pub fn new() -> Linter {
        Linter {
            hir: Hir::default(),
            script_url: Url::parse("wirelab:///script.rhai").expect("static url"),
            defs_url: Url::parse("wirelab:///api.d.rhai").expect("static url"),
            defs_src: String::new(),
        }
    }

    /// Declare the host API plus the current component names.
    pub fn set_api(&mut self, comp_names: &[String]) {
        let mut s = String::from(
            "module static;\n\
             fn log(data: ?);\n\
             fn millis() -> int;\n\
             fn after(ms: int, callback: ?);\n\
             fn pin(gpio: int) -> ?;\n\
             fn comp(name: String) -> ?;\n\
             fn chip() -> String;\n\
             fn board_has(what: String) -> bool;\n\
             fn rgb(r: int, g: int, b: int);\n\
             fn uart(tx: int, rx: int, baud: int);\n\
             fn uart_send(data: ?);\n\
             fn send_board(board: ?, text: ?);\n\
             fn http_get(url: ?);\n\
             fn spi_setup(sck: int, mosi: int, miso: int, freq_khz: int);\n\
             fn spi_xfer(cs: int, data: ?);\n\
             fn i2c_setup(sda: int, scl: int, freq_khz: int);\n\
             fn i2c_write(addr: int, data: ?);\n\
             fn i2c_read(addr: int, reg: int, len: int);\n\
             fn lcd_init(sck: int, mosi: int, cs: int, dc: int, rst: int);\n\
             fn lcd_clear(r: int, g: int, b: int);\n\
             fn lcd_rect(x: int, y: int, w: int, h: int, r: int, g: int, b: int);\n\
             fn lcd_text(x: int, y: int, text: String, r: int, g: int, b: int);\n\
             const me: ?;\n",
        );
        for name in comp_names {
            if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                s += &format!("const {name}: ?;\n");
            }
        }
        if s == self.defs_src {
            return;
        }
        self.defs_src = s;
        let parse = Parser::new(&self.defs_src).parse_def();
        self.hir.add_source(&self.defs_url, &parse.clone_syntax());
        self.hir.resolve_all();
    }

    /// All diagnostics for one script: syntax first, semantics when clean.
    pub fn lint(&mut self, src: &str) -> Vec<LintDiag> {
        let mut out = Vec::new();
        let parse = Parser::new(src).parse_script();
        for e in &parse.errors {
            let start = u32::from(e.range.start()) as usize;
            let end = u32::from(e.range.end()) as usize;
            out.push(diag_at(src, start, end, e.kind.to_string()));
        }
        if !out.is_empty() {
            return out;
        }

        self.hir.add_source(&self.script_url, &parse.clone_syntax());
        self.hir.resolve_all();
        let Some(source) = self.hir.source_by_url(&self.script_url) else { return out };
        for err in self.hir.errors_for_source(source) {
            let symbol = match &err.kind {
                ErrorKind::UnresolvedReference { reference_symbol, .. } => *reference_symbol,
                ErrorKind::DuplicateFnParameter { duplicate_symbol, .. } => *duplicate_symbol,
                ErrorKind::UnresolvedImport { import } => *import,
                ErrorKind::NestedFunction { function } => *function,
            };
            let range = self.hir[symbol].selection_or_text_range().unwrap_or_default();
            let start = u32::from(range.start()) as usize;
            let end = u32::from(range.end()) as usize;
            // `this` is bound by the WireLab runtime, not the script scope.
            if src.get(start..end) == Some("this") {
                continue;
            }
            out.push(diag_at(src, start, end, err.kind.to_string()));
        }
        out
    }
}

fn diag_at(src: &str, start: usize, end: usize, message: String) -> LintDiag {
    let clamped = start.min(src.len());
    let line = src[..clamped].bytes().filter(|&b| b == b'\n').count() + 1;
    let col = clamped - src[..clamped].rfind('\n').map(|i| i + 1).unwrap_or(0) + 1;
    LintDiag { start, end: end.max(start + 1), line, col, message }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linted(src: &str) -> Vec<LintDiag> {
        let mut l = Linter::new();
        l.set_api(&["led1".into(), "btn".into()]);
        l.lint(src)
    }

    #[test]
    fn clean_script_has_no_diags() {
        let d = linted(
            "fn on_press() {\n    this.n = (this.n ?? 0) + 1;\n    led1.toggle();\n    \
             after(100, || led1.off());\n    log(millis());\n}\n",
        );
        assert!(d.is_empty(), "unexpected: {d:?}");
    }

    #[test]
    fn syntax_error_is_reported_with_position() {
        let d = linted("fn on_press( {\n}\n");
        assert!(!d.is_empty());
        assert_eq!(d[0].line, 1);
    }

    #[test]
    fn unresolved_reference_is_reported() {
        let d = linted("fn on_press() {\n    nosuch.on();\n}\n");
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].message.contains("cannot resolve"), "{}", d[0].message);
        assert_eq!(d[0].line, 2);
    }

    #[test]
    fn api_and_component_names_resolve() {
        let d = linted(
            "fn on_reading(mv) {\n    if board_has(\"wifi\") { log(chip()); }\n    \
             pin(4).high();\n    btn.is_pressed();\n    me.on();\n    comp(\"led1\").off();\n    \
             rgb(255, 0, 128);\n    pin(28).input_pullup();\n}\n",
        );
        assert!(d.is_empty(), "unexpected: {d:?}");
    }
}
