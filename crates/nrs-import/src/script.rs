//! `<script>` execution (Cargo feature `scripting`) behind a JS layer
//! that mirrors what nrsml scripts see upstream, via a pure-Rust JS
//! engine (`boa_engine`) so no C toolchain / V8 dependency is needed.
//!
//! `nrsml_runtime.js` ports the relevant runtime verbatim: nrs-lib-ts's
//! matrix/vector classes and `ContextAPI`, and nrsml's scope chain
//! (`document` -> `entry` -> `impact`/`relation`, each with `type`,
//! `parent`, `document`, `entry`, `root`, `value` and a `local` copied
//! from its parent when it opens), `executeScript`
//! (`new Function("global","local","scope", code).apply(scope, ...)`
//! then `local[entryPoint]()`), `<contributor>` handling and
//! `acceptImpact`'s fallback to the enclosing entry. The parser drives
//! that runtime in document order, so a script observes the same state
//! it would upstream: `<contributor>` tags and `<script>`s interleave,
//! entry-level `local` reaches the impacts that follow it, and a
//! document-level impact sees `scope.entry === undefined`.
//!
//! Without the `scripting` feature the same contributor semantics run
//! in Rust and every `<script>` is reported as skipped.
//!
//! Contributor weights come back to Rust as JSON. Only scalar weights
//! are representable in `ParsedImpact.contributors`; a script that sets
//! a diagonal/regular matrix (no real script does) is warned about and
//! that contributor dropped.

#[cfg(not(feature = "scripting"))]
use crate::xml::parse_fraction;
use crate::xml::transform_id;
use crate::xtree::XNode;

/// A scope handle. The document scope is `ScriptEngine::ROOT`; every
/// other scope is opened (and later closed) by the parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeId(usize);

pub struct ScriptEngine {
    #[cfg(feature = "scripting")]
    engine: engine::Engine,
    /// The enclosing entry id for each open scope, by handle: what
    /// `$` placeholders expand to, and what an impact or relation with
    /// no contributor falls back to.
    entry_of: Vec<Option<String>>,
}

impl ScriptEngine {
    pub const ROOT: ScopeId = ScopeId(0);

    pub fn new() -> Self {
        ScriptEngine {
            #[cfg(feature = "scripting")]
            engine: engine::Engine::new(),
            entry_of: vec![None],
        }
    }

    fn open(&mut self, entry: Option<String>) -> ScopeId {
        self.entry_of.push(entry);
        ScopeId(self.entry_of.len() - 1)
    }

    /// The id of the entry enclosing `scope` (itself, for an entry).
    pub fn entry_id(&self, scope: ScopeId) -> Option<&str> {
        self.entry_of[scope.0].as_deref()
    }

    fn context(&self, scope: ScopeId) -> String {
        match self.entry_id(scope) {
            Some(id) => format!("entry {id}"),
            None => "document".to_string(),
        }
    }

    /// nrsml's `processEntry`: a child scope of `parent` whose
    /// `value` is the entry and whose `entry` is itself.
    pub fn open_entry(
        &mut self,
        parent: ScopeId,
        id: &str,
        title: &str,
        entry_type: Option<&str>,
    ) -> ScopeId {
        let scope = self.open(Some(id.to_string()));
        #[cfg(feature = "scripting")]
        self.engine.open_entry(scope, parent, id, title, entry_type);
        #[cfg(not(feature = "scripting"))]
        let _ = (parent, title, entry_type);
        scope
    }

    pub fn close_entry(&mut self, scope: ScopeId) {
        #[cfg(feature = "scripting")]
        self.engine.close(scope);
        let _ = scope;
    }

    /// Run one `<script>` element in `scope` (a document or entry).
    pub fn run_script(&mut self, scope: ScopeId, script: &XNode, warnings: &mut Vec<String>) {
        let context = self.context(scope);
        #[cfg(feature = "scripting")]
        if let Some(call) = resolve_script(script, warnings) {
            if let Err(e) = self.engine.run_script(scope, &call) {
                warnings.push(format!("{context}: <script> failed: {e}"));
            }
        }
        #[cfg(not(feature = "scripting"))]
        skipped_script(script, &context, warnings);
    }

