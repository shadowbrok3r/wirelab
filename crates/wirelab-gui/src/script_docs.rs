//! Script buffers per component and diagnostics for every scripted component.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use wirelab_core::circuit::{Circuit, CompId};
use wirelab_core::library::Library;
use wirelab_core::script::ScriptHost;

use crate::rhai_lint::{ApiContext, LintDiag, Linter, Severity};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FindKind {
    Next,
    Replace,
    All,
}

/// An open completion popup in the script editor.
pub struct Completion {
    /// (label, insert text, cursor chars back from end, detail).
    pub items: Vec<(String, String, usize, String)>,
    pub selected: usize,
    /// Char index where the word being completed starts.
    pub word_start: usize,
}

/// One component's editor buffer and view state.
pub struct ScriptDoc {
    pub buffer: String,
    pub completion: Option<Completion>,
    /// (cursor char index, lint key) the popup was built for.
    pub completion_key: (usize, u64),
    /// Caret (line, col), 1-based.
    pub cursor: (usize, usize),
    pub find_open: bool,
    pub find: String,
    pub replace: String,
    /// One-shot requests consumed by the editor.
    pub pending_jump: Option<usize>,
    pub pending_find: Option<FindKind>,
    pub pending_snippet: Option<&'static str>,
}

impl ScriptDoc {
    fn new(buffer: String) -> Self {
        ScriptDoc {
            buffer,
            completion: None,
            completion_key: (usize::MAX, 0),
            cursor: (1, 1),
            find_open: false,
            find: String::new(),
            replace: String::new(),
            pending_jump: None,
            pending_find: None,
            pending_snippet: None,
        }
    }
}

pub struct ScriptDocs {
    docs: HashMap<CompId, ScriptDoc>,
    diags: HashMap<CompId, (u64, Vec<LintDiag>)>,
    linter: Linter,
    /// Inline type hints in the editor.
    pub hints_on: bool,
}

impl Default for ScriptDocs {
    fn default() -> Self {
        ScriptDocs {
            docs: HashMap::new(),
            diags: HashMap::new(),
            linter: Linter::new(),
            hints_on: true,
        }
    }
}

fn saved(circuit: &Circuit, id: CompId) -> Option<&str> {
    circuit
        .components
        .get(&id)
        .and_then(|c| c.script.as_deref())
}

impl ScriptDocs {
    /// The editor state for `id`, loaded from its saved script on first use.
    pub fn doc(&mut self, id: CompId, circuit: &Circuit) -> &mut ScriptDoc {
        self.docs
            .entry(id)
            .or_insert_with(|| ScriptDoc::new(saved(circuit, id).unwrap_or_default().to_string()))
    }

    pub fn get(&self, id: CompId) -> Option<&ScriptDoc> {
        self.docs.get(&id)
    }

    /// The buffer differs from the script the circuit holds.
    pub fn is_dirty(&self, id: CompId, circuit: &Circuit) -> bool {
        self.docs
            .get(&id)
            .is_some_and(|d| match saved(circuit, id) {
                Some(s) => s != d.buffer,
                None => !d.buffer.is_empty(),
            })
    }

    pub fn revert(&mut self, id: CompId, circuit: &Circuit) {
        if let Some(d) = self.docs.get_mut(&id) {
            d.buffer = saved(circuit, id).unwrap_or_default().to_string();
            d.completion = None;
        }
    }

    /// Replaces the buffer, e.g. after attaching a template.
    pub fn set_buffer(&mut self, id: CompId, circuit: &Circuit, text: String) {
        self.doc(id, circuit).buffer = text;
    }

    /// Forgets every buffer, for a project or board switch.
    pub fn clear(&mut self) {
        self.docs.clear();
        self.diags.clear();
    }

    pub fn diags(&self, id: CompId) -> &[LintDiag] {
        self.diags.get(&id).map_or(&[], |(_, d)| d.as_slice())
    }

    /// Cache key of the diagnostics shown for `id`, for completion bookkeeping.
    pub fn diag_key(&self, id: CompId) -> u64 {
        self.diags.get(&id).map_or(0, |(k, _)| *k)
    }

    /// Errors and warnings over every script.
    pub fn counts(&self) -> (usize, usize) {
        self.diags
            .values()
            .flat_map(|(_, d)| d)
            .fold((0, 0), |(e, w), d| match d.severity {
                Severity::Error => (e + 1, w),
                Severity::Warning => (e, w + 1),
            })
    }

    /// Components with diagnostics, worst first.
    pub fn with_problems(&self) -> Vec<CompId> {
        let mut ids: Vec<CompId> = self
            .diags
            .iter()
            .filter(|(_, (_, d))| !d.is_empty())
            .map(|(id, _)| *id)
            .collect();
        ids.sort_by_key(|id| (self.diags(*id).iter().map(|d| d.severity).min(), *id));
        ids
    }

    /// Re-lints every scripted component whose source, circuit names or runtime error changed.
    pub fn refresh(&mut self, circuit: &Circuit, lib: &Library, host: &ScriptHost) {
        let (names, mut ctx) = api_context(circuit, lib);
        let mut names_hash = std::hash::DefaultHasher::new();
        for (name, (part, actions)) in &ctx.comps {
            (name, part, actions).hash(&mut names_hash);
        }
        let names_hash = names_hash.finish();

        let mut live = Vec::new();
        for comp in circuit.components.values() {
            let dirty = self.is_dirty(comp.id, circuit);
            let src = match (self.docs.get(&comp.id), &comp.script) {
                (Some(d), _) if dirty || comp.script.is_some() => d.buffer.as_str(),
                (_, Some(s)) => s.as_str(),
                _ => continue,
            };
            if src.trim().is_empty() {
                continue;
            }
            live.push(comp.id);
            // A runtime error only belongs to the source that is actually running.
            let runtime = (!dirty)
                .then(|| host.errors.get(&comp.id))
                .flatten()
                .map(String::as_str);
            let mut h = std::hash::DefaultHasher::new();
            (src, names_hash, runtime).hash(&mut h);
            let key = h.finish();
            if self.diags.get(&comp.id).is_some_and(|(k, _)| *k == key) {
                continue;
            }
            ctx.me = names.get(&comp.id).cloned();
            let diags = self.linter.diagnose(src, &ctx, host, runtime);
            self.diags.insert(comp.id, (key, diags));
        }
        self.diags.retain(|id, _| live.contains(id));
    }

    /// Lints arbitrary source against the current circuit, for tools that do not edit a document.
    pub fn diagnose_source(
        &mut self,
        src: &str,
        circuit: &Circuit,
        lib: &Library,
        host: &ScriptHost,
        me: Option<CompId>,
    ) -> Vec<LintDiag> {
        let (names, mut ctx) = api_context(circuit, lib);
        ctx.me = me.and_then(|id| names.get(&id).cloned());
        self.linter.diagnose(src, &ctx, host, None)
    }
}

/// Script names per component and what each name can do.
fn api_context(circuit: &Circuit, lib: &Library) -> (HashMap<CompId, String>, ApiContext) {
    let names = wirelab_core::script::component_names(circuit, lib);
    let mut ctx = ApiContext::default();
    for (id, name) in &names {
        if let Some(def) = circuit
            .components
            .get(id)
            .and_then(|c| lib.component(&c.def_id))
        {
            ctx.comps.insert(
                name.clone(),
                (
                    def.name.clone(),
                    def.actions.iter().map(|a| a.id.clone()).collect(),
                ),
            );
        }
    }
    (names, ctx)
}
