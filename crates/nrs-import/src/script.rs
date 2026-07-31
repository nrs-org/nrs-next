//! `<script>` execution (Cargo feature `scripting`), via a pure-Rust JS
//! engine (`boa_engine`) so no C toolchain / V8 dependency is needed.
//!
//! Ported to match the real interpreter's `executeScript` mechanics
//! (confirmed by reading `github.com/nrs-org/nrsml`,
//! `nrsml/parse.ts`): a script body runs as
//! `new Function("global","local","scope", code).apply(scope, [scope.root.global, scope.local, scope])`,
//! and if the `<script>` tag has an `entryPoint` attribute, that
//! function is invoked as `local[entryPoint]()` immediately after.
//!
//! Real-data usage (only 3 files in the whole dataset use `<script>`
//! at all: `Koikake.xml`, `SB69.xml`, `ReStage.xml`) only ever needs a
//! script to mutate `this.value[0].contributors` (a real JS `Map`)
//! with `newScalarMatrix(x)`-tagged scalar weights, read/write a
//! per-scope `local` object, and read/write one document-wide
//! `global` object across the whole parse — so that's the whole API
//! surface implemented here, not a general nrsml interpreter. A
//! script that goes beyond that (e.g. constructs a Diagonal/Regular
//! contributor matrix — something no real script in the dataset does)
//! is surfaced as a warning rather than silently mishandled, since
//! `ParsedImpact.contributors` has no slot for a non-scalar weight.
//!
//! Implementation note: rather than learn `boa_engine`'s
//! object/Map/native-function Rust API in detail, every scope's
//! `<script>` children plus a `JSON.stringify` readback are compiled
//! into *one* JS program and run through a single confirmed-stable
//! primitive, `Context::eval`, with the result parsed back via
//! `serde_json`. `global` state persists across the whole document
//! because one `ScriptEngine` (and its one `boa_engine::Context`)
//! lives for the whole parse.

use crate::xtree::XNode;

/// One `<script>` child, resolved to executable source (inline text,
/// or the contents of `src=`) plus its `entryPoint`, if any.
#[cfg(feature = "scripting")]
struct ScriptCall {
    source: String,
    entry_point: Option<String>,
}

#[cfg(feature = "scripting")]
fn resolve_scripts(scripts_parent: &XNode, warnings: &mut Vec<String>) -> Vec<ScriptCall> {
    scripts_parent
        .children()
        .filter(|c| c.tag_name() == "script")
        .filter_map(|node| {
            let entry_point = node.attribute("entryPoint").map(String::from);
            if let Some(src) = node.attribute("src") {
                let path = node.source_dir().join(src);
                match std::fs::read_to_string(&path) {
                    Ok(source) => Some(ScriptCall {
                        source,
                        entry_point,
                    }),
                    Err(e) => {
                        warnings.push(format!("<script src=\"{src}\">: {}: {e}", path.display()));
                        None
                    }
                }
            } else {
                Some(ScriptCall {
                    source: node.text().to_string(),
                    entry_point,
                })
            }
        })
        .collect()
}

#[cfg(feature = "scripting")]
mod engine {
    use super::ScriptCall;
    use boa_engine::{Context, JsError, Source};

    const PRELUDE: &str = r#"
        var __nrs_global = {};
        function __nrs_new_scalar_matrix(x) { return { kind: "scalar", value: x }; }
        function __nrs_new_diagonal_matrix(arr) { return { kind: "diagonal", value: arr }; }
        function __nrs_new_regular_matrix(arr) { return { kind: "regular", value: arr }; }
    "#;

    pub struct Engine {
        context: Context,
    }

    impl Engine {
        pub fn new() -> Self {
            let mut context = Context::default();
            context
                .eval(Source::from_bytes(PRELUDE))
                .expect("prelude is valid JS");
            Engine { context }
        }

        fn run(&mut self, program: &str) -> Result<String, JsError> {
            let result = self.context.eval(Source::from_bytes(program))?;
            result
                .to_string(&mut self.context)
                .map(|s| s.to_std_string_escaped())
        }

        /// Run entry-scope scripts: no `scope.value`/contributors
        /// involved, only `local`/`global` side effects.
        pub fn run_entry_scripts(
            &mut self,
            scripts: &[ScriptCall],
            entry_id: &str,
        ) -> Result<(), String> {
            if scripts.is_empty() {
                return Ok(());
            }
            let program = build_program(scripts, entry_id, None);
            self.run(&program).map(|_| ()).map_err(|e| e.to_string())
        }