    /// The contributors of each of the `count` impacts one impact tag
    /// builds (nrsml's `processImpactBase` + `acceptImpact`), running
    /// its `<contributor>` and `<script>` children in document order.
    /// The impacts share one contributor set, as upstream; left empty,
    /// it falls back to the enclosing entry (at document level there is
    /// none, so it stays empty).
    pub fn impact_contributors(
        &mut self,
        parent: ScopeId,
        node: &XNode,
        count: usize,
        warnings: &mut Vec<String>,
    ) -> Vec<Vec<(String, f32)>> {
        let entry = self.entry_id(parent).map(String::from);
        let context = self.context(parent);

        #[cfg(feature = "scripting")]
        {
            let scope = self.open(entry.clone());
            self.engine.open_impact(scope, parent, count);
            for child in node.children() {
                match child.tag_name() {
                    "contributor" => {
                        let Some(raw) = child.attribute("id") else {
                            continue;
                        };
                        let id = transform_id(raw, entry.as_deref());
                        if let Err(e) =
                            self.engine
                                .impact_contributor(scope, &id, child.attribute("factor"))
                        {
                            warnings.push(format!("{context}: <contributor id=\"{id}\">: {e}"));
                        }
                    }
                    "script" => {
                        if let Some(call) = resolve_script(child, warnings) {
                            if let Err(e) = self.engine.run_script(scope, &call) {
                                warnings.push(format!("{context}: <script> failed: {e}"));
                            }
                        }
                    }
                    _ => {}
                }
            }
            self.engine.close_items(scope, &context, warnings)
        }

        #[cfg(not(feature = "scripting"))]
        {
            let mut first: Vec<(String, f32)> = Vec::new();
            for child in node.children() {
                match child.tag_name() {
                    "contributor" => {
                        let Some(raw) = child.attribute("id") else {
                            continue;
                        };
                        let id = transform_id(raw, entry.as_deref());
                        let factor = child.attribute("factor").map(parse_fraction).unwrap_or(1.0);
                        set_contributor(&mut first, id, factor.min(1.0));
                    }
                    "script" => skipped_script(child, &context, warnings),
                    _ => {}
                }
            }
            if first.is_empty() {
                if let Some(entry) = entry {
                    first.push((entry, 1.0));
                }
            }
            // The paired shorthands share one contributors Map upstream.
            vec![first; count]
        }
    }

    /// A relation tag's contributors (nrsml's `processRelationBase`),
    /// falling back to the enclosing entry like an impact's.
    pub fn relation_contributors(
        &mut self,
        parent: ScopeId,
        node: &XNode,
        warnings: &mut Vec<String>,
    ) -> Vec<(String, f32)> {
        let entry = self.entry_id(parent).map(String::from);
        let context = self.context(parent);

        #[cfg(feature = "scripting")]
        {
            let scope = self.open(entry.clone());
            self.engine.open_relation(scope, parent);
            for child in node.children() {
                match child.tag_name() {
                    "contributor" => {
                        let Some(raw) = child.attribute("id") else {
                            continue;
                        };
                        let id = transform_id(raw, entry.as_deref());
                        let factor = child.attribute("factor").unwrap_or("");
                        if let Err(e) = self.engine.relation_contributor(scope, &id, factor) {
                            warnings.push(format!("{context}: <contributor id=\"{id}\">: {e}"));
                        }
                    }
                    "script" => {
                        if let Some(call) = resolve_script(child, warnings) {
                            if let Err(e) = self.engine.run_script(scope, &call) {
                                warnings.push(format!("{context}: <script> failed: {e}"));
                            }
                        }
                    }
                    _ => {}
                }
            }
            self.engine
                .close_items(scope, &context, warnings)
                .into_iter()
                .next()
                .unwrap_or_default()
        }

        #[cfg(not(feature = "scripting"))]
        {
            let mut contributors: Vec<(String, f32)> = Vec::new();
            for child in node.children() {
                match child.tag_name() {
                    "contributor" => {
                        let Some(raw) = child.attribute("id") else {
                            continue;
                        };
                        let id = transform_id(raw, entry.as_deref());
                        match child.attribute("factor").and_then(|f| f.trim().parse::<f32>().ok()) {
                            Some(factor) => set_contributor(&mut contributors, id, factor.min(1.0)),
                            None => warnings.push(format!(
                                "{context}: relation <contributor id=\"{id}\"> has no numeric factor, dropped"
                            )),
                        }
                    }
                    "script" => skipped_script(child, &context, warnings),
                    _ => {}
                }
            }
            if contributors.is_empty() {
                if let Some(entry) = entry {
                    contributors.push((entry, 1.0));
                }
            }
            contributors
        }
    }
}

