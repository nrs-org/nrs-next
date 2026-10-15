//! Parser for the legacy `nrsml` XML format (schema:
//! `https://raw.githubusercontent.com/btmxh/nrsml/master/xml/schema.xsd`).
//!
//! This produces an intermediate representation (`ParsedDocument`), not
//! `nrs-core` types directly — `snapshot.rs` does that conversion using
//! `legacy.rs`'s formulas, and `roles.rs` handles `<role>`/`<musicvar>`
//! music-credit weighting. `<include>` is resolved and `<def>`/`<ref>`
//! macros are expanded (see `xtree.rs`) before entry parsing runs, so
//! this module only ever sees an already-flattened, already-expanded
//! tree. Nothing is silently dropped: anything this parser can't turn
//! into a concrete value (an unresolved template variable, a
//! `<script>` it doesn't understand) is recorded in
//! `ParsedDocument::warnings` rather than guessed at silently.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use time::Date;

use crate::script::{ScopeId, ScriptEngine};
use crate::xtree::{self, XNode};

#[derive(Debug)]
pub enum ImportError {
    Io(PathBuf, std::io::Error),
    Xml(PathBuf, roxmltree::Error),
    IncludeNotSupported(PathBuf),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Io(p, e) => write!(f, "{}: {e}", p.display()),
            ImportError::Xml(p, e) => write!(f, "{}: {e}", p.display()),
            ImportError::IncludeNotSupported(p) => {
                write!(
                    f,
                    "{}: <include> needs filesystem access; use parse_file, not parse_str",
                    p.display()
                )
            }
        }
    }
}
impl std::error::Error for ImportError {}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedEntry {
    pub id: String,
    pub title: String,
    /// Best-effort external id (INFRA.md's `(store, store_id)`),
    /// picked from whichever `<source>` sub-element is present, in
    /// priority order. `store == "none"` if the entry has no
    /// recognisable source at all (common for auto-generated track
    /// ids and bare artist entries).
    pub store: String,
    pub store_id: String,
    /// `<bind url="..."/>`: canonical URLs of the store records this entry
    /// is (stripped XML). Titles and store-asserted containment come from
    /// these via `stores::apply_store_facts`.
    pub binds: Vec<String>,
    /// `title` was filled in from a store, not written in the XML. Scoring
    /// rules that read the title (the "feat." credit rule) skip it: a store
    /// re-titling a record must not move a score.
    pub title_from_store: bool,
    /// `<unlink id="..."/>`: children whose store-asserted `contains` edge
    /// this entry refuses (the store's tracklist says so, NRS doesn't).
    pub unlinks: Vec<String>,
    pub best_girl: Option<String>,
    pub impacts: Vec<ParsedImpact>,
    pub relations: Vec<ParsedRelation>,
    /// Per-contributor role credit (`roles.rs` converts this into
    /// auto-generated `Contains` relations toward each credited id).
    pub roles: Vec<ParsedRole>,
    pub music_var: Option<ParsedMusicVar>,
    /// `<validatorSuppress rules="a;b"/>`: validation rules this entry is
    /// exempt from.
    pub suppress: Vec<String>,
}

