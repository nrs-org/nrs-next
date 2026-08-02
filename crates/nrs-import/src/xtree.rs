//! An owned XML tree, plus the `<def>`/`<ref>` macro system
//! (`nrsml/nrsml/macro.ts`'s `substituteMacro`, confirmed by reading
//! the actual interpreter source at `github.com/nrs-org/nrsml`).
//!
//! `roxmltree`'s tree borrows from the original document text, so it
//! can't hold synthesized nodes (a `<ref>` expansion substitutes macro
//! variables into copies of a `<def>`'s template - text that never
//! appeared in any single source file). This module converts to an
//! owned tree first so macro expansion has somewhere to put its
//! output; `xml.rs`'s entry/impact/relation parsing then walks the
//! (already macro-expanded) result instead of the raw `roxmltree` tree.
//!
//! The macro system itself is pure text substitution - `<ref
//! name="Foo" a_base="0.6">...</ref>` looks up the `<def name="Foo"
//! vars="a_base:double">TEMPLATE</def>` registered earlier, replaces
//! every occurrence of the substring `"a_base"` in the template's
//! attribute values and text with `"0.6"`, and splices the ref's own
//! children in wherever `<children/>` appears in the template. No JS
//! involved for this part - only `<script>` (see `script.rs`) needs
//! that.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct XNode {
    tag: String,
    attrs: BTreeMap<String, String>,
    children: Vec<XNode>,
    text: String,
    /// The directory of the XML file this node was parsed from -
    /// needed only to resolve `<script src="../relative.js">` paths
    /// (mirrors `<include>`'s own path resolution). `Arc` because
    /// every node from one file shares the identical path.
    source_dir: Arc<Path>,
    /// The consecutive run of `<!-- -->` comments immediately before
    /// this node in the source (joined by newline), if any - real
    /// grading reasoning lives here far more often than in the
    /// structured attributes (`nrs-store`'s `impact.note`, once
    /// confirmed through the review UI, is seeded from this). Only
    /// tracked for nested nodes (impact tags are always nested inside
    /// an `<entry>`); top-level document children don't collect this.
    preceding_comment: Option<String>,
}

impl XNode {
    pub fn tag_name(&self) -> &str {
        &self.tag
    }
    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attrs.get(name).map(String::as_str)
    }
    pub fn children(&self) -> impl Iterator<Item = &XNode> {
        self.children.iter()
    }
    /// Concatenated direct text content (for inline `<script>` bodies).
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Directory of the source file this node was parsed from, for
    /// resolving `<script src="...">` relative paths.
    pub fn source_dir(&self) -> &Path {
        &self.source_dir
    }
    /// The comment block immediately preceding this node, if any.
    pub fn preceding_comment(&self) -> Option<&str> {
        self.preceding_comment.as_deref()
    }
}

pub fn from_roxmltree(node: roxmltree::Node, source_dir: &Arc<Path>) -> XNode {
    let tag = node.tag_name().name().to_string();
    let attrs = node
        .attributes()
        .map(|a| (a.name().to_string(), a.value().to_string()))
        .collect();
    let mut children = Vec::new();
    let mut text = String::new();
    // Comments are collected separately from `text` (previously they
    // were silently merged into it via the `child.text()` fallback,
    // since roxmltree's `.text()` also covers comment nodes) and
    // attached to whichever element immediately follows a run of them
    // - matching how every comment in the real data actually sits.
    // Non-whitespace text content between a comment and an element
    // breaks the association; pure indentation/whitespace doesn't.
    let mut pending_comment: Vec<String> = Vec::new();
    for child in node.children() {
        if child.is_comment() {
            if let Some(t) = child.text() {
                let trimmed = t.trim();
                if !trimmed.is_empty() {
                    pending_comment.push(trimmed.to_string());
                }
            }
        } else if child.is_element() {
            let mut child_node = from_roxmltree(child, source_dir);
            if !pending_comment.is_empty() {
                child_node.preceding_comment = Some(pending_comment.join("\n"));
            }
            pending_comment.clear();
            children.push(child_node);
        } else if let Some(t) = child.text() {
            text.push_str(t);
            if !t.trim().is_empty() {
                pending_comment.clear();
            }
        }
    }
    XNode {
        tag,
        attrs,
        children,
        text,
        source_dir: source_dir.clone(),
        preceding_comment: None,
    }
}