impl Default for ScriptEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Upstream's `Map.set`: a repeated id keeps its first position and
/// takes the new factor.
#[cfg(not(feature = "scripting"))]
fn set_contributor(contributors: &mut Vec<(String, f32)>, id: String, factor: f32) {
    match contributors
        .iter_mut()
        .find(|(existing, _)| *existing == id)
    {
        Some(slot) => slot.1 = factor,
        None => contributors.push((id, factor)),
    }
}

#[cfg(not(feature = "scripting"))]
fn skipped_script(_script: &XNode, context: &str, warnings: &mut Vec<String>) {
    warnings.push(format!(
        "{context}: <script> skipped (build with the \"scripting\" feature to run it)"
    ));
}

/// One `<script>` element, resolved to executable source (inline text,
/// or the contents of `src=`) plus its `entryPoint`, if any.
#[cfg(feature = "scripting")]
struct ScriptCall {
    source: String,
    entry_point: Option<String>,
}

#[cfg(feature = "scripting")]
fn resolve_script(node: &XNode, warnings: &mut Vec<String>) -> Option<ScriptCall> {
    let entry_point = node.attribute("entryPoint").map(String::from);
    let source = match node.attribute("src") {
        Some(src) => {
            let path = node.source_dir().join(src);
            match std::fs::read_to_string(&path) {
                Ok(source) => source,
                Err(e) => {
                    warnings.push(format!("<script src=\"{src}\">: {}: {e}", path.display()));
                    return None;
                }
            }
        }
        None => node.text().to_string(),
    };
    Some(ScriptCall {
        source,
        entry_point,
    })
}

#[cfg(feature = "scripting")]
mod engine {
    use super::{ScopeId, ScriptCall};
    use boa_engine::{Context, Source};

    const RUNTIME: &str = include_str!("nrsml_runtime.js");

    /// One `boa_engine::Context` for the whole parse, so `global` and
    /// the scope chain persist across calls. Rust talks to the runtime
    /// only through `Context::eval` of small `__nrs.*(...)` calls with
    /// JSON-encoded arguments.
    pub struct Engine {
        context: Context,
    }

    /// A JSON literal is a valid JS literal for strings and `null`.
    fn js<T: Into<serde_json::Value>>(value: T) -> String {
        value.into().to_string()
    }

    impl Engine {
        pub fn new() -> Self {
            let mut context = Context::default();
            context
                .eval(Source::from_bytes(RUNTIME))
                .expect("nrsml_runtime.js is valid JS");
            Engine { context }
        }

        fn call(&mut self, expression: &str) -> Result<String, String> {
            let result = self
                .context
                .eval(Source::from_bytes(expression))
                .map_err(|e| e.to_string())?;
            result
                .to_string(&mut self.context)
                .map(|s| s.to_std_string_escaped())
                .map_err(|e| e.to_string())
        }

        /// Opens a scope on the JS side and checks its handle matches
        /// the Rust-side one (both sides allocate handles in lockstep).
        fn open(&mut self, expected: ScopeId, expression: &str) {
            let handle = self.call(expression).expect("opening a scope cannot throw");
            debug_assert_eq!(handle, expected.0.to_string(), "scope handles out of sync");
        }