/// `<musicvar>`: overrides to `roles::MusicVars`'s defaults. `None`
/// fields fall back to the computed default.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ParsedMusicVar {
    pub vocallyrics: Option<f32>,
    pub lyricsmusic: Option<f32>,
    pub emolyrics: Option<f32>,
    pub arrange: Option<f32>,
    pub instperform: Option<f32>,
    pub feat: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedImpact {
    pub kind: ImpactKind,
    /// `(entry id, factor)`; defaults to `[(self, 1.0)]` when no
    /// explicit `<contributor>` children are present.
    pub contributors: Vec<(String, f32)>,
    /// The `<!-- -->` comment block immediately preceding this impact
    /// tag, if any (`XNode::preceding_comment`) -- the real grading
    /// reasoning, seeded into `nrs-store`'s review workflow rather than
    /// becoming `impact.note` directly (INFRA.md's review UI confirms
    /// or edits it first).
    pub source_comment: Option<String>,
    /// Optional provenance for an explicit `anchors="..."` refinement.
    /// These stay attached to the parsed impact so automated migrations can
    /// be audited without changing the core snapshot format.
    pub anchor_source: Option<String>,
    pub anchor_confidence: Option<f32>,
    /// A `<validatorSuppress rules="..."/>` child: validation rules every
    /// entry this impact targets is exempt from.
    pub suppress: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImpactKind {
    Cry {
        blend: Vec<(String, f32)>,
    },
    Nei {
        base: f32,
        blend: Vec<(String, f32)>,
    },
    Aei {
        base: f32,
        blend: Vec<(String, f32)>,
    },
    Pads {
        days: f32,
        ranges: Vec<PeriodRange>,
        blend: Vec<(String, f32)>,
    },
    Waifu {
        days: f32,
        ranges: Vec<PeriodRange>,
    },
    Ehi {
        blend: Vec<(String, f32)>,
    },
    Epi {
        factor: f32,
        blend: Vec<(String, f32)>,
    },
    Jumpscare,
    SleeplessNight,
    Politics,
    InterestField {
        new_field: bool,
    },
    Dropped,
    Meme {
        strength: f32,
        days: f32,
        ranges: Vec<PeriodRange>,
    },
    Music {
        base: f32,
    },
    Additional {
        value: f32,
    },
    Visual {
        visual_type: String,
        base: f32,
        unique: f32,
    },
    OsuSong {
        personal: f32,
        community: f32,
    },
    Writing {
        character: f32,
        story: f32,
        pacing: f32,
        originality: f32,
    },
    AnimeConsumedProgress {
        boredom: f32,
        /// `None` when the XML leaves it to the anime store
        /// (`stores::apply_store_facts` fills the bound entry's count).
        episodes: Option<f32>,
        /// `None` when the XML leaves it out: the anime store's length for
        /// movies and shorts, NRS's 20-minute default otherwise.
        episode_duration_minutes: Option<f32>,
    },
    ConsumedProgress {
        boredom: f32,
        duration_minutes: f32,
    },
    MusicConsumedProgress {
        /// `None` when the XML leaves it to musiclib
        /// (`stores::apply_store_facts` fills the bound track's length).
        duration_minutes: Option<f32>,
    },
    /// `<regularImpact><score><component value=".." factor=".."/>...`:
    /// a raw, already-computed value directly at a named old factor
    /// (e.g. `"Emotion.AP"`, `"Art.Language"`) - no formula, unlike
    /// every other impact kind.
    Regular {
        components: Vec<(String, f32)>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeriodRange {
    pub from: String,
    pub to: String,
    /// Original dynamic expression, when resolving the endpoint lost
    /// information (for example `today` versus a literal date equal to today).
    pub from_expression: Option<String>,
    pub to_expression: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParsedRelation {
    Remix {
        to: String,
    },
    FeatureMusic {
        to: String,
    },
    KilledBy {
        to: String,
        potential: f32,
        effect: f32,
    },
    Contains {
        to: String,
        factor: f32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedRole {
    pub id: String,
    pub roles_expr: String,
}

/// A relation whose borrowers are its explicit `<contributor>`
/// children rather than the enclosing entry: the real engine's
/// `createRelationMaps` gives each contributor its own copy of the
/// relation, scaled by that contributor's factor, and the enclosing
/// entry (if any) only stands in when no contributor is listed.
#[derive(Debug, Clone, PartialEq)]
pub struct SharedRelation {
    pub relation: ParsedRelation,
    pub contributors: Vec<(String, f32)>,
}

#[derive(Debug, Default)]
pub struct ParsedDocument {
    pub entries: Vec<ParsedEntry>,
    /// Impacts declared at document level, outside any `<entry>` -
    /// typically one event split across several works via
    /// `<contributor>` children (shared memes, cross-franchise `aei`s).
    pub shared_impacts: Vec<ParsedImpact>,
    /// Relations carrying explicit `<contributor>` children, whether
    /// declared inside an entry or at document level.
    pub shared_relations: Vec<SharedRelation>,
    pub warnings: Vec<String>,
}

/// Parse a document rooted at `path`, following `<include>` elements
/// recursively (relative to the including file's directory) and
/// expanding `<def>`/`<ref>` macros, before entry parsing runs. `today`
/// pins what the `"today"`/`"today+N"` date forms resolve to, so
/// parsing is deterministic given a fixed reference date.
pub fn parse_file(path: &Path, today: Date) -> Result<ParsedDocument, ImportError> {
    let mut nodes = Vec::new();
    collect_document_nodes(path, true, &mut nodes)?;
    Ok(build_document(nodes, today))
}

/// Parse XML text directly, with no filesystem access - `<include>`
/// errors in this mode. Convenient for tests and embedded snippets.
pub fn parse_str(xml: &str, today: Date) -> Result<ParsedDocument, ImportError> {
    let mut nodes = Vec::new();
    collect_document_text(xml, Path::new("<string>"), false, &mut nodes)?;
    Ok(build_document(nodes, today))
}

fn collect_document_nodes(
    path: &Path,
    allow_include: bool,
    out: &mut Vec<XNode>,
) -> Result<(), ImportError> {
    let text = fs::read_to_string(path).map_err(|e| ImportError::Io(path.to_path_buf(), e))?;
    collect_document_text(&text, path, allow_include, out)
}

/// Reads the document's top-level children into `out`, inlining every
/// `<include>` in place (so macro collection afterwards sees one
/// flattened, whole-document node list).
fn collect_document_text(
    text: &str,
    path: &Path,
    allow_include: bool,
    out: &mut Vec<XNode>,
) -> Result<(), ImportError> {
    let xml =
        roxmltree::Document::parse(text).map_err(|e| ImportError::Xml(path.to_path_buf(), e))?;
    let root = xml.root_element();
    let dir = xtree::arc_dir(path);

    for child in root.children().filter(roxmltree::Node::is_element) {
        if child.tag_name().name() == "include" {
            if !allow_include {
                return Err(ImportError::IncludeNotSupported(path.to_path_buf()));
            }
            if let Some(src) = child.attribute("src") {
                collect_document_nodes(&dir.join(src), allow_include, out)?;
            }
        } else {
            out.push(xtree::from_roxmltree(child, &dir));
        }
    }
    Ok(())
}

fn build_document(nodes: Vec<XNode>, today: Date) -> ParsedDocument {
    let mut warnings = Vec::new();
    let mut macros = BTreeMap::new();
    xtree::collect_macros(&nodes, &mut macros);
    let expanded = xtree::expand_refs(nodes, &macros, &mut warnings);

    let mut entries = Vec::new();
    let mut shared_impacts = Vec::new();
    let mut shared_relations = Vec::new();
    let mut engine = ScriptEngine::new();
    for node in &expanded {
        let tag = node.tag_name();
        match tag {
            "entry" => parse_entry(
                node,
                today,
                &mut entries,
                &mut shared_relations,
                &mut warnings,
                &mut engine,
                ScriptEngine::ROOT,
            ),
            "def" => {} // already collected by collect_macros
            "script" => engine.run_script(ScriptEngine::ROOT, node, &mut warnings),
            "remix" | "featureMusic" | "killedBy" => {
                let context = "document";
                let contributors =
                    engine.relation_contributors(ScriptEngine::ROOT, node, &mut warnings);
                if contributors.is_empty() {
                    warnings.push(format!(
                        "{context}: top-level <{tag}> has no <contributor>, so nothing borrows through it; skipped"
                    ));
                } else if let Some(relation) =
                    parse_relation(node, tag, None, context, &mut warnings)
                {
                    shared_relations.push(SharedRelation {
                        relation,
                        contributors,
                    });
                }
            }
            _ => {
                let context = "document";
                let kinds = parse_impact_kinds(node, tag, today, context, &mut warnings);
                if kinds.is_empty() {
                    continue;
                }
                let per_impact = engine.impact_contributors(
                    ScriptEngine::ROOT,
                    node,
                    kinds.len(),
                    &mut warnings,
                );
                for (kind, contributors) in kinds.into_iter().zip(per_impact) {
                    // No enclosing entry to fall back to.
                    if contributors.is_empty() {
                        warnings.push(format!(
                            "{context}: top-level <{tag}> has no <contributor>, so it has no target; skipped"
                        ));
                        continue;
                    }
                    shared_impacts.push(parsed_impact(
                        node,
                        kind,
                        contributors,
                        context,
                        &mut warnings,
                    ));
                }
            }
        }
    }
    warn_duplicate_entry_ids(&entries, &mut warnings);
    ParsedDocument {
        entries,
        shared_impacts,
        shared_relations,
        warnings,
    }
}

/// A genuine duplicate `<entry id="...">` declaration (the same id
/// authored twice, e.g. a shared cross-franchise track re-declared in
/// two different files) silently collides downstream exactly like an
/// unresolved `$` placeholder would: `nrs_core`'s `Snapshot.entries`
/// keys everything by `EntryId`, so two declarations of one id merge
/// into a single measure. Not something to guess a fix for - just
/// make it visible.
fn warn_duplicate_entry_ids(entries: &[ParsedEntry], warnings: &mut Vec<String>) {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for e in entries {
        *counts.entry(e.id.as_str()).or_default() += 1;
    }
    for (id, count) in counts {
        if count > 1 {
            warnings.push(format!(
                "entry id '{id}' is declared {count} times; all declarations will be merged into one entry"
            ));
        }
    }
}

/// `transformId` (`nrsml/parse.ts`): a `$` in an id is a positional
/// placeholder, not a real id - e.g. `<entry id="$-1">` for an
/// unnamed track - and is only unique *within its containing entry*.
/// Every `$` gets replaced with the id of the nearest enclosing
/// `<entry>` before the string is used anywhere (as a declaration or
/// as a reference), which is what actually makes it globally unique.
/// A top-level entry (no enclosing entry) leaves `$` untouched,
/// matching the real engine's guard - moot in practice since
/// top-level entries always carry a real external id.
pub(crate) fn transform_id(id: &str, current_entry_id: Option<&str>) -> String {
    match current_entry_id {
        Some(cur) => id.replace('$', cur),
        None => id.to_string(),
    }
}

fn parse_entry(
    node: &XNode,
    today: Date,
    entries: &mut Vec<ParsedEntry>,
    shared_relations: &mut Vec<SharedRelation>,
    warnings: &mut Vec<String>,
    engine: &mut ScriptEngine,
    parent: ScopeId,
) {
    let Some(raw_id) = node.attribute("id") else {
        warnings.push(
            "<entry> missing required attribute 'id', entry (and any children) skipped".into(),
        );
        return;
    };
    let id = transform_id(raw_id, engine.entry_id(parent));
    let title = node.attribute("title").unwrap_or("").to_string();
    let scope = engine.open_entry(parent, &id, &title, node.attribute("entrytype"));
    let context_prefix = format!("entry {id}");

    let mut entry = ParsedEntry {
        id: id.clone(),
        title,
        store: "none".into(),
        store_id: id.clone(),
        binds: Vec::new(),
        title_from_store: false,
        unlinks: Vec::new(),
        best_girl: None,
        impacts: Vec::new(),
        relations: Vec::new(),
        roles: Vec::new(),
        music_var: None,
        suppress: Vec::new(),
    };

    for child in node.children() {
        let tag = child.tag_name();
        match tag {
            "entry" => {
                match child.attribute("id") {
                    Some(raw_child_id) => {
                        let child_id = transform_id(raw_child_id, Some(&id));
                        entry.relations.push(ParsedRelation::Contains {
                            to: child_id,
                            factor: 1.0,
                        });
                    }
                    None => warnings.push(format!(
                        "{context_prefix}: nested <entry> missing required attribute 'id', skipped"
                    )),
                }
                parse_entry(
                    child,
                    today,
                    entries,
                    shared_relations,
                    warnings,
                    engine,
                    scope,
                );
            }
            "source" => parse_source(child, &mut entry),
            "bind" => match child.attribute("url") {
                Some(url) => entry.binds.push(url.to_string()),
                None => warnings.push(format!(
                    "{context_prefix}: <bind> missing required attribute 'url', skipped"
                )),
            },
            "unlink" => match child.attribute("id") {
                Some(target) => entry.unlinks.push(transform_id(target, Some(&id))),
                None => warnings.push(format!(
                    "{context_prefix}: <unlink> missing required attribute 'id', skipped"
                )),
            },
            "bestGirl" => entry.best_girl = child.attribute("name").map(String::from),
            "contains" => {
                for (to, factor) in parse_contains(child, 1.0, &id) {
                    entry
                        .relations
                        .push(ParsedRelation::Contains { to, factor });
                }
            }
            "role" => {
                if let Some(role_id) = child.attribute("id") {
                    let roles_expr = child.attribute("roles").unwrap_or("").to_string();
                    entry.roles.push(ParsedRole {
                        id: transform_id(role_id, Some(&id)),
                        roles_expr,
                    });
                } else {
                    warnings.push(format!(
                        "{context_prefix}: <role> missing required attribute 'id', skipped"
                    ));
                }
            }
            "remix" | "featureMusic" | "killedBy" => {
                if let Some(relation) =
                    parse_relation(child, tag, Some(&id), &context_prefix, warnings)
                {
                    let contributors = engine.relation_contributors(scope, child, warnings);
                    if contributors == [(id.clone(), 1.0)] {
                        entry.relations.push(relation);
                    } else {
                        shared_relations.push(SharedRelation {
                            relation,
                            contributors,
                        });
                    }
                }
            }
            "musicvar" => {
                entry.music_var = Some(ParsedMusicVar {
                    vocallyrics: child.attribute("vocallyrics").and_then(|s| s.parse().ok()),
                    lyricsmusic: child.attribute("lyricsmusic").and_then(|s| s.parse().ok()),
                    emolyrics: child.attribute("emolyrics").and_then(|s| s.parse().ok()),
                    arrange: child.attribute("arrange").and_then(|s| s.parse().ok()),
                    instperform: child.attribute("instperform").and_then(|s| s.parse().ok()),
                    feat: child.attribute("feat").map(|s| s == "true"),
                });
            }
            "script" => engine.run_script(scope, child, warnings),
            "validatorSuppress" => entry.suppress.extend(suppressed_rules(child)),
            "queue" | "def" => {} // pure metadata / already-expanded macro decl
            _ => {
                let kinds = parse_impact_kinds(child, tag, today, &context_prefix, warnings);
                if kinds.is_empty() {
                    continue;
                }
                let per_impact = engine.impact_contributors(scope, child, kinds.len(), warnings);
                for (kind, contributors) in kinds.into_iter().zip(per_impact) {
                    entry.impacts.push(parsed_impact(
                        child,
                        kind,
                        contributors,
                        &context_prefix,
                        warnings,
                    ));
                }
            }
        }
    }

    engine.close_entry(scope);
    entries.push(entry);
}

fn parsed_impact(
    node: &XNode,
    kind: ImpactKind,
    contributors: Vec<(String, f32)>,
    context: &str,
    warnings: &mut Vec<String>,
) -> ParsedImpact {
    ParsedImpact {
        kind,
        contributors,
        source_comment: node.preceding_comment().map(String::from),
        anchor_source: node.attribute("anchorSource").map(String::from),
        anchor_confidence: parse_anchor_confidence(node, context, warnings),
        suppress: node
            .children()
            .filter(|child| child.tag_name() == "validatorSuppress")
            .flat_map(suppressed_rules)
            .collect(),
    }
}

/// A `<validatorSuppress rules="a;b"/>`'s rule names; empty pieces (a
/// stray leading `;`) are skipped.
fn suppressed_rules(node: &XNode) -> Vec<String> {
    node.attribute("rules")
        .unwrap_or("")
        .split(';')
        .map(str::trim)
        .filter(|rule| !rule.is_empty())
        .map(String::from)
        .collect()
}

fn parse_source(node: &XNode, entry: &mut ParsedEntry) {
    // Priority order for a single (store, store_id): the dedicated
    // provider tags first, then a generic <urls><url .../></urls>
    // fallback (the common case for music entries, which mostly don't
    // have a dedicated tag).
    for provider in ["mal", "al", "adb", "ks", "vndb"] {
        if let Some(child) = node.children().find(|c| c.tag_name() == provider) {
            if let Some(id) = child.attribute("id") {
                entry.store = provider.to_string();
                entry.store_id = id.to_string();
                return;
            }
        }
    }
    if let Some(vgmdb) = node.children().find(|c| c.tag_name() == "vgmdb") {
        for key in ["product", "album", "artist", "track", "disc"] {
            if let Some(id) = vgmdb.attribute(key) {
                entry.store = format!("vgmdb-{key}");
                entry.store_id = id.to_string();
                return;
            }
        }
    }
    if let Some(urls) = node.children().find(|c| c.tag_name() == "urls") {
        if let Some(url) = urls.children().find(|c| c.tag_name() == "url") {
            if let (Some(name), Some(src)) = (url.attribute("name"), url.attribute("src")) {
                entry.store = format!("url-{name}");
                entry.store_id = src.to_string();
            }
        }
    }
}

/// `<contains>` may nest further `<contains>` children, each
/// multiplying its own `factor` (default `1.0`) into the parent's -
/// e.g. `<contains factor="0.5"><contains id="X"/></contains>`.
fn parse_contains(
    node: &XNode,
    inherited_factor: f32,
    current_entry_id: &str,
) -> Vec<(String, f32)> {
    let factor = node.attribute("factor").map(parse_fraction).unwrap_or(1.0) * inherited_factor;
    let mut result = Vec::new();
    if let Some(id) = node.attribute("id") {
        result.push((transform_id(id, Some(current_entry_id)), factor));
    }
    for child in node.children().filter(|c| c.tag_name() == "contains") {
        result.extend(parse_contains(child, factor, current_entry_id));
    }
    result
}

pub(crate) fn parse_fraction(s: &str) -> f32 {
    if let Some((num, den)) = s.split_once('/') {
        let n: f32 = num.trim().parse().unwrap_or(1.0);
        let d: f32 = den.trim().parse().unwrap_or(1.0);
        n / d
    } else {
        s.trim().parse().unwrap_or(1.0)
    }
}

fn parse_relation(
    node: &XNode,
    tag: &str,
    current_entry_id: Option<&str>,
    context: &str,
    warnings: &mut Vec<String>,
) -> Option<ParsedRelation> {
    let to = match node.attribute("id") {
        Some(id) => transform_id(id, current_entry_id),
        None => {
            warnings.push(format!(
                "{context}: <{tag}> missing required attribute 'id', skipped"
            ));
            return None;
        }
    };
    match tag {
        "remix" => Some(ParsedRelation::Remix { to }),
        "featureMusic" => Some(ParsedRelation::FeatureMusic { to }),
        "killedBy" => {
            let potential = req_f32(node, "potential", context, warnings)?;
            let effect = req_f32(node, "effect", context, warnings)?;
            Some(ParsedRelation::KilledBy {
                to,
                potential,
                effect,
            })
        }
        _ => unreachable!(),
    }
}

fn req_f32(node: &XNode, name: &str, context: &str, warnings: &mut Vec<String>) -> Option<f32> {
    match node.attribute(name).and_then(|s| s.parse().ok()) {
        Some(v) => Some(v),
        None => {
            warnings.push(format!(
                "{context}: <{}> missing/unparseable required attribute '{name}', skipped",
                node.tag_name()
            ));
            None
        }
    }
}

fn opt_f32(node: &XNode, name: &str, default: f32) -> f32 {
    node.attribute(name)
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn parse_anchor_confidence(node: &XNode, context: &str, warnings: &mut Vec<String>) -> Option<f32> {
    let raw = node.attribute("anchorConfidence")?;
    match raw.parse::<f32>() {
        Ok(value) if (0.0..=1.0).contains(&value) => Some(value),
        _ => {
            warnings.push(format!(
                "{context}: <{}> has invalid anchorConfidence '{raw}' (expected 0..1), ignored",
                node.tag_name()
            ));
            None
        }
    }
}

/// `EIEmotions`: either a bare code (`"MU"`, implicit weight 1.0) or a
/// colon-separated list of `CODE-weight` pairs (`"CU-0.5:CP-0.5"`).
fn parse_emotions(s: &str) -> Vec<(String, f32)> {
    if !s.contains(':') && !s.contains('-') {
        return vec![(s.to_string(), 1.0)];
    }
    s.split(':')
        .filter_map(|part| {
            let (code, w) = part.rsplit_once('-')?;
            Some((code.to_string(), w.parse().ok()?))
        })
        .collect()
}

fn is_canonical_anchor(code: &str) -> bool {
    matches!(
        code,
        "comedy"
            | "hype"
            | "tragedy"
            | "wistful"
            | "cozy"
            | "heartwarming"
            | "hate"
            | "discomfort"
            | "waifu"
            | "meme"
            | "hooked"
            | "worldview"
            | "gateway"
            | "writing"
            | "visual"
            | "music"
            | "boredom"
            | "additional"
    )
}

fn parse_explicit_anchors(
    raw: &str,
    node: &XNode,
    context: &str,
    warnings: &mut Vec<String>,
) -> Option<Vec<(String, f32)>> {
    let parsed = parse_emotions(raw);
    let unique: BTreeSet<&str> = parsed.iter().map(|(code, _)| code.as_str()).collect();
    let sum: f32 = parsed.iter().map(|(_, weight)| *weight).sum();
    let valid = !parsed.is_empty()
        && parsed.iter().all(|(code, weight)| {
            is_canonical_anchor(code) && weight.is_finite() && *weight >= 0.0
        })
        && unique.len() == parsed.len()
        && (sum - 1.0).abs() <= 0.001;
    if !valid {
        warnings.push(format!(
            "{context}: <{}> has invalid anchors '{raw}' (use unique canonical ids with non-negative weights summing to 1), skipped",
            node.tag_name()
        ));
        None
    } else {
        Some(parsed)
    }
}

/// Prefer the refined canonical `anchors` vocabulary when present, while
/// accepting the old six-box `emotions` vocabulary during migration.
fn parse_anchor_blend(
    node: &XNode,
    context: &str,
    warnings: &mut Vec<String>,
) -> Option<Vec<(String, f32)>> {
    match (node.attribute("anchors"), node.attribute("emotions")) {
        (Some(anchors), Some(_)) => {
            warnings.push(format!(
                "{context}: <{}> has both 'anchors' and legacy 'emotions'; using 'anchors'",
                node.tag_name()
            ));
            parse_explicit_anchors(anchors, node, context, warnings)
        }
        (Some(anchors), None) => parse_explicit_anchors(anchors, node, context, warnings),
        (None, Some(emotions)) => Some(parse_emotions(emotions)),
        (None, None) => Some(Vec::new()),
    }
}

/// `Duration` (`"(\d+:)?\d\d?:\d{2}"`, i.e. `MM:SS` or `HH:MM:SS`) to
/// total minutes.
fn parse_duration_minutes(s: &str) -> Option<f32> {
    let parts: Vec<f32> = s
        .split(':')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    match parts.as_slice() {
        [m, sec] => Some(m + sec / 60.0),
        [h, m, sec] => Some(h * 60.0 + m + sec / 60.0),
        _ => None,
    }
}

/// `Date`: `"today"`, `"today+N"`, `"epoch"`, `"epoch+N"`, or an ISO
/// 8601 date.
fn parse_date(s: &str, today: Date) -> Option<Date> {
    if s == "today" {
        return Some(today);
    }
    if let Some(rest) = s.strip_prefix("today+") {
        return Some(today + time::Duration::days(rest.parse().ok()?));
    }
    const EPOCH: Date = time::macros::date!(1970 - 01 - 01);
    if s == "epoch" {
        return Some(EPOCH);
    }
    if let Some(rest) = s.strip_prefix("epoch+") {
        return Some(EPOCH + time::Duration::days(rest.parse().ok()?));
    }
    Date::parse(s, &time::format_description::well_known::Iso8601::DATE).ok()
}

/// Total days for a `length`/`from`/`to` period, on `node` itself plus
/// any nested `<periods>` children, all summed (INFRA's
/// `DatePeriod[]`). Returns `0.0` (with a warning) if nothing usable
/// is present, rather than failing the whole entry.
fn period_days(
    node: &XNode,
    today: Date,
    context: &str,
    warnings: &mut Vec<String>,
) -> (f32, Vec<PeriodRange>) {
    let one = |n: &XNode| -> Option<(f32, Option<PeriodRange>)> {
        if let Some(len) = n.attribute("length") {
            return len.parse().ok().map(|days| (days, None));
        }
        let from_source = n.attribute("from")?;
        let from = parse_date(from_source, today)?;
        let (to, to_expression) = match n.attribute("to") {
            Some(to_source) => (
                parse_date(to_source, today)?,
                to_source
                    .starts_with("today")
                    .then(|| to_source.to_string()),
            ),
            // nrsml treats an omitted end as an interval continuing through
            // today. Preserve that dynamic meaning as well as its resolution.
            None => (today, Some("today".into())),
        };
        Some((
            (to - from).whole_days() as f32,
            Some(PeriodRange {
                from: from.to_string(),
                to: to.to_string(),
                from_expression: from_source
                    .starts_with("today")
                    .then(|| from_source.to_string()),
                to_expression,
            }),
        ))
    };

    let mut total = 0.0;
    let mut ranges = Vec::new();
    let mut found = false;
    if let Some((d, range)) = one(node) {
        total += d;
        ranges.extend(range);
        found = true;
    }
    for child in node.children().filter(|c| c.tag_name() == "periods") {
        if let Some((d, range)) = one(child) {
            total += d;
            ranges.extend(range);
            found = true;
        }
    }
    if !found {
        warnings.push(format!(
            "{context}: <{}> has no usable length/from/to (possibly an unresolved template value), treated as 0 days",
            node.tag_name()
        ));
    }
    (total, ranges)
}

/// The impacts one tag stands for. Most tags are a single impact, but
/// `maxAEIPADS` and `cryPADS` are the real engine's shorthands for a
/// pair (`DAH_standards.ts`): a full-strength `aei` (factor 1.0) or a
/// `cry`, plus a `pads` over the same emotions and period.
fn parse_impact_kinds(
    node: &XNode,
    tag: &str,
    today: Date,
    context: &str,
    warnings: &mut Vec<String>,
) -> Vec<ImpactKind> {
    let Some(pads) = parse_impact_kind(node, tag, today, context, warnings) else {
        return Vec::new();
    };
    let ImpactKind::Pads { blend, .. } = &pads else {
        return vec![pads];
    };
    let blend = blend.clone();
    match tag {
        "maxAEIPADS" => vec![ImpactKind::Aei { base: 1.0, blend }, pads],
        "cryPADS" => vec![ImpactKind::Cry { blend }, pads],
        _ => vec![pads],
    }
}

fn parse_impact_kind(
    node: &XNode,
    tag: &str,
    today: Date,
    context: &str,
    warnings: &mut Vec<String>,
) -> Option<ImpactKind> {
    match tag {
        "cry" => Some(ImpactKind::Cry {
            blend: parse_anchor_blend(node, context, warnings)?,
        }),
        "nei" => Some(ImpactKind::Nei {
            base: req_f32(node, "base", context, warnings)?,
            blend: parse_anchor_blend(node, context, warnings)?,
        }),
        "aei" => Some(ImpactKind::Aei {
            base: req_f32(node, "base", context, warnings)?,
            blend: parse_anchor_blend(node, context, warnings)?,
        }),
        "pads" | "maxAEIPADS" | "cryPADS" => {
            let (days, ranges) = period_days(node, today, context, warnings);
            Some(ImpactKind::Pads {
                days,
                ranges,
                blend: parse_anchor_blend(node, context, warnings)?,
            })
        }
        "waifu" => {
            let (days, ranges) = period_days(node, today, context, warnings);
            Some(ImpactKind::Waifu { days, ranges })
        }
        "ehi" => Some(ImpactKind::Ehi {
            blend: parse_anchor_blend(node, context, warnings)?,
        }),
        "epi" => Some(ImpactKind::Epi {
            factor: req_f32(node, "base", context, warnings)?,
            blend: parse_anchor_blend(node, context, warnings)?,
        }),
        "jumpscare" => Some(ImpactKind::Jumpscare),
        "sleeplessNight" => Some(ImpactKind::SleeplessNight),
        "politics" => Some(ImpactKind::Politics),
        "interestField" => Some(ImpactKind::InterestField {
            new_field: node.attribute("new").map(|s| s == "true").unwrap_or(false),
        }),
        "dropped" => Some(ImpactKind::Dropped),
        "meme" => {
            let (days, ranges) = period_days(node, today, context, warnings);
            Some(ImpactKind::Meme {
                strength: req_f32(node, "strength", context, warnings)?,
                days,
                ranges,
            })
        }
        "music" => Some(ImpactKind::Music {
            base: req_f32(node, "base", context, warnings)?,
        }),
        "additional" => Some(ImpactKind::Additional {
            value: req_f32(node, "value", context, warnings)?,
        }),
        "visual" => Some(ImpactKind::Visual {
            visual_type: node.attribute("type").unwrap_or("").to_string(),
            base: req_f32(node, "base", context, warnings)?,
            unique: req_f32(node, "unique", context, warnings)?,
        }),
        "osuSong" => Some(ImpactKind::OsuSong {
            personal: opt_f32(node, "personal", 0.0),
            community: opt_f32(node, "community", 0.0),
        }),
        "writing" => Some(ImpactKind::Writing {
            character: req_f32(node, "character", context, warnings)?,
            story: req_f32(node, "story", context, warnings)?,
            pacing: req_f32(node, "pacing", context, warnings)?,
            originality: req_f32(node, "originality", context, warnings)?,
        }),
        "animeConsumedProgress" => Some(ImpactKind::AnimeConsumedProgress {
            boredom: req_f32(node, "boredom", context, warnings)?,
            episodes: match node.attribute("episodes") {
                None => None,
                Some(_) => Some(req_f32(node, "episodes", context, warnings)?),
            },
            episode_duration_minutes: node
                .attribute("episodeDuration")
                .and_then(parse_duration_minutes),
        }),
        "consumedProgress" => Some(ImpactKind::ConsumedProgress {
            boredom: req_f32(node, "boredom", context, warnings)?,
            duration_minutes: node
                .attribute("duration")
                .and_then(parse_duration_minutes)
                .or_else(|| {
                    warnings.push(format!(
                        "{context}: <consumedProgress> unparseable 'duration'"
                    ));
                    None
                })?,
        }),
        "musicConsumedProgress" => Some(ImpactKind::MusicConsumedProgress {
            duration_minutes: match node.attribute("length") {
                None => None,
                Some(length) => Some(parse_duration_minutes(length).or_else(|| {
                    warnings.push(format!(
                        "{context}: <musicConsumedProgress> unparseable 'length'"
                    ));
                    None
                })?),
            },
        }),
        // `<consumed>`'s duration attribute is (inconsistently, but
        // per-schema) named "length", unlike consumedProgress's "duration".
        "consumed" => Some(ImpactKind::ConsumedProgress {
            boredom: req_f32(node, "boredom", context, warnings)?,
            duration_minutes: node
                .attribute("length")
                .and_then(parse_duration_minutes)
                .or_else(|| {
                    warnings.push(format!("{context}: <consumed> unparseable 'length'"));
                    None
                })?,
        }),
        "regularImpact" => {
            let score = node.children().find(|c| c.tag_name() == "score");
            let components: Vec<(String, f32)> = score
                .into_iter()
                .flat_map(|s| s.children())
                .filter(|c| c.tag_name() == "component")
                .filter_map(|c| {
                    let factor = c.attribute("factor")?.to_string();
                    let value: f32 = c.attribute("value")?.parse().ok()?;
                    Some((factor, value))
                })
                .collect();
            if components.is_empty() {
                warnings.push(format!(
                    "{context}: <regularImpact> has no usable <component>s, skipped"
                ));
                None
            } else {
                Some(ImpactKind::Regular { components })
            }
        }
        other => {
            warnings.push(format!("{context}: unknown element <{other}>, skipped"));
            None
        }
    }
}

/// All ids referenced anywhere (contributors, relation targets) - used
/// by `snapshot.rs` to tell a real dangling/null-entry reference apart
/// from an id that just hasn't been parsed yet.
pub fn referenced_ids(doc: &ParsedDocument) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let impacts = doc
        .entries
        .iter()
        .flat_map(|e| &e.impacts)
        .chain(&doc.shared_impacts);
    for impact in impacts {
        for (id, _) in &impact.contributors {
            ids.insert(id.clone());
        }
    }
    for entry in &doc.entries {
        for relation in &entry.relations {
            ids.insert(relation.target().to_string());
        }
    }
    for shared in &doc.shared_relations {
        ids.insert(shared.relation.target().to_string());
        for (id, _) in &shared.contributors {
            ids.insert(id.clone());
        }
    }
    ids
}

impl ParsedRelation {
    pub fn target(&self) -> &str {
        match self {
            ParsedRelation::Remix { to }
            | ParsedRelation::FeatureMusic { to }
            | ParsedRelation::KilledBy { to, .. }
            | ParsedRelation::Contains { to, .. } => to,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn today() -> Date {
        time::macros::date!(2020 - 01 - 11)
    }

    fn entry<'a>(doc: &'a ParsedDocument, id: &str) -> &'a ParsedEntry {
        doc.entries
            .iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| panic!("no entry {id}"))
    }

    #[test]
    fn parses_bind_and_unlink() {
        let doc = parse_str(
            r#"<document>
                <entry id="M-1">
                    <bind url="https://musicbrainz.org/release/r" />
                    <unlink id="$-2" />
                    <entry id="$-1"><bind url="https://youtu.be/abcdefghijk" /></entry>
                </entry>
            </document>"#,
            today(),
        )
        .unwrap();
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        let album = entry(&doc, "M-1");
        assert_eq!(album.title, "");
        assert_eq!(album.binds, ["https://musicbrainz.org/release/r"]);
        assert_eq!(album.unlinks, ["M-1-2"]);
        assert_eq!(entry(&doc, "M-1-1").binds, ["https://youtu.be/abcdefghijk"]);
    }

    #[test]
    fn parses_source_and_cry() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="Test">
                    <source><mal id="123" /></source>
                    <cry emotions="MU" />
                </entry>
            </document>"#,
            today(),
        )
        .unwrap();
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        let e = entry(&doc, "A-1");
        assert_eq!(e.store, "mal");
        assert_eq!(e.store_id, "123");
        assert_eq!(e.impacts.len(), 1);
        assert_eq!(
            e.impacts[0].kind,
            ImpactKind::Cry {
                blend: vec![("MU".into(), 1.0)]
            }
        );
        assert_eq!(e.impacts[0].contributors, vec![("A-1".into(), 1.0)]);
    }

    #[test]
    fn canonical_anchors_override_legacy_emotions_with_provenance() {
        let doc = parse_str(
            r#"<document><entry id="A-1">
                <ehi anchors="hype-0.7:comedy-0.3" emotions="AP"
                     anchorSource="luna-v1" anchorConfidence="0.91" />
            </entry></document>"#,
            today(),
        )
        .unwrap();
        let impact = &entry(&doc, "A-1").impacts[0];
        assert_eq!(
            impact.kind,
            ImpactKind::Ehi {
                blend: vec![("hype".into(), 0.7), ("comedy".into(), 0.3)]
            }
        );
        assert_eq!(impact.anchor_source.as_deref(), Some("luna-v1"));
        assert_eq!(impact.anchor_confidence, Some(0.91));
        assert!(doc
            .warnings
            .iter()
            .any(|warning| warning.contains("both 'anchors' and legacy 'emotions'")));
    }

    #[test]
    fn malformed_canonical_anchors_are_skipped_without_legacy_fallback() {
        let doc = parse_str(
            r#"<document><entry id="A-1">
                <ehi anchors="made-up-1.0" emotions="AP" />
            </entry></document>"#,
            today(),
        )
        .unwrap();
        assert!(entry(&doc, "A-1").impacts.is_empty());
        assert!(doc
            .warnings
            .iter()
            .any(|warning| warning.contains("invalid anchors")));
    }

    #[test]
    fn max_aei_pads_and_cry_pads_expand_to_both_impacts() {
        let doc = parse_str(
            r#"<document><entry id="A-1">
                <maxAEIPADS emotions="CU" from="2020-01-01" to="2020-01-02" />
                <cryPADS emotions="MU" from="2020-01-01" to="2020-01-02" />
            </entry></document>"#,
            today(),
        )
        .unwrap();
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        let kinds: Vec<_> = entry(&doc, "A-1").impacts.iter().map(|i| &i.kind).collect();
        assert_eq!(kinds.len(), 4);
        assert_eq!(
            kinds[0],
            &ImpactKind::Aei {
                base: 1.0,
                blend: vec![("CU".into(), 1.0)]
            }
        );
        assert!(matches!(kinds[1], ImpactKind::Pads { .. }));
        assert_eq!(
            kinds[2],
            &ImpactKind::Cry {
                blend: vec![("MU".into(), 1.0)]
            }
        );
        assert!(matches!(kinds[3], ImpactKind::Pads { .. }));
    }

    #[test]
    fn top_level_impacts_and_relations_are_kept() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" />
                <entry id="B-1" />
                <entry id="K-1" />
                <meme strength="1.0" from="2020-01-01" to="2020-01-10">
                    <contributor id="A-1" factor="0.75" />
                    <contributor id="B-1" factor="0.25" />
                </meme>
                <killedBy id="K-1" potential="0.4" effect="0.9">
                    <contributor id="A-1" factor="0.8" />
                </killedBy>
            </document>"#,
            today(),
        )
        .unwrap();
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        assert_eq!(doc.shared_impacts.len(), 1);
        assert_eq!(
            doc.shared_impacts[0].contributors,
            vec![("A-1".into(), 0.75), ("B-1".into(), 0.25)]
        );
        assert_eq!(
            doc.shared_relations,
            vec![SharedRelation {
                relation: ParsedRelation::KilledBy {
                    to: "K-1".into(),
                    potential: 0.4,
                    effect: 0.9,
                },
                contributors: vec![("A-1".into(), 0.8)],
            }]
        );
    }

    #[test]
    fn top_level_impact_without_contributors_warns() {
        let doc = parse_str(
            r#"<document><aei base="0.5" emotions="AP" /></document>"#,
            today(),
        )
        .unwrap();
        assert!(doc.shared_impacts.is_empty());
        assert!(doc
            .warnings
            .iter()
            .any(|warning| warning.contains("no <contributor>")));
    }

    #[test]
    fn relation_contributors_replace_the_enclosing_entry() {
        let doc = parse_str(
            r#"<document><entry id="F-1">
                <entry id="A-1" />
                <killedBy id="K-1" potential="0.4" effect="0.9">
                    <contributor id="A-1" factor="0.8" />
                    <contributor id="F-1" factor="0.2" />
                </killedBy>
            </entry></document>"#,
            today(),
        )
        .unwrap();
        // Only the nested-entry Contains stays on F-1 itself.
        assert_eq!(entry(&doc, "F-1").relations.len(), 1);
        assert_eq!(doc.shared_relations.len(), 1);
        assert_eq!(
            doc.shared_relations[0].contributors,
            vec![("A-1".into(), 0.8), ("F-1".into(), 0.2)]
        );
    }

    #[test]
    fn nested_entry_becomes_flat_plus_contains() {
        let doc = parse_str(
            r#"<document>
                <entry id="F-1" title="Franchise">
                    <entry id="A-1" title="Child" />
                </entry>
            </document>"#,
            today(),
        )
        .unwrap();
        assert_eq!(doc.entries.len(), 2);
        let f = entry(&doc, "F-1");
        assert_eq!(
            f.relations,
            vec![ParsedRelation::Contains {
                to: "A-1".into(),
                factor: 1.0
            }]
        );
    }

    #[test]
    fn explicit_contains_parses_fraction_factor() {
        let doc = parse_str(
            r#"<document>
                <entry id="C-1" title="Container">
                    <contains id="M-1" factor="1/5" />
                </entry>
                <entry id="M-1" title="Member" />
            </document>"#,
            today(),
        )
        .unwrap();
        let c = entry(&doc, "C-1");
        assert_eq!(
            c.relations,
            vec![ParsedRelation::Contains {
                to: "M-1".into(),
                factor: 0.2
            }]
        );
    }

    #[test]
    fn nested_contains_multiplies_factor() {
        let doc = parse_str(
            r#"<document>
                <entry id="C-1" title="Container">
                    <contains factor="0.5">
                        <contains id="M-1" />
                    </contains>
                </entry>
                <entry id="M-1" title="Member" />
            </document>"#,
            today(),
        )
        .unwrap();
        let c = entry(&doc, "C-1");
        assert_eq!(
            c.relations,
            vec![ParsedRelation::Contains {
                to: "M-1".into(),
                factor: 0.5
            }]
        );
    }

    #[test]
    fn relations_parse_correctly() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="A">
                    <remix id="B-1" />
                    <featureMusic id="C-1" />
                    <killedBy id="D-1" potential="0.3" effect="1.0" />
                </entry>
            </document>"#,
            today(),
        )
        .unwrap();
        let a = entry(&doc, "A-1");
        assert_eq!(a.relations.len(), 3);
        assert!(a
            .relations
            .contains(&ParsedRelation::Remix { to: "B-1".into() }));
        assert!(a
            .relations
            .contains(&ParsedRelation::FeatureMusic { to: "C-1".into() }));
        assert!(a.relations.contains(&ParsedRelation::KilledBy {
            to: "D-1".into(),
            potential: 0.3,
            effect: 1.0
        }));
    }

    #[test]
    fn contributor_split_including_null_entry() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="A">
                    <cry emotions="MU">
                        <contributor id="A-1" factor="0.7" />
                        <contributor id="Progressive Crisis [null entry]" factor="0.3" />
                    </cry>
                </entry>
            </document>"#,
            today(),
        )
        .unwrap();
        let a = entry(&doc, "A-1");
        assert_eq!(
            a.impacts[0].contributors,
            vec![
                ("A-1".into(), 0.7),
                ("Progressive Crisis [null entry]".into(), 0.3)
            ]
        );
    }

    #[test]
    fn meme_date_range_computes_days() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="A">
                    <meme strength="0.5" from="2020-01-01" to="2020-01-11" />
                </entry>
            </document>"#,
            today(),
        )
        .unwrap();
        assert_eq!(
            entry(&doc, "A-1").impacts[0].kind,
            ImpactKind::Meme {
                strength: 0.5,
                days: 10.0,
                ranges: vec![PeriodRange {
                    from: "2020-01-01".into(),
                    to: "2020-01-11".into(),
                    from_expression: None,
                    to_expression: None,
                }],
            }
        );
    }

    #[test]
    fn meme_today_reference_resolves_against_fixed_date() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="A">
                    <meme strength="0.5" from="2020-01-01" to="today" />
                </entry>
            </document>"#,
            today(), // fixed at 2020-01-11
        )
        .unwrap();
        assert_eq!(
            entry(&doc, "A-1").impacts[0].kind,
            ImpactKind::Meme {
                strength: 0.5,
                days: 10.0,
                ranges: vec![PeriodRange {
                    from: "2020-01-01".into(),
                    to: "2020-01-11".into(),
                    from_expression: None,
                    to_expression: Some("today".into()),
                }],
            }
        );
    }

    #[test]
    fn missing_period_warns_and_defaults_to_zero_days() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="A"><waifu waifu="X" /></entry>
            </document>"#,
            today(),
        )
        .unwrap();
        assert_eq!(
            entry(&doc, "A-1").impacts[0].kind,
            ImpactKind::Waifu {
                days: 0.0,
                ranges: Vec::new(),
            }
        );
        assert!(doc
            .warnings
            .iter()
            .any(|w| w.contains("no usable length/from/to")));
    }

    #[test]
    fn role_is_carried() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="A"><role id="M-2" roles="vocal+image" /></entry>
            </document>"#,
            today(),
        )
        .unwrap();
        let a = entry(&doc, "A-1");
        assert_eq!(
            a.roles,
            vec![ParsedRole {
                id: "M-2".into(),
                roles_expr: "vocal+image".into()
            }]
        );
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    }

    #[test]
    fn musicvar_is_parsed() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="A"><musicvar feat="true" arrange="0.75" /></entry>
            </document>"#,
            today(),
        )
        .unwrap();
        let mv = entry(&doc, "A-1").music_var.unwrap();
        assert_eq!(mv.feat, Some(true));
        assert_eq!(mv.arrange, Some(0.75));
        assert_eq!(mv.vocallyrics, None);
    }

    #[cfg(not(feature = "scripting"))]
    #[test]
    fn script_element_warns_but_does_not_fail_parse() {
        let doc = parse_str(
            r#"<document>
                <entry id="A-1" title="A"><script>1+1</script><cry emotions="MU" /></entry>
            </document>"#,
            today(),
        )
        .unwrap();
        assert_eq!(entry(&doc, "A-1").impacts.len(), 1);
        assert!(doc.warnings.iter().any(|w| w.contains("<script>")));
    }

    #[test]
    fn def_ref_macro_is_expanded_before_entry_parsing() {
        let doc = parse_str(
            r#"<document>
                <def name="Greet" vars="a_base:double,a_emotions:string">
                    <nei base="a_base" emotions="a_emotions" />
                </def>
                <entry id="A-1" title="A">
                    <ref name="Greet" a_base="0.6" a_emotions="AP" />
                </entry>
            </document>"#,
            today(),
        )
        .unwrap();
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        let a = entry(&doc, "A-1");
        assert_eq!(a.impacts.len(), 1);
        // ImpactKind::Nei::base stores the raw attribute value; the
        // remap to [0,2] happens later in snapshot.rs's atoms_for.
        assert_eq!(
            a.impacts[0].kind,
            ImpactKind::Nei {
                base: 0.6,
                blend: vec![("AP".into(), 1.0)]
            }
        );
    }

    #[test]
    fn emotions_bare_code_defaults_to_full_weight() {
        assert_eq!(parse_emotions("MU"), vec![("MU".to_string(), 1.0)]);
    }

    #[test]
    fn emotions_split_parses_weighted_pairs() {
        assert_eq!(
            parse_emotions("CU-0.5:CP-0.5"),
            vec![("CU".to_string(), 0.5), ("CP".to_string(), 0.5)]
        );
    }

    #[test]
    fn duration_minutes_parses_mm_ss_and_hh_mm_ss() {
        assert_eq!(parse_duration_minutes("5:42"), Some(5.0 + 42.0 / 60.0));
        assert_eq!(parse_duration_minutes("1:05:42"), Some(65.0 + 42.0 / 60.0));
    }

    #[test]
    fn transform_id_replaces_dollar_with_enclosing_entry() {
        assert_eq!(transform_id("$-1", Some("M-ALBUM")), "M-ALBUM-1");
        assert_eq!(transform_id("M-MAL-123", Some("M-ALBUM")), "M-MAL-123");
        assert_eq!(transform_id("$-1", None), "$-1");
    }

    #[test]
    fn dollar_id_scoped_to_containing_entry_end_to_end() {
        let doc = parse_str(
            r#"<document>
                <entry id="M-ALBUM-A" title="Album A">
                    <entry id="$-1" title="Track A1" />
                </entry>
                <entry id="M-ALBUM-B" title="Album B">
                    <entry id="$-1" title="Track B1" />
                </entry>
            </document>"#,
            today(),
        )
        .unwrap();
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        assert!(doc.entries.iter().any(|e| e.id == "M-ALBUM-A-1"));
        assert!(doc.entries.iter().any(|e| e.id == "M-ALBUM-B-1"));
        // No entry should keep the raw, unresolved "$-1".
        assert!(!doc.entries.iter().any(|e| e.id == "$-1"));
    }

    #[test]
    fn duplicate_entry_id_declaration_warns() {
        let doc = parse_str(
            r#"<document>
                <entry id="M-1" title="First declaration" />
                <entry id="M-1" title="Second declaration" />
            </document>"#,
            today(),
        )
        .unwrap();
        assert_eq!(doc.entries.len(), 2);
        assert!(doc.warnings.iter().any(|w| w.contains("declared 2 times")));
    }
}
