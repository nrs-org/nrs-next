//! Turns a `ParsedDocument` (from `xml.rs`) into an `nrs_core::Snapshot`,
//! using `legacy.rs`'s formulas. A contributor/relation target id that
//! isn't a known entry (a "null entry" placeholder, or a genuinely
//! dangling reference) is dropped silently, matching the old engine's
//! own behaviour (`impactScores.get(entry)?.add(...)` no-ops for
//! unknown ids) - but bad *values* (a `meme` strength outside its
//! valid range, an unrecognised emotion code) are warned about, not
//! silently dropped, since those indicate a real data or parsing
//! problem rather than an intentional discard.

use std::collections::{BTreeMap, BTreeSet};

use nrs_core::{
    AnchorId, Atom, Entry, EntryId, Impact, ImpactId, Palette, RelKind, Relation, Snapshot,
    SourceId, Transform,
};

use crate::anchors;
use crate::legacy;
use crate::roles;
use crate::xml::{ImpactKind, ParsedDocument, ParsedImpact, ParsedRelation, PeriodRange};

/// `AU`/`MU`/`CP` are unambiguous 1:1 renames (`hate`/`discomfort`/
/// `cozy`); `AP`/`MP`/`CU` were overloaded boxes under the legacy
/// palette and resolve to a *default* sub-anchor (`comedy`/
/// `heartwarming`/`tragedy`) -- `is_anchor_ambiguous` below flags
/// exactly these three for the review UI to confirm or re-split.
fn anchor_for_code(code: &str) -> Option<AnchorId> {
    match code {
        "AU" => Some(anchors::hate()),
        "AP" => Some(anchors::comedy()),
        "MU" => Some(anchors::discomfort()),
        "MP" => Some(anchors::heartwarming()),
        "CU" => Some(anchors::tragedy()),
        "CP" => Some(anchors::cozy()),
        "comedy" => Some(anchors::comedy()),
        "hype" => Some(anchors::hype()),
        "tragedy" => Some(anchors::tragedy()),
        "wistful" => Some(anchors::wistful()),
        "cozy" => Some(anchors::cozy()),
        "heartwarming" => Some(anchors::heartwarming()),
        "hate" => Some(anchors::hate()),
        "discomfort" => Some(anchors::discomfort()),
        "waifu" => Some(anchors::waifu()),
        "meme" => Some(anchors::meme()),
        "hooked" => Some(anchors::hooked()),
        "worldview" => Some(anchors::worldview()),
        "gateway" => Some(anchors::gateway()),
        "writing" => Some(anchors::writing()),
        "visual" => Some(anchors::visual()),
        "music" => Some(anchors::music()),
        "boredom" => Some(anchors::boredom()),
        "additional" => Some(anchors::additional()),
        _ => None,
    }
}

fn blend_anchors(
    blend: &[(String, f32)],
    context: &str,
    warnings: &mut Vec<String>,
) -> Vec<(AnchorId, f32)> {
    blend
        .iter()
        .filter_map(|(code, weight)| match anchor_for_code(code) {
            Some(anchor) => Some((anchor, *weight)),
            None => {
                warnings.push(format!(
                    "{context}: unrecognised emotion code '{code}', dropped"
                ));
                None
            }
        })
        .collect()
}

/// `FactorScore` enumeration names (schema), as used by
/// `<regularImpact><score><component factor="..">`.
fn anchor_for_factor_name(name: &str) -> Option<AnchorId> {
    Some(match name {
        "Emotion.AU" => anchors::hate(),
        "Emotion.AP" => anchors::comedy(),
        "Emotion.MU" => anchors::discomfort(),
        "Emotion.MP" => anchors::heartwarming(),
        "Emotion.CU" => anchors::tragedy(),
        "Emotion.CP" => anchors::cozy(),
        "Art.Language" => anchors::writing(),
        "Art.Visual" => anchors::visual(),
        "Art.Music" => anchors::music(),
        "Boredom" => anchors::boredom(),
        "Additional" => anchors::additional(),
        "Anchor.comedy" => anchors::comedy(),
        "Anchor.hype" => anchors::hype(),
        "Anchor.tragedy" => anchors::tragedy(),
        "Anchor.wistful" => anchors::wistful(),
        "Anchor.cozy" => anchors::cozy(),
        "Anchor.heartwarming" => anchors::heartwarming(),
        "Anchor.hate" => anchors::hate(),
        "Anchor.discomfort" => anchors::discomfort(),
        "Anchor.waifu" => anchors::waifu(),
        "Anchor.meme" => anchors::meme(),
        "Anchor.hooked" => anchors::hooked(),
        "Anchor.worldview" => anchors::worldview(),
        "Anchor.gateway" => anchors::gateway(),
        "Anchor.writing" => anchors::writing(),
        "Anchor.visual" => anchors::visual(),
        "Anchor.music" => anchors::music(),
        "Anchor.boredom" => anchors::boredom(),
        "Anchor.additional" => anchors::additional(),
        _ => return None,
    })
}

