//! Live script diagnostics: the live engine's own compile check for syntax, the vendored rhai-lsp
//! crates (`lsp/`: rhai-rowan, rhai-hir) for unresolved references, a WireLab API check for
//! component methods, and runtime errors placed at their reported line.

use std::collections::BTreeMap;

use rhai_hir::Hir;
use rhai_hir::error::ErrorKind;
use rhai_rowan::parser::Parser;
use url::Url;
use wirelab_core::script::ScriptHost;

#[allow(unused_imports)]
pub use wirelab_core::script_api::{API_DOCS, ApiDoc, CALLBACK_DOCS};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagKind {
    Syntax,
    Reference,
    Api,
    Runtime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LintDiag {
    /// Byte range into the script source.
    pub start: usize,
    pub end: usize,
    /// 1-based position of `start`.
    pub line: usize,
    pub col: usize,
    pub message: String,
    pub severity: Severity,
    pub kind: DiagKind,
}

/// Methods every component handle has; anything else fails at runtime.
const COMPONENT_METHODS: &[&str] = &[
    "on",
    "off",
    "toggle",
    "blink",
    "breathe",
    "dim",
    "set_angle",
    "beep",
    "tone",
    "act",
    "is_on",
    "is_pressed",
    "millivolts",
];
/// Component methods that read state instead of driving an action.
const QUERIES: &[&str] = &["act", "is_on", "is_pressed", "millivolts"];

/// What a script can address: component names with their part name and supported actions.
#[derive(Debug, Clone, Default)]
pub struct ApiContext {
    pub comps: BTreeMap<String, (String, Vec<String>)>,
    /// The component `me` refers to.
    pub me: Option<String>,
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

    /// rhai-rowan syntax errors, or rhai-hir semantic errors when the syntax is clean.
    pub fn lint(&mut self, src: &str) -> Vec<LintDiag> {
        let mut out = Vec::new();
        let parse = Parser::new(src).parse_script();
        for e in &parse.errors {
            let start = u32::from(e.range.start()) as usize;
            let end = u32::from(e.range.end()) as usize;
            out.push(diag_at(
                src,
                start,
                end,
                e.kind.to_string(),
                Severity::Error,
                DiagKind::Syntax,
            ));
        }
        if !out.is_empty() {
            return out;
        }

        self.hir.add_source(&self.script_url, &parse.clone_syntax());
        self.hir.resolve_all();
        let Some(source) = self.hir.source_by_url(&self.script_url) else {
            return out;
        };
        for err in self.hir.errors_for_source(source) {
            let symbol = match &err.kind {
                ErrorKind::UnresolvedReference {
                    reference_symbol, ..
                } => *reference_symbol,
                ErrorKind::DuplicateFnParameter {
                    duplicate_symbol, ..
                } => *duplicate_symbol,
                ErrorKind::UnresolvedImport { import } => *import,
                ErrorKind::NestedFunction { function } => *function,
            };
            let range = self.hir[symbol]
                .selection_or_text_range()
                .unwrap_or_default();
            let start = u32::from(range.start()) as usize;
            let end = u32::from(range.end()) as usize;
            // `this` is bound by the WireLab runtime, not the script scope.
            if src.get(start..end) == Some("this") {
                continue;
            }
            out.push(diag_at(
                src,
                start,
                end,
                err.kind.to_string(),
                Severity::Error,
                DiagKind::Reference,
            ));
        }
        out
    }

    /// Every diagnostic for one script, sorted by position.
    ///
    /// `runtime` is the live script's last runtime error; pass it only while the editor shows the
    /// source that is running.
    pub fn diagnose(
        &mut self,
        src: &str,
        ctx: &ApiContext,
        host: &ScriptHost,
        runtime: Option<&str>,
    ) -> Vec<LintDiag> {
        let mut out = Vec::new();
        match host.check(src) {
            Err((message, at)) => {
                let start = at.map_or(0, |(line, col)| byte_at(src, line, col));
                out.push(diag_at(
                    src,
                    start,
                    token_end(src, start),
                    message,
                    Severity::Error,
                    DiagKind::Syntax,
                ));
            }
            Ok(()) => {
                let names: Vec<String> = ctx.comps.keys().cloned().collect();
                self.set_api(&names);
                // The engine accepted the syntax; rowan disagreeing is a parser gap, not a problem.
                out.extend(
                    self.lint(src)
                        .into_iter()
                        .filter(|d| d.kind != DiagKind::Syntax),
                );
                out.extend(api_diags(src, ctx));
            }
        }
        if let Some(err) = runtime {
            let start = runtime_position(err).map_or(0, |(line, col)| byte_at(src, line, col));
            out.push(diag_at(
                src,
                start,
                token_end(src, start),
                err.to_string(),
                Severity::Error,
                DiagKind::Runtime,
            ));
        }
        out.sort_by_key(|d| (d.start, d.severity));
        out
    }
}

fn diag_at(
    src: &str,
    start: usize,
    end: usize,
    message: String,
    severity: Severity,
    kind: DiagKind,
) -> LintDiag {
    let clamped = floor_char(src, start.min(src.len()));
    let line = src[..clamped].bytes().filter(|&b| b == b'\n').count() + 1;
    let col = src[..clamped]
        .chars()
        .rev()
        .take_while(|&c| c != '\n')
        .count()
        + 1;
    let end = floor_char(src, end.min(src.len())).max(clamped);
    LintDiag {
        start: clamped,
        end: end.max(clamped + 1).min(src.len().max(clamped + 1)),
        line,
        col,
        message,
        severity,
        kind,
    }
}

fn floor_char(src: &str, mut i: usize) -> usize {
    while i > 0 && !src.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Byte offset of a 1-based (line, column-in-chars) position, clamped to the source.
pub fn byte_at(src: &str, line: usize, col: usize) -> usize {
    let mut offset = 0;
    for (n, text) in src.split_inclusive('\n').enumerate() {
        if n + 1 == line {
            let body = text.trim_end_matches(['\n', '\r']);
            return offset
                + body
                    .char_indices()
                    .nth(col.saturating_sub(1))
                    .map_or(body.len(), |(i, _)| i);
        }
        offset += text.len();
    }
    src.len()
}

/// End of the identifier starting at `start`, or one character on.
fn token_end(src: &str, start: usize) -> usize {
    let rest = &src[start.min(src.len())..];
    let ident: usize = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .map(char::len_utf8)
        .sum();
    start + ident.max(rest.chars().next().map_or(0, char::len_utf8))
}

/// The first `(line N, position M)` a Rhai error message reports.
pub fn runtime_position(message: &str) -> Option<(usize, usize)> {
    let at = message.find("(line ")?;
    let rest = &message[at + 6..];
    let line_end = rest.find(|c: char| !c.is_ascii_digit())?;
    let line = rest[..line_end].parse().ok()?;
    let rest = rest[line_end..].strip_prefix(", position ")?;
    let col_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let col = rest[..col_end].parse().ok()?;
    Some((line, col))
}

/// `receiver.method(` calls outside strings and comments: (receiver, method, method byte range).
fn method_calls(src: &str) -> Vec<(&str, &str, usize, usize)> {
    let b = src.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            q @ (b'"' | b'`' | b'\'') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            c if ident(c) && (i == 0 || !ident(b[i - 1])) => {
                let start = i;
                while i < b.len() && ident(b[i]) {
                    i += 1;
                }
                let chained = start > 0 && b[start - 1] == b'.';
                if !chained && b.get(i) == Some(&b'.') {
                    let m_start = i + 1;
                    let mut j = m_start;
                    while j < b.len() && ident(b[j]) {
                        j += 1;
                    }
                    let mut k = j;
                    while k < b.len() && b[k] == b' ' {
                        k += 1;
                    }
                    if j > m_start && b.get(k) == Some(&b'(') {
                        out.push((&src[start..i], &src[m_start..j], m_start, j));
                    }
                }
            }
            _ => i += 1,
        }
    }
    out
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// Unknown component methods (errors at runtime) and actions the part does not support.
fn api_diags(src: &str, ctx: &ApiContext) -> Vec<LintDiag> {
    let mut out = Vec::new();
    for (recv, method, start, end) in method_calls(src) {
        let target = if recv == "me" {
            ctx.me.as_deref()
        } else {
            Some(recv)
        };
        let Some((part, actions)) = target.and_then(|t| ctx.comps.get(t)) else {
            continue;
        };
        if !COMPONENT_METHODS.contains(&method) {
            let hint = COMPONENT_METHODS
                .iter()
                .map(|m| (edit_distance(method, m), m))
                .filter(|(d, _)| *d <= 2)
                .min()
                .map(|(_, m)| format!(" (did you mean `{m}`?)"))
                .unwrap_or_default();
            let message = format!("`{recv}` has no method `{method}`{hint}");
            out.push(diag_at(
                src,
                start,
                end,
                message,
                Severity::Error,
                DiagKind::Api,
            ));
        } else if !QUERIES.contains(&method) && !actions.iter().any(|a| a == method) {
            let can = if actions.is_empty() {
                "it has no actions".to_string()
            } else {
                format!("it can {}", actions.join(", "))
            };
            let message = format!("{part} `{recv}` doesn't do `.{method}()`; {can}");
            out.push(diag_at(
                src,
                start,
                end,
                message,
                Severity::Warning,
                DiagKind::Api,
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linted(src: &str) -> Vec<LintDiag> {
        let mut l = Linter::new();
        l.set_api(&["led1".into(), "btn".into()]);
        l.lint(src)
    }

    fn ctx() -> ApiContext {
        let mut ctx = ApiContext::default();
        ctx.comps.insert(
            "led1".into(),
            (
                "Red LED".into(),
                vec!["on".into(), "off".into(), "blink".into()],
            ),
        );
        ctx.comps
            .insert("btn".into(), ("Push Button".into(), vec![]));
        ctx.me = Some("btn".into());
        ctx
    }

    fn diagnosed(src: &str, runtime: Option<&str>) -> Vec<LintDiag> {
        Linter::new().diagnose(src, &ctx(), &ScriptHost::new(), runtime)
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

    #[test]
    fn engine_syntax_errors_carry_the_engine_position() {
        let d = diagnosed("fn on_press() {\n    led1.on(;\n}\n", None);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!((d[0].kind, d[0].line), (DiagKind::Syntax, 2));
    }

    #[test]
    fn misspelled_and_unsupported_methods_are_flagged() {
        let src = "fn on_press() {\n    led1.bink(200);\n    led1.set_angle(4);\n    \
                   // led1.nope()\n    log(\"led1.nope()\");\n    me.is_pressed();\n}\n";
        let d = diagnosed(src, None);
        assert_eq!(d.len(), 2, "{d:?}");
        assert_eq!((d[0].severity, d[0].line), (Severity::Error, 2));
        assert!(
            d[0].message.contains("did you mean `blink`"),
            "{}",
            d[0].message
        );
        assert_eq!(&src[d[0].start..d[0].end], "bink");
        assert_eq!((d[1].severity, d[1].line), (Severity::Warning, 3));
        assert!(
            d[1].message.contains("it can on, off, blink"),
            "{}",
            d[1].message
        );
    }

    #[test]
    fn runtime_errors_land_on_their_line() {
        let src = "fn on_press() {\n    let x = 1 / 0;\n}\n";
        let d = diagnosed(
            src,
            Some("on_press: Division by zero: 1 / 0 (line 2, position 15)"),
        );
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!((d[0].kind, d[0].line, d[0].col), (DiagKind::Runtime, 2, 15));
    }

    #[test]
    fn positions_parse_and_map_to_bytes() {
        assert_eq!(runtime_position("x (line 12, position 3)"), Some((12, 3)));
        assert_eq!(runtime_position("no position"), None);
        assert_eq!(byte_at("ab\ncd\n", 2, 2), 4);
        assert_eq!(byte_at("ab\ncd\n", 9, 1), 6);
    }
}