        pub fn open_entry(
            &mut self,
            scope: ScopeId,
            parent: ScopeId,
            id: &str,
            title: &str,
            entry_type: Option<&str>,
        ) {
            self.open(
                scope,
                &format!(
                    "__nrs.openEntry({}, {}, {}, {})",
                    parent.0,
                    js(id),
                    js(title),
                    js(entry_type)
                ),
            );
        }

        pub fn open_impact(&mut self, scope: ScopeId, parent: ScopeId, count: usize) {
            self.open(scope, &format!("__nrs.openImpact({}, {count})", parent.0));
        }

        pub fn open_relation(&mut self, scope: ScopeId, parent: ScopeId) {
            self.open(scope, &format!("__nrs.openRelation({})", parent.0));
        }

        pub fn run_script(&mut self, scope: ScopeId, call: &ScriptCall) -> Result<(), String> {
            self.call(&format!(
                "__nrs.runScript({}, {}, {})",
                scope.0,
                js(call.source.as_str()),
                js(call.entry_point.as_deref())
            ))
            .map(|_| ())
        }

        pub fn impact_contributor(
            &mut self,
            scope: ScopeId,
            id: &str,
            factor: Option<&str>,
        ) -> Result<(), String> {
            self.call(&format!(
                "__nrs.impactContributor({}, {}, {})",
                scope.0,
                js(id),
                js(factor)
            ))
            .map(|_| ())
        }

        pub fn relation_contributor(
            &mut self,
            scope: ScopeId,
            id: &str,
            factor: &str,
        ) -> Result<(), String> {
            self.call(&format!(
                "__nrs.relationContributor({}, {}, {})",
                scope.0,
                js(id),
                js(factor)
            ))
            .map(|_| ())
        }

        pub fn close(&mut self, scope: ScopeId) {
            self.call(&format!("__nrs.close({})", scope.0))
                .expect("closing a scope cannot throw");
        }