/// `VisualType.factor` (`DAH_standards.ts`), by `VisualKind` name.
fn visual_type_factor(name: &str) -> Option<f32> {
    Some(match name {
        "animated" | "rpg3dGame" => 1.0,
        "animatedShort" | "animatedMV" | "visualNovel" | "manga" => 0.8,
        "animatedGachaCardArt" => 0.7,
        "gachaCardArt" => 0.6,
        "lightNovel" | "semiAnimatedMV" => 0.5,
        "staticMV" => 0.3,
        "albumArt" => 0.25,
        _ => return None,
    })
}

/// The exact literal string `IMPACT_KINDS[..]`'s `visual_kind` `<select>`
/// uses for each factor -- `visual_type_factor` only ever returns one
/// of these seven values, but plain `f32::to_string()` prints `1.0` as
/// `"1"` (Display trims the trailing zero), which wouldn't match the
/// option's `value="1.0"` and so wouldn't come back pre-selected.
fn format_visual_factor(f: f32) -> &'static str {
    const KNOWN: [(f32, &str); 7] = [
        (1.0, "1.0"),
        (0.8, "0.8"),
        (0.7, "0.7"),
        (0.6, "0.6"),
        (0.5, "0.5"),
        (0.3, "0.3"),
        (0.25, "0.25"),
    ];
    KNOWN
        .iter()
        .find(|(v, _)| (v - f).abs() < 1e-6)
        .map(|(_, s)| *s)
        .unwrap_or("0.25")
}

/// `anchor1`/`weight1`.. as nrs-app's `blend_slots()` form fields
/// expect them (already-resolved anchor names, not raw box codes) --
/// only the first 4 (the most the form supports, and the real data's
/// observed max).
fn add_blend_params(params: &mut BTreeMap<String, String>, anchors: &[(AnchorId, f32)]) {
    for (i, (anchor, weight)) in anchors.iter().enumerate().take(4) {
        params.insert(format!("anchor{}", i + 1), anchor.0.clone());
        params.insert(format!("weight{}", i + 1), weight.to_string());
    }
}

/// Preserve a single real calendar interval for the editor/timeline.
/// Multiple disjoint ranges intentionally stay duration-only until the
/// UI has a first-class multi-period editor; collapsing them into the
/// earliest/latest endpoints would falsely imply continuous coverage.
fn add_period_params(params: &mut BTreeMap<String, String>, ranges: &[PeriodRange]) {
    if let [range] = ranges {
        params.insert("period_from".into(), range.from.clone());
        params.insert("period_to".into(), range.to.clone());
        if let Some(expression) = &range.from_expression {
            params.insert("period_from_expression".into(), expression.clone());
        }
        if let Some(expression) = &range.to_expression {
            params.insert("period_to_expression".into(), expression.clone());
        }
    }
}

/// Dates a period kind's atoms when it has exactly one calendar period
/// (several disjoint periods stay undated, as `add_period_params` keeps
/// them duration-only).
fn with_period(atoms: Vec<Atom>, ranges: &[PeriodRange]) -> Vec<Atom> {
    match ranges {
        [range] => legacy::dated(atoms, &range.from, &range.to),
        _ => atoms,
    }
}