pub fn arc_dir(path: &Path) -> Arc<Path> {
    Arc::from(path.parent().unwrap_or_else(|| Path::new(".")))
}

#[derive(Debug, Clone)]
pub struct Macro {
    pub template: Vec<XNode>,
}

/// Find every `<def name=".." vars="a:type,b:type">...</def>` anywhere
/// in the tree (macros are treated as globally visible - a
/// simplification of the original's lexical scoping that matches how
/// they're actually used: simple top-level definitions).
pub fn collect_macros(nodes: &[XNode], macros: &mut BTreeMap<String, Macro>) {
    for node in nodes {
        if node.tag_name() == "def" {
            if let Some(name) = node.attribute("name") {
                macros.insert(
                    name.to_string(),
                    Macro {
                        template: node.children.clone(),
                    },
                );
            }
        }
        collect_macros(&node.children, macros);
    }
}

fn substitute_string(s: &str, vars: &BTreeMap<String, String>) -> String {
    let mut result = s.to_string();
    for (k, v) in vars {
        result = result.replace(k.as_str(), v.as_str());
    }
    result
}

fn substitute_children(
    nodes: &[XNode],
    vars: &BTreeMap<String, String>,
    ref_children: &[XNode],
) -> Vec<XNode> {
    let mut out = Vec::new();
    for node in nodes {
        if node.tag_name() == "children" {
            out.extend(ref_children.iter().cloned());
        } else {
            out.push(XNode {
                tag: node.tag.clone(),
                attrs: node
                    .attrs
                    .iter()
                    .map(|(k, v)| (k.clone(), substitute_string(v, vars)))
                    .collect(),
                children: substitute_children(&node.children, vars, ref_children),
                text: substitute_string(&node.text, vars),
                source_dir: node.source_dir.clone(),
                // Template-internal comments aren't meaningful per
                // instantiation; a <ref> call site's own comment is
                // reattached separately in expand_refs_depth below.
                preceding_comment: None,
            });
        }
    }
    out
}

const MAX_MACRO_DEPTH: usize = 64;

/// Replace every `<ref name=".." attr="value" ...>children</ref>` with
/// its macro expansion, recursively (a ref's expansion may itself
/// contain further refs). `<def>` nodes are left as-is (harmless,
/// inert once collected) rather than stripped, since callers already
/// skip them once parsing the expanded tree.
pub fn expand_refs(
    nodes: Vec<XNode>,
    macros: &BTreeMap<String, Macro>,
    warnings: &mut Vec<String>,
) -> Vec<XNode> {
    expand_refs_depth(nodes, macros, warnings, 0)
}