        /// Run impact-scope scripts, seeded with `existing_contributors`
        /// as the starting `scope.value[0].contributors` Map; returns
        /// its final `(id, matrix-json)` contents.
        pub fn run_impact_scripts(
            &mut self,
            scripts: &[ScriptCall],
            entry_id: &str,
            existing_contributors: &[(String, f32)],
        ) -> Result<Vec<(String, serde_json::Value)>, String> {
            if scripts.is_empty() {
                return Ok(existing_contributors
                    .iter()
                    .map(|(id, f)| (id.clone(), scalar_json(*f)))
                    .collect());
            }
            let program = build_program(scripts, entry_id, Some(existing_contributors));
            let json = self.run(&program).map_err(|e| e.to_string())?;
            serde_json::from_str(&json).map_err(|e| format!("couldn't parse script result: {e}"))
        }
    }

    fn scalar_json(value: f32) -> serde_json::Value {
        serde_json::json!({ "kind": "scalar", "value": value })
    }

    fn build_program(
        scripts: &[ScriptCall],
        entry_id: &str,
        contributors_seed: Option<&[(String, f32)]>,
    ) -> String {
        let entry_id_json = serde_json::to_string(entry_id).unwrap();

        let mut header = String::new();
        header.push_str("var local = {};\n");
        header.push_str("var global = __nrs_global;\n");
        if let Some(seed) = contributors_seed {
            header.push_str("var __contributors = new Map();\n");
            for (id, factor) in seed {
                let id_json = serde_json::to_string(id).unwrap();
                header.push_str(&format!(
                    "__contributors.set({id_json}, __nrs_new_scalar_matrix({factor}));\n"
                ));
            }
            header.push_str("var scope = { value: [ { contributors: __contributors } ], ");
        } else {
            header.push_str("var scope = { ");
        }
        header.push_str(&format!(
            "entry: {{ value: {{ id: {entry_id_json} }} }}, parent: {{ entry: {{ value: {{ id: {entry_id_json} }} }} }}, \
             root: {{ context: {{ api: {{ newScalarMatrix: __nrs_new_scalar_matrix, newDiagonalMatrix: __nrs_new_diagonal_matrix, \
             newRegularMatrix: __nrs_new_regular_matrix }} }} }} }};\n"
        ));

        let mut body = String::new();
        for call in scripts {
            body.push_str("(function(global, local, scope) {\n");
            body.push_str(&call.source);
            body.push_str("\n}).call(scope, global, local, scope);\n");
            if let Some(ep) = &call.entry_point {
                let ep_json = serde_json::to_string(ep).unwrap();
                body.push_str(&format!(
                    "if (typeof local[{ep_json}] === 'function') {{ local[{ep_json}](); }}\n"
                ));
            }
        }

        let tail = if contributors_seed.is_some() {
            "return JSON.stringify(Array.from(__contributors.entries()));"
        } else {
            "return null;"
        };

        format!("(function() {{\n{header}{body}{tail}\n}})()")
    }
}

pub struct ScriptEngine {
    #[cfg(feature = "scripting")]
    engine: engine::Engine,
}

impl ScriptEngine {
    pub fn new() -> Self {
        ScriptEngine {
            #[cfg(feature = "scripting")]
            engine: engine::Engine::new(),
        }
    }

    /// Run every `<script>` child of `scripts_parent` (an `<entry>`)
    /// in document order, sharing one `local` object across them.
    #[cfg(feature = "scripting")]
    pub fn run_entry_scripts(
        &mut self,
        scripts_parent: &XNode,
        entry_id: &str,
        warnings: &mut Vec<String>,
    ) {
        let scripts = resolve_scripts(scripts_parent, warnings);
        if let Err(e) = self.engine.run_entry_scripts(&scripts, entry_id) {
            warnings.push(format!("entry {entry_id}: <script> failed: {e}"));
        }
    }

    #[cfg(not(feature = "scripting"))]
    pub fn run_entry_scripts(
        &mut self,
        scripts_parent: &XNode,
        _entry_id: &str,
        warnings: &mut Vec<String>,
    ) {
        for _ in scripts_parent
            .children()
            .filter(|c| c.tag_name() == "script")
        {
            warnings
                .push("<script> skipped (build with the \"scripting\" feature to run it)".into());
        }
    }