/// The DAH_standards kind slug + the exact form-field params that
/// kind's manual-entry form would submit -- see `atoms_for`.
type ImpactDah = (&'static str, BTreeMap<String, String>);

/// Builds the atoms *and* (when this kind has a DAH_standards
/// manual-entry form counterpart -- everything except the raw
/// `regularImpact` escape hatch) the exact form-field params that form
/// would submit, so nrs-app's kind-based editor works on every
/// imported impact, not only ones created through it. `None` alongside
/// empty atoms on a value error (the caller drops those impacts
/// entirely) or for `Regular`, which has no natural-parameter form to
/// map onto.
fn atoms_for(
    kind: &ImpactKind,
    source: SourceId,
    context: &str,
    warnings: &mut Vec<String>,
) -> (Vec<Atom>, Option<ImpactDah>) {
    let mut params = BTreeMap::new();
    params.insert("source".to_string(), source.0.clone());

    let (atoms, slug): (Vec<Atom>, Option<&'static str>) = match kind {
        ImpactKind::Cry { blend } => {
            let anchors = blend_anchors(blend, context, warnings);
            add_blend_params(&mut params, &anchors);
            (legacy::cry(source, &anchors), Some("cry"))
        }
        ImpactKind::Nei { base, blend } => {
            let anchors = blend_anchors(blend, context, warnings);
            params.insert("factor".into(), base.abs().to_string());
            params.insert("sign".into(), base.signum().to_string());
            add_blend_params(&mut params, &anchors);
            (
                legacy::nei(source, base.abs(), base.signum(), &anchors),
                Some("nei"),
            )
        }
        ImpactKind::Aei { base, blend } => {
            let anchors = blend_anchors(blend, context, warnings);
            params.insert("factor".into(), base.abs().to_string());
            params.insert("sign".into(), base.signum().to_string());
            add_blend_params(&mut params, &anchors);
            (
                legacy::aei(source, base.abs(), base.signum(), &anchors),
                Some("aei"),
            )
        }
        ImpactKind::Pads {
            days,
            ranges,
            blend,
        } => {
            let anchors = blend_anchors(blend, context, warnings);
            params.insert("days".into(), days.to_string());
            add_period_params(&mut params, ranges);
            add_blend_params(&mut params, &anchors);
            (
                with_period(legacy::pads(source, *days, &anchors), ranges),
                Some("pads"),
            )
        }
        ImpactKind::Waifu { days, ranges } => {
            params.insert("days".into(), days.to_string());
            add_period_params(&mut params, ranges);
            (
                with_period(legacy::waifu(source, *days), ranges),
                Some("waifu"),
            )
        }
        ImpactKind::Ehi { blend } => {
            let anchors = blend_anchors(blend, context, warnings);
            add_blend_params(&mut params, &anchors);
            let atoms = if anchors.is_empty() {
                legacy::ehi(source)
            } else {
                legacy::ehi_with_blend(source, &anchors)
            };
            (atoms, Some("ehi"))
        }
        ImpactKind::Epi { factor, blend } => {
            let anchors = blend_anchors(blend, context, warnings);
            params.insert("factor".into(), factor.to_string());
            add_blend_params(&mut params, &anchors);
            let atoms = if anchors.is_empty() {
                legacy::epi(source, *factor)
            } else {
                legacy::epi_with_blend(source, *factor, &anchors)
            };
            (atoms, Some("epi"))
        }
        ImpactKind::Jumpscare => (legacy::jumpscare(source), Some("jumpscare")),
        ImpactKind::SleeplessNight => (legacy::sleepless_night(source), Some("sleepless_night")),
        ImpactKind::Politics => (legacy::politics(source), Some("politics")),
        ImpactKind::InterestField { new_field } => {
            params.insert("new_field".into(), new_field.to_string());
            (
                legacy::interest_field(source, *new_field),
                Some("interest_field"),
            )
        }
        ImpactKind::Dropped => (legacy::dropped(source), Some("dropped")),
        ImpactKind::Meme {
            strength,
            days,
            ranges,
        } => {
            if !(0.0..2.0).contains(strength) {
                warnings.push(format!(
                    "{context}: meme strength {strength} out of [0, 2) range, skipped"
                ));
                (Vec::new(), None)
            } else {
                params.insert("strength".into(), strength.to_string());
                params.insert("days".into(), days.to_string());
                add_period_params(&mut params, ranges);
                (
                    with_period(legacy::meme(source, *strength, *days), ranges),
                    Some("meme"),
                )
            }
        }
        ImpactKind::Music { base } => {
            params.insert("music_base".into(), base.to_string());
            (legacy::music(source, *base), Some("music"))
        }
        ImpactKind::Additional { value } => {
            params.insert("value".into(), value.to_string());
            (legacy::additional(source, *value), Some("additional"))
        }
        ImpactKind::Visual {
            visual_type,
            base,
            unique,
        } => match visual_type_factor(visual_type) {
            Some(factor) => {
                params.insert("visual_kind".into(), format_visual_factor(factor).into());
                params.insert("base".into(), base.to_string());
                params.insert("unique".into(), unique.to_string());
                (
                    legacy::visual(source, factor, *base, *unique),
                    Some("visual"),
                )
            }
            None => {
                warnings.push(format!(
                    "{context}: unrecognised visual type '{visual_type}', skipped"
                ));
                (Vec::new(), None)
            }
        },
        ImpactKind::OsuSong {
            personal,
            community,
        } => {
            params.insert("personal".into(), personal.to_string());
            params.insert("community".into(), community.to_string());
            (
                legacy::osu_song(source, *personal, *community),
                Some("osu_song"),
            )
        }
        ImpactKind::Writing {
            character,
            story,
            pacing,
            originality,
        } => {
            params.insert("character_complexity".into(), character.to_string());
            params.insert("story_quality".into(), story.to_string());
            params.insert("pacing".into(), pacing.to_string());
            params.insert("originality".into(), originality.to_string());
            (
                legacy::writing(source, *character, *story, *pacing, *originality),
                Some("writing"),
            )
        }
        ImpactKind::AnimeConsumedProgress {
            boredom,
            episodes,
            episode_duration_minutes,
        } => {
            params.insert("boredom".into(), boredom.to_string());
            params.insert("episodes".into(), episodes.to_string());
            params.insert(
                "episode_duration_minutes".into(),
                episode_duration_minutes.to_string(),
            );
            (
                legacy::anime_consumed(source, *boredom, *episodes, *episode_duration_minutes),
                Some("anime_consumed"),
            )
        }
        ImpactKind::ConsumedProgress {
            boredom,
            duration_minutes,
        } => {
            params.insert("boredom".into(), boredom.to_string());
            params.insert("duration_minutes".into(), duration_minutes.to_string());
            (
                legacy::consumed(source, *boredom, *duration_minutes),
                Some("consumed"),
            )
        }
        ImpactKind::MusicConsumedProgress { duration_minutes } => {
            params.insert("duration_minutes".into(), duration_minutes.to_string());
            (
                legacy::music_consumed_progress(source, *duration_minutes),
                Some("music_consumed_progress"),
            )
        }
        ImpactKind::Regular { components } => {
            let atoms = components
                .iter()
                .filter_map(|(factor, value)| match anchor_for_factor_name(factor) {
                    Some(anchor) => Some(Atom {
                        source: source.clone(),
                        blend: vec![(anchor, 1.0)],
                        magnitude: *value,
                        time: nrs_core::TimeSupport::Moment,
                    }),
                    None => {
                        warnings.push(format!(
                            "{context}: unrecognised regularImpact factor '{factor}', dropped"
                        ));
                        None
                    }
                })
                .collect();
            // No DAH_standards form maps onto a raw regularImpact.
            (atoms, None)
        }
    };

    match slug {
        Some(slug) if !atoms.is_empty() => (atoms, Some((slug, params))),
        _ => (atoms, None),
    }
}

/// Comments and anchor-ambiguity flags collected alongside `Snapshot`
/// during `build_snapshot`, keyed by the `ImpactId`s it generated --
/// staging data for `nrs-store`'s review workflow, not part of the
/// frozen `Snapshot`/`Impact` contract itself.
#[derive(Debug, Default)]
pub struct BuildExtras {
    /// The `<!-- -->` block immediately preceding the impact tag, if
    /// any -- not yet a confirmed `impact.note`, just a suggestion.
    pub source_comments: BTreeMap<ImpactId, String>,
    /// Impacts whose anchor came from one of the historically
    /// overloaded boxes (`AP`/`MP`/`CU`) or an `epi`/`ehi` "legendary
    /// moment" marker -- these got a *default* anchor, not a confident
    /// one, and want a human look via the review UI.
    pub needs_review: BTreeSet<ImpactId>,
    /// The raw `roles="..."` expression(s) (`DAH_entry_roles`) that
    /// produced each Credit relation's transform -- multiple `<role>`
    /// tags for the same contributor on the same work merge into one
    /// relation (`roles::weight_vector` sums their terms), so this
    /// joins their source expressions with "; ". Keyed by (contributor,
    /// work) since `Relation` carries no id of its own. Lets the
    /// Credit UI show "compose*0.6+arrange*0.4" instead of only the
    /// resulting weight vector.
    pub credit_roles: BTreeMap<(EntryId, EntryId), String>,
    /// The DAH_standards kind + the exact form params `atoms_for`
    /// derived it from, plus this impact's `<contributor factor="..">`
    /// share of the formula's full-magnitude result (1.0 when there's
    /// no split) -- a fanned-out multi-contributor impact keeps the
    /// same kind/params as its siblings, just a different share. Lets
    /// nrs-app's kind-based editor work on every DAH-mappable
    /// bulk-imported impact, not only ones created through it: it
    /// recomputes the formula from `params`, then multiplies by
    /// `factor` before saving, so re-editing never silently discards
    /// the share.
    pub impact_dah: BTreeMap<ImpactId, (&'static str, BTreeMap<String, String>, f32)>,
}

/// Whether `kind`'s anchor was a default guess rather than a confident
/// classification -- see `BuildExtras::needs_review`.
fn is_anchor_ambiguous(kind: &ImpactKind) -> bool {
    const OVERLOADED: [&str; 3] = ["AP", "MP", "CU"];
    const OVERLOADED_FACTORS: [&str; 3] = ["Emotion.AP", "Emotion.MP", "Emotion.CU"];
    match kind {
        ImpactKind::Cry { blend }
        | ImpactKind::Nei { blend, .. }
        | ImpactKind::Aei { blend, .. }
        | ImpactKind::Pads { blend, .. } => blend
            .iter()
            .any(|(code, _)| OVERLOADED.contains(&code.as_str())),
        ImpactKind::Epi { blend, .. } | ImpactKind::Ehi { blend } => {
            blend.is_empty()
                || blend
                    .iter()
                    .any(|(code, _)| OVERLOADED.contains(&code.as_str()))
        }
        ImpactKind::Regular { components } => components
            .iter()
            .any(|(factor, _)| OVERLOADED_FACTORS.contains(&factor.as_str())),
        _ => false,
    }
}

/// Build a `Snapshot` from a parsed document. Returns conversion-time
/// warnings in addition to whatever `xml::parse_file` already
/// collected during parsing (dangling contributor/relation ids,
/// out-of-range values, etc.) - callers should combine both.
pub fn build_snapshot(doc: &ParsedDocument) -> (Snapshot, BuildExtras, Vec<String>) {
    let mut warnings = Vec::new();
    let known_ids: BTreeSet<&str> = doc.entries.iter().map(|e| e.id.as_str()).collect();

    let mut entries = Vec::with_capacity(doc.entries.len());
    let mut impacts = Vec::new();
    let mut relations = Vec::new();
    let mut extras = BuildExtras::default();

    for parsed in &doc.entries {
        entries.push(Entry {
            id: EntryId(parsed.id.clone()),
            store: parsed.store.clone(),
            store_id: parsed.store_id.clone(),
            title_cache: if parsed.title.is_empty() {
                None
            } else {
                Some(parsed.title.clone())
            },
        });

        let context = format!("entry {}", parsed.id);
        for impact in &parsed.impacts {
            push_impact(
                impact,
                SourceId(parsed.id.clone()),
                &context,
                &known_ids,
                &mut impacts,
                &mut extras,
                &mut warnings,
            );
        }

        for relation in &parsed.relations {
            if !known_ids.contains(relation.target()) {
                warnings.push(format!(
                    "{context}: relation target '{}' is not a known entry, relation dropped",
                    relation.target()
                ));
                continue;
            }
            relations.push(build_relation(relation, EntryId(parsed.id.clone())));
        }

        if !parsed.roles.is_empty() {
            // Multiple <role> tags for the same id merge (their terms
            // concatenate, and weight_vector sums duplicate atomic
            // roles) rather than overwriting each other.
            let mut by_id: BTreeMap<&str, Vec<roles::RoleTerm>> = BTreeMap::new();
            let mut raw_exprs_by_id: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
            for role in &parsed.roles {
                match roles::parse_role_expression(&role.roles_expr) {
                    Ok(terms) => {
                        by_id.entry(role.id.as_str()).or_default().extend(terms);
                        raw_exprs_by_id
                            .entry(role.id.as_str())
                            .or_default()
                            .push(role.roles_expr.as_str());
                    }
                    Err(e) => warnings.push(format!(
                        "{context}: role expression '{}' for '{}': {e}",
                        role.roles_expr, role.id
                    )),
                }
            }

            let mentions_feat = by_id
                .values()
                .any(|terms| roles::mentions_image_feat(terms));
            let title_has_feat = parsed.title.contains("feat.") || parsed.title.contains("ft.");
            let mut vars = roles::MusicVars {
                feat: mentions_feat || title_has_feat,
                ..Default::default()
            };
            if let Some(mv) = parsed.music_var {
                if let Some(v) = mv.vocallyrics {
                    vars.vocallyrics = v;
                }
                if let Some(v) = mv.lyricsmusic {
                    vars.lyricsmusic = v;
                }
                if let Some(v) = mv.emolyrics {
                    vars.emolyrics = v;
                }
                if let Some(v) = mv.arrange {
                    vars.arrange = v;
                }
                if let Some(v) = mv.instperform {
                    vars.instperform = v;
                }
                if let Some(v) = mv.feat {
                    vars.feat = v;
                }
            }

            for (contributor_id, terms) in by_id {
                if !known_ids.contains(contributor_id) {
                    warnings.push(format!(
                        "{context}: role credit target '{contributor_id}' is not a known entry, dropped"
                    ));
                    continue;
                }
                let transform = roles::vector_to_transform(roles::weight_vector(&terms, &vars));
                let from = EntryId(contributor_id.to_string());
                let to = EntryId(parsed.id.clone());
                if let Some(raws) = raw_exprs_by_id.get(contributor_id) {
                    extras
                        .credit_roles
                        .insert((from.clone(), to.clone()), raws.join("; "));
                }
                relations.push(Relation {
                    kind: RelKind::Credit,
                    from,
                    to,
                    transform,
                });
            }
        }
    }

    for impact in &doc.shared_impacts {
        // A document-level impact has no enclosing entry to be its
        // source. Its largest contributor stands in, so every share of
        // the one event keeps the same source (a franchise containing
        // several sharers then sums them rather than docking them as
        // redundant siblings).
        let Some(source) = primary_contributor(&impact.contributors, &known_ids) else {
            warnings.push("document: shared impact has no known contributor, dropped".to_string());
            continue;
        };
        push_impact(
            impact,
            SourceId(source.to_string()),
            "document",
            &known_ids,
            &mut impacts,
            &mut extras,
            &mut warnings,
        );
    }

    for shared in &doc.shared_relations {
        let to = shared.relation.target();
        if !known_ids.contains(to) {
            warnings.push(format!(
                "document: relation target '{to}' is not a known entry, relation dropped"
            ));
            continue;
        }
        for (contributor_id, factor) in &shared.contributors {
            if !known_ids.contains(contributor_id.as_str()) {
                warnings.push(format!(
                    "document: relation contributor '{contributor_id}' is not a known entry, share dropped"
                ));
                continue;
            }
            let mut relation = build_relation(&shared.relation, EntryId(contributor_id.clone()));
            scale_transform(&mut relation.transform, *factor);
            relations.push(relation);
        }
    }

    let snapshot = Snapshot {
        entries,
        impacts,
        relations,
        palette: Palette {
            anchors: Vec::new(),
            k_feat: crate::anchors::legacy_k_feat(),
        },
    };
    (snapshot, extras, warnings)
}

/// Splits one parsed impact into a per-contributor `Impact`, each
/// scaled by its contributor's factor and sourced from `source`.
fn push_impact(
    impact: &ParsedImpact,
    source: SourceId,
    context: &str,
    known_ids: &BTreeSet<&str>,
    impacts: &mut Vec<Impact>,
    extras: &mut BuildExtras,
    warnings: &mut Vec<String>,
) {
    let (base_atoms, dah) = atoms_for(&impact.kind, source, context, warnings);
    if base_atoms.is_empty() {
        return;
    }
    for (contributor_id, factor) in &impact.contributors {
        if !known_ids.contains(contributor_id.as_str()) {
            warnings.push(format!(
                "{context}: contributor '{contributor_id}' is not a known entry (null-entry placeholder or dangling reference), share dropped"
            ));
            continue;
        }
        let atoms: Vec<Atom> = base_atoms
            .iter()
            .cloned()
            .map(|mut atom| {
                atom.magnitude *= factor;
                atom
            })
            .collect();
        let id = ImpactId(format!("i{}", impacts.len() + 1));
        if let Some(comment) = &impact.source_comment {
            extras.source_comments.insert(id.clone(), comment.clone());
        }
        if is_anchor_ambiguous(&impact.kind)
            || impact
                .anchor_confidence
                .is_some_and(|confidence| confidence < 0.8)
        {
            extras.needs_review.insert(id.clone());
        }
        if let Some((kind_slug, params)) = &dah {
            extras
                .impact_dah
                .insert(id.clone(), (kind_slug, params.clone(), *factor));
        }
        impacts.push(Impact {
            id,
            target: EntryId(contributor_id.clone()),
            atoms,
        });
    }
}

/// The known contributor with the largest factor (the first, on a tie).
fn primary_contributor<'a>(
    contributors: &'a [(String, f32)],
    known_ids: &BTreeSet<&str>,
) -> Option<&'a str> {
    let mut best: Option<(&str, f32)> = None;
    for (id, factor) in contributors {
        if known_ids.contains(id.as_str()) && best.is_none_or(|(_, f)| *factor > f) {
            best = Some((id, *factor));
        }
    }
    best.map(|(id, _)| id)
}