fn expand_refs_depth(
    nodes: Vec<XNode>,
    macros: &BTreeMap<String, Macro>,
    warnings: &mut Vec<String>,
    depth: usize,
) -> Vec<XNode> {
    if depth > MAX_MACRO_DEPTH {
        warnings.push(
            "macro expansion exceeded max depth (64), possible recursive <ref>; stopped".into(),
        );
        return nodes;
    }
    nodes
        .into_iter()
        .flat_map(|node| -> Vec<XNode> {
            if node.tag_name() == "ref" {
                let Some(name) = node.attribute("name").map(String::from) else {
                    warnings.push("<ref> missing required attribute 'name', skipped".into());
                    return vec![];
                };
                let Some(macro_def) = macros.get(&name) else {
                    warnings.push(format!("<ref name=\"{name}\"/>: no such macro, skipped"));
                    return vec![];
                };
                let args: BTreeMap<String, String> = node
                    .attrs
                    .iter()
                    .filter(|(k, _)| k.as_str() != "name")
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                let ref_comment = node.preceding_comment.clone();
                let expanded = substitute_children(&macro_def.template, &args, &node.children);
                let mut expanded = expand_refs_depth(expanded, macros, warnings, depth + 1);
                // The <ref> call site's own comment (e.g. "during the
                // anti-ksdgk era..." before <ref name="KoikakeAEI" />)
                // annotates whatever it expands to, not the template
                // definition - reattach it to the first expanded node
                // rather than lose it.
                if let (Some(comment), Some(first)) = (ref_comment, expanded.first_mut()) {
                    first.preceding_comment.get_or_insert(comment);
                }
                expanded
            } else {
                let XNode {
                    tag,
                    attrs,
                    children,
                    text,
                    source_dir,
                    preceding_comment,
                } = node;
                vec![XNode {
                    tag,
                    attrs,
                    children: expand_refs_depth(children, macros, warnings, depth + 1),
                    text,
                    source_dir,
                    preceding_comment,
                }]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(xml: &str) -> Vec<XNode> {
        let doc = roxmltree::Document::parse(xml).unwrap();
        let dir = arc_dir(Path::new("test.xml"));
        doc.root_element()
            .children()
            .filter(roxmltree::Node::is_element)
            .map(|n| from_roxmltree(n, &dir))
            .collect()
    }

    #[test]
    fn substitutes_attribute_variables() {
        let nodes = parse(
            r#"<r>
                <def name="Greet" vars="a_name:string">
                    <cry emotions="a_name" />
                </def>
                <ref name="Greet" a_name="MU" />
            </r>"#,
        );
        let mut macros = BTreeMap::new();
        collect_macros(&nodes, &mut macros);
        let mut warnings = Vec::new();
        let expanded = expand_refs(nodes, &macros, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");

        let cry = expanded
            .iter()
            .find(|n| n.tag_name() == "cry")
            .expect("expanded <cry>");
        assert_eq!(cry.attribute("emotions"), Some("MU"));
    }

    #[test]
    fn splices_children_at_the_children_marker() {
        let nodes = parse(
            r#"<r>
                <def name="Wrap" vars="">
                    <aei base="1.0">
                        <children />
                    </aei>
                </def>
                <ref name="Wrap"><contributor id="X" /></ref>
            </r>"#,
        );
        let mut macros = BTreeMap::new();
        collect_macros(&nodes, &mut macros);
        let mut warnings = Vec::new();
        let expanded = expand_refs(nodes, &macros, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");

        let aei = expanded.iter().find(|n| n.tag_name() == "aei").unwrap();
        let contributor = aei
            .children()
            .find(|c| c.tag_name() == "contributor")
            .expect("spliced contributor");
        assert_eq!(contributor.attribute("id"), Some("X"));
    }

    #[test]
    fn unknown_macro_warns_and_drops_the_ref() {
        let nodes = parse(r#"<r><ref name="DoesNotExist" /></r>"#);
        let macros = BTreeMap::new();
        let mut warnings = Vec::new();
        let expanded = expand_refs(nodes, &macros, &mut warnings);
        assert!(expanded.is_empty());
        assert!(warnings.iter().any(|w| w.contains("DoesNotExist")));
    }

    #[test]
    fn captures_the_comment_block_immediately_preceding_a_nested_element() {
        let nodes = parse(
            r#"<r>
                <outer>
                    <!-- first line -->
                    <!-- second line -->
                    <inner />
                    <after />
                </outer>
            </r>"#,
        );
        let outer = &nodes[0];
        let inner = outer.children().find(|n| n.tag_name() == "inner").unwrap();
        assert_eq!(inner.preceding_comment(), Some("first line\nsecond line"));

        // A later sibling with no comment of its own doesn't inherit
        // the previous one.
        let after = outer.children().find(|n| n.tag_name() == "after").unwrap();
        assert_eq!(after.preceding_comment(), None);
    }

    #[test]
    fn real_text_between_comment_and_element_breaks_the_association() {
        let nodes = parse(
            r#"<r>
                <outer>
                    <!-- orphaned -->
                    some real text
                    <inner />
                </outer>
            </r>"#,
        );
        let outer = &nodes[0];
        let inner = outer.children().find(|n| n.tag_name() == "inner").unwrap();
        assert_eq!(inner.preceding_comment(), None);
    }

    #[test]
    fn ref_call_site_comment_is_reattached_to_its_expansion() {
        let nodes = parse(
            r#"<r>
                <def name="Greet" vars="a_name:string">
                    <cry emotions="a_name" />
                </def>
                <outer>
                    <!-- why this exists -->
                    <ref name="Greet" a_name="MU" />
                </outer>
            </r>"#,
        );
        let mut macros = BTreeMap::new();
        collect_macros(&nodes, &mut macros);
        let mut warnings = Vec::new();
        let expanded = expand_refs(nodes, &macros, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");

        let outer = expanded.iter().find(|n| n.tag_name() == "outer").unwrap();
        let cry = outer.children().find(|n| n.tag_name() == "cry").unwrap();
        assert_eq!(cry.preceding_comment(), Some("why this exists"));
    }
}