    /// Run every `<script>` child of `scripts_parent` (an impact tag
    /// like `<aei>`/`<meme>`) at impact scope, seeded with
    /// `existing_contributors`; returns the merged, final contributor
    /// list. Non-scalar contributor weights (never produced by any
    /// real script, but theoretically constructible via
    /// `newDiagonalMatrix`/`newRegularMatrix`) are warned about and
    /// dropped, since `ParsedImpact.contributors` is scalar-only.
    #[cfg(feature = "scripting")]
    pub fn run_impact_scripts(
        &mut self,
        scripts_parent: &XNode,
        entry_id: &str,
        existing_contributors: &[(String, f32)],
        warnings: &mut Vec<String>,
    ) -> Vec<(String, f32)> {
        let scripts = resolve_scripts(scripts_parent, warnings);
        if scripts.is_empty() {
            return existing_contributors.to_vec();
        }
        match self.engine.run_impact_scripts(&scripts, entry_id, existing_contributors) {
            Ok(entries) => entries
                .into_iter()
                .filter_map(|(id, matrix)| match matrix.get("kind").and_then(|k| k.as_str()) {
                    Some("scalar") => matrix.get("value").and_then(|v| v.as_f64()).map(|v| (id, v as f32)),
                    other => {
                        warnings.push(format!(
                            "entry {entry_id}: <script> set contributor '{id}' to a {} matrix, not representable, dropped",
                            other.unwrap_or("unknown")
                        ));
                        None
                    }
                })
                .collect(),
            Err(e) => {
                warnings.push(format!("entry {entry_id}: <script> failed: {e}"));
                existing_contributors.to_vec()
            }
        }
    }

    #[cfg(not(feature = "scripting"))]
    pub fn run_impact_scripts(
        &mut self,
        scripts_parent: &XNode,
        _entry_id: &str,
        existing_contributors: &[(String, f32)],
        warnings: &mut Vec<String>,
    ) -> Vec<(String, f32)> {
        for _ in scripts_parent
            .children()
            .filter(|c| c.tag_name() == "script")
        {
            warnings
                .push("<script> skipped (build with the \"scripting\" feature to run it)".into());
        }
        existing_contributors.to_vec()
    }
}

impl Default for ScriptEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(test, feature = "scripting"))]
mod tests {
    use super::*;
    use crate::xtree::{arc_dir, from_roxmltree};
    use std::path::Path;

    fn parse_one(xml: &str) -> XNode {
        let doc = roxmltree::Document::parse(xml).unwrap();
        let dir = arc_dir(Path::new("test.xml"));
        from_roxmltree(doc.root_element(), &dir)
    }

    /// The exact Koikake.xml pattern: split one impact's contribution
    /// between the entry itself and a fixed external id.
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
        let mut warnings = Vec::new();
        let result = engine.run_impact_scripts(&node, "A-MAL-4181", &[], &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(result.len(), 2);
        let by_id: std::collections::BTreeMap<_, _> = result.into_iter().collect();
        assert!((by_id["A-MAL-4181"] - 0.7).abs() < 1e-5);
        assert!((by_id["V-VNDB-17516"] - 0.3).abs() < 1e-5);
    }

    /// The RSTSB69Music.js pattern, reduced: entry-scope script
    /// registers this entry's id into `global`, a later impact-scope
    /// script reads `global` back out and sets contributors from it.
    #[test]
    fn global_state_persists_across_scope_invocations() {
        let track = parse_one(
            r#"<entry>
                <script>
                    global.tracks ??= [];
                    global.tracks.push(scope.entry.value.id);
                </script>
            </entry>"#,
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
        engine.run_entry_scripts(&track, "M-TRACK-1", &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");

        let result = engine.run_impact_scripts(&meme, "F-FRANCHISE", &[], &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(result, vec![("M-TRACK-1".to_string(), 1.0)]);
    }

    #[test]
    fn non_scalar_contributor_matrix_warns_and_is_dropped() {
        let node = parse_one(
            r#"<aei>
                <script>
                    scope.value[0].contributors.set("X", scope.root.context.api.newDiagonalMatrix([1,0,0,0,0,0,0,0,0,0,0]));
                </script>
            </aei>"#,
        );
        let mut engine = ScriptEngine::new();
        let mut warnings = Vec::new();
        let result = engine.run_impact_scripts(&node, "E", &[], &mut warnings);
        assert!(result.is_empty());
        assert!(warnings.iter().any(|w| w.contains("not representable")));
    }
}