fn build_relation(relation: &ParsedRelation, from: EntryId) -> Relation {
    let to = EntryId(relation.target().to_string());
    match relation {
        ParsedRelation::Remix { .. } => legacy::remix(from, to),
        ParsedRelation::FeatureMusic { .. } => legacy::feature_music(from, to),
        ParsedRelation::KilledBy {
            potential, effect, ..
        } => legacy::killed_by(from, to, *potential, *effect),
        ParsedRelation::Contains { factor, .. } => legacy::contains_weighted(from, to, *factor),
    }
}

/// A contributor's share of a relation: the real engine's
/// `createRelationMaps` multiplies the contributor weight into the
/// relation's own matrix.
fn scale_transform(transform: &mut Transform, factor: f32) {
    match transform {
        Transform::Scalar(c) => *c *= factor,
        Transform::Regional(map) => map.values_mut().for_each(|w| *w *= factor),
        Transform::Transport(map) => map.values_mut().for_each(|w| *w *= factor),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blend(codes: &[&str]) -> Vec<(String, f32)> {
        codes.iter().map(|c| (c.to_string(), 1.0)).collect()
    }

    #[test]
    fn overloaded_boxes_are_ambiguous() {
        assert!(is_anchor_ambiguous(&ImpactKind::Nei {
            base: 0.5,
            blend: blend(&["AP"]),
        }));
        assert!(is_anchor_ambiguous(&ImpactKind::Aei {
            base: 0.5,
            blend: blend(&["MP"]),
        }));
        assert!(is_anchor_ambiguous(&ImpactKind::Cry {
            blend: blend(&["CU"]),
        }));
        // Mixed blend: any overloaded code taints the whole impact.
        assert!(is_anchor_ambiguous(&ImpactKind::Nei {
            base: 0.5,
            blend: blend(&["CP", "AP"]),
        }));
        assert!(is_anchor_ambiguous(&ImpactKind::Epi {
            factor: 1.0,
            blend: Vec::new(),
        }));
        assert!(is_anchor_ambiguous(&ImpactKind::Ehi { blend: Vec::new() }));
    }

    #[test]
    fn unambiguous_boxes_are_not_flagged() {
        assert!(!is_anchor_ambiguous(&ImpactKind::Nei {
            base: 0.5,
            blend: blend(&["AU"]),
        }));
        assert!(!is_anchor_ambiguous(&ImpactKind::Aei {
            base: 0.5,
            blend: blend(&["MU"]),
        }));
        assert!(!is_anchor_ambiguous(&ImpactKind::Cry {
            blend: blend(&["CP"]),
        }));
        assert!(!is_anchor_ambiguous(&ImpactKind::Waifu {
            days: 30.0,
            ranges: Vec::new(),
        }));
        assert!(!is_anchor_ambiguous(&ImpactKind::Meme {
            strength: 0.5,
            days: 10.0,
            ranges: Vec::new(),
        }));
        assert!(!is_anchor_ambiguous(&ImpactKind::Ehi {
            blend: blend(&["hype"]),
        }));
    }
}