        /// Closes an impact/relation scope and reads back each item's
        /// contributors, keeping only finite scalar weights.
        pub fn close_items(
            &mut self,
            scope: ScopeId,
            context: &str,
            warnings: &mut Vec<String>,
        ) -> Vec<Vec<(String, f32)>> {
            let json = self
                .call(&format!("__nrs.close({})", scope.0))
                .expect("closing a scope cannot throw");
            let items: Vec<Vec<(String, serde_json::Value)>> =
                serde_json::from_str(&json).expect("close returns contributor JSON");
            items
                .into_iter()
                .map(|item| {
                    item.into_iter()
                        .filter_map(|(id, matrix)| {
                            let kind = matrix.get("kind").and_then(|k| k.as_str());
                            let value = matrix.get("value").and_then(|v| v.as_f64());
                            match (kind, value) {
                                (Some("scalar"), Some(v)) if v.is_finite() => Some((id, v as f32)),
                                (Some("scalar"), _) => {
                                    warnings.push(format!(
                                        "{context}: contributor '{id}' has a non-numeric factor, dropped"
                                    ));
                                    None
                                }
                                (other, _) => {
                                    warnings.push(format!(
                                        "{context}: contributor '{id}' is a {} matrix, not representable, dropped",
                                        other.unwrap_or("unknown")
                                    ));
                                    None
                                }
                            }
                        })
                        .collect()
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xtree::{arc_dir, from_roxmltree};
    use std::path::Path;

    fn parse_one(xml: &str) -> XNode {
        let doc = roxmltree::Document::parse(xml).unwrap();
        let dir = arc_dir(Path::new("test.xml"));
        from_roxmltree(doc.root_element(), &dir)
    }

    #[test]
    fn impact_without_contributors_falls_back_to_its_entry() {
        let mut engine = ScriptEngine::new();
        let entry = engine.open_entry(ScriptEngine::ROOT, "A-1", "", None);
        let mut warnings = Vec::new();
        let result = engine.impact_contributors(entry, &parse_one("<aei />"), 2, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(result, vec![vec![("A-1".to_string(), 1.0)]; 2]);
    }

    /// A paired shorthand's impacts share one contributors Map upstream.
    #[test]
    fn paired_impacts_share_contributors() {
        let mut engine = ScriptEngine::new();
        let entry = engine.open_entry(ScriptEngine::ROOT, "A-1", "", None);
        let node = parse_one(r#"<maxAEIPADS><contributor id="B-1" factor="0.5" /></maxAEIPADS>"#);
        let mut warnings = Vec::new();
        let result = engine.impact_contributors(entry, &node, 2, &mut warnings);
        assert_eq!(result, vec![vec![("B-1".to_string(), 0.5)]; 2]);
    }

    #[test]
    fn document_level_impact_has_no_fallback() {
        let mut engine = ScriptEngine::new();
        let mut warnings = Vec::new();
        let result = engine.impact_contributors(
            ScriptEngine::ROOT,
            &parse_one("<meme />"),
            1,
            &mut warnings,
        );
        assert_eq!(result, vec![Vec::new()]);
    }

    #[test]
    fn repeated_contributor_keeps_the_last_factor_clamped() {
        let mut engine = ScriptEngine::new();
        let entry = engine.open_entry(ScriptEngine::ROOT, "A-1", "", None);
        let node = parse_one(
            r#"<aei><contributor id="B-1" factor="0.25" /><contributor id="C-1" /><contributor id="B-1" factor="1.5" /></aei>"#,
        );
        let mut warnings = Vec::new();
        let result = engine.impact_contributors(entry, &node, 1, &mut warnings);
        assert_eq!(
            result,
            vec![vec![("B-1".to_string(), 1.0), ("C-1".to_string(), 1.0)]]
        );
    }

    #[cfg(feature = "scripting")]
    mod scripting {
        use super::*;

        /// The exact Koikake.xml pattern: split one impact's
        /// contribution between the entry itself and a fixed external id.
        #[test]
        fn impact_script_sets_scalar_contributors() {
            let node = parse_one(
                r#"<aei>
                    <script>
                        this.value[0].contributors.set(this.parent.entry.value.id, this.root.context.api.newScalarMatrix(0.7));
                        this.value[0].contributors.set("V-VNDB-17516", this.root.context.api.newScalarMatrix(1.0 - 0.7));
                    </script>
                </aei>"#,
            );
            let mut engine = ScriptEngine::new();
            let entry = engine.open_entry(ScriptEngine::ROOT, "A-MAL-4181", "", None);
            let mut warnings = Vec::new();
            let result = engine.impact_contributors(entry, &node, 1, &mut warnings);
            assert!(warnings.is_empty(), "{warnings:?}");
            let by_id: std::collections::BTreeMap<_, _> = result[0].iter().cloned().collect();
            assert!((by_id["A-MAL-4181"] - 0.7).abs() < 1e-5);
            assert!((by_id["V-VNDB-17516"] - 0.3).abs() < 1e-5);
        }

        /// The RSTSB69Music.js pattern, reduced: an entry-level script
        /// registers its entry into `global`; a later document-level
        /// meme reads `global` back and sets contributors from it.
        #[test]
        fn global_state_persists_across_scopes() {
            let register = parse_one(
                r#"<script>
                    global.tracks ??= [];
                    global.tracks.push(scope.entry.value.id);
                </script>"#,
            );
            let meme = parse_one(
                r#"<meme>
                    <script>
                        for (const id of global.tracks) {
                            scope.value[0].contributors.set(id, scope.root.context.api.newScalarMatrix(1.0));
                        }
                    </script>
                </meme>"#,
            );
            let mut engine = ScriptEngine::new();
            let mut warnings = Vec::new();
            let track = engine.open_entry(ScriptEngine::ROOT, "M-TRACK-1", "", None);
            engine.run_script(track, &register, &mut warnings);
            engine.close_entry(track);
            let result = engine.impact_contributors(ScriptEngine::ROOT, &meme, 1, &mut warnings);
            assert!(warnings.is_empty(), "{warnings:?}");
            assert_eq!(result, vec![vec![("M-TRACK-1".to_string(), 1.0)]]);
        }

        /// `local` set by an entry-level script is copied into the
        /// scopes opened after it; a document-level scope has no entry.
        #[test]
        fn local_flows_down_and_entry_is_undefined_at_document_level() {
            let mut engine = ScriptEngine::new();
            let mut warnings = Vec::new();
            let entry = engine.open_entry(ScriptEngine::ROOT, "A-1", "", None);
            engine.run_script(
                entry,
                &parse_one("<script>local.share = 0.4;</script>"),
                &mut warnings,
            );
            let node = parse_one(
                r#"<aei><script>
                    scope.value[0].contributors.set(scope.entry.value.id, scope.root.context.api.newScalarMatrix(local.share));
                </script></aei>"#,
            );
            let result = engine.impact_contributors(entry, &node, 1, &mut warnings);
            assert_eq!(result, vec![vec![("A-1".to_string(), 0.4)]]);

            let node = parse_one(
                r#"<meme><contributor id="A-1" /><script>
                    if (scope.entry !== undefined || scope.parent.type !== "document") throw new Error("wrong scope");
                </script></meme>"#,
            );
            let result = engine.impact_contributors(ScriptEngine::ROOT, &node, 1, &mut warnings);
            assert!(warnings.is_empty(), "{warnings:?}");
            assert_eq!(result, vec![vec![("A-1".to_string(), 1.0)]]);
        }

        /// Matrices are the real nrs-lib-ts classes, methods included.
        #[test]
        fn matrix_api_matches_upstream() {
            let node = parse_one(
                r#"<aei><script>
                    const api = scope.root.context.api;
                    const m = api.newScalarMatrix(0.5).add(api.newScalarMatrix(0.75));
                    m.clamp01();
                    scope.value[0].contributors.set("X", m.matmul(api.newScalarMatrix(0.5)));
                </script></aei>"#,
            );
            let mut engine = ScriptEngine::new();
            let mut warnings = Vec::new();
            let result = engine.impact_contributors(ScriptEngine::ROOT, &node, 1, &mut warnings);
            assert!(warnings.is_empty(), "{warnings:?}");
            assert_eq!(result, vec![vec![("X".to_string(), 0.5)]]);
        }

        #[test]
        fn contributor_factor_is_a_js_expression() {
            let node = parse_one(r#"<aei><contributor id="X" factor="1/4" /></aei>"#);
            let mut engine = ScriptEngine::new();
            let mut warnings = Vec::new();
            let result = engine.impact_contributors(ScriptEngine::ROOT, &node, 1, &mut warnings);
            assert_eq!(result, vec![vec![("X".to_string(), 0.25)]]);
        }

        #[test]
        fn relation_scripts_set_contributors() {
            let node = parse_one(
                r#"<killedBy id="K"><script>
                    scope.value[0].contributors.set("A-1", scope.root.context.api.newScalarMatrix(0.3));
                </script></killedBy>"#,
            );
            let mut engine = ScriptEngine::new();
            let mut warnings = Vec::new();
            let result = engine.relation_contributors(ScriptEngine::ROOT, &node, &mut warnings);
            assert!(warnings.is_empty(), "{warnings:?}");
            assert_eq!(result, vec![("A-1".to_string(), 0.3)]);
        }

        #[test]
        fn non_scalar_contributor_matrix_warns_and_is_dropped() {
            let node = parse_one(
                r#"<aei><script>
                    scope.value[0].contributors.set("X", scope.root.context.api.newDiagonalMatrix([1,0,0]));
                </script></aei>"#,
            );
            let mut engine = ScriptEngine::new();
            let mut warnings = Vec::new();
            let result = engine.impact_contributors(ScriptEngine::ROOT, &node, 1, &mut warnings);
            assert_eq!(result, vec![Vec::new()]);
            assert!(warnings.iter().any(|w| w.contains("not representable")));
        }

        #[test]
        fn script_errors_become_warnings() {
            let mut engine = ScriptEngine::new();
            let mut warnings = Vec::new();
            engine.run_script(
                ScriptEngine::ROOT,
                &parse_one("<script>throw new Error('boom');</script>"),
                &mut warnings,
            );
            assert!(warnings.iter().any(|w| w.contains("boom")), "{warnings:?}");
        }
    }
}
