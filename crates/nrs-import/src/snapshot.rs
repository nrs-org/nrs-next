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
    SourceId,
};

use crate::anchors;
use crate::legacy;
use crate::roles;
use crate::xml::{ImpactKind, ParsedDocument, ParsedRelation};

fn anchor_for_code(code: &str) -> Option<AnchorId> {
    match code {
        "AU" => Some(anchors::au()),
        "AP" => Some(anchors::ap()),
        "MU" => Some(anchors::mu()),
        "MP" => Some(anchors::mp()),
        "CU" => Some(anchors::cu()),
        "CP" => Some(anchors::cp()),
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
        "Emotion.AU" => anchors::au(),
        "Emotion.AP" => anchors::ap(),
        "Emotion.MU" => anchors::mu(),
        "Emotion.MP" => anchors::mp(),
        "Emotion.CU" => anchors::cu(),
        "Emotion.CP" => anchors::cp(),
        "Art.Language" => anchors::al(),
        "Art.Visual" => anchors::av(),
        "Art.Music" => anchors::am(),
        "Boredom" => anchors::boredom(),
        "Additional" => anchors::additional(),
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

fn atoms_for(
    kind: &ImpactKind,
    source: SourceId,
    context: &str,
    warnings: &mut Vec<String>,
) -> Vec<Atom> {
    match kind {
        ImpactKind::Cry { blend } => legacy::cry(source, &blend_anchors(blend, context, warnings)),
        ImpactKind::Nei { base, blend } => legacy::nei(
            source,
            base.abs(),
            base.signum(),
            &blend_anchors(blend, context, warnings),
        ),
        ImpactKind::Aei { base, blend } => legacy::aei(
            source,
            base.abs(),
            base.signum(),
            &blend_anchors(blend, context, warnings),
        ),
        ImpactKind::Pads { days, blend } => {
            legacy::pads(source, *days, &blend_anchors(blend, context, warnings))
        }
        ImpactKind::Waifu { days } => legacy::waifu(source, *days),
        ImpactKind::Ehi => legacy::ehi(source),
        ImpactKind::Epi { factor } => legacy::epi(source, *factor),
        ImpactKind::Jumpscare => legacy::jumpscare(source),
        ImpactKind::SleeplessNight => legacy::sleepless_night(source),
        ImpactKind::Politics => legacy::politics(source),
        ImpactKind::InterestField { new_field } => legacy::interest_field(source, *new_field),
        ImpactKind::Dropped => legacy::dropped(source),
        ImpactKind::Meme { strength, days } => {
            if !(0.0..2.0).contains(strength) {
                warnings.push(format!(
                    "{context}: meme strength {strength} out of [0, 2) range, skipped"
                ));
                return Vec::new();
            }
            legacy::meme(source, *strength, *days)
        }
        ImpactKind::Music { base } => legacy::music(source, *base),
        ImpactKind::Additional { value } => legacy::additional(source, *value),
        ImpactKind::Visual {
            visual_type,
            base,
            unique,
        } => match visual_type_factor(visual_type) {
            Some(factor) => legacy::visual(source, factor, *base, *unique),
            None => {
                warnings.push(format!(
                    "{context}: unrecognised visual type '{visual_type}', skipped"
                ));
                Vec::new()
            }
        },
        ImpactKind::OsuSong {
            personal,
            community,
        } => legacy::osu_song(source, *personal, *community),
        ImpactKind::Writing {
            character,
            story,
            pacing,
            originality,
        } => legacy::writing(source, *character, *story, *pacing, *originality),
        ImpactKind::AnimeConsumedProgress {
            boredom,
            episodes,
            episode_duration_minutes,
        } => legacy::anime_consumed(source, *boredom, *episodes, *episode_duration_minutes),
        ImpactKind::ConsumedProgress {
            boredom,
            duration_minutes,
        } => legacy::consumed(source, *boredom, *duration_minutes),
        ImpactKind::MusicConsumedProgress { duration_minutes } => {
            legacy::music_consumed_progress(source, *duration_minutes)
        }
        ImpactKind::Regular { components } => components
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
            .collect(),
    }
}

/// Build a `Snapshot` from a parsed document. Returns conversion-time
/// warnings in addition to whatever `xml::parse_file` already
/// collected during parsing (dangling contributor/relation ids,
/// out-of-range values, etc.) - callers should combine both.
pub fn build_snapshot(doc: &ParsedDocument) -> (Snapshot, Vec<String>) {
    let mut warnings = Vec::new();
    let known_ids: BTreeSet<&str> = doc.entries.iter().map(|e| e.id.as_str()).collect();

    let mut entries = Vec::with_capacity(doc.entries.len());
    let mut impacts = Vec::new();
    let mut relations = Vec::new();
    let mut impact_seq = 0usize;

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
            let base_atoms = atoms_for(
                &impact.kind,
                SourceId(parsed.id.clone()),
                &context,
                &mut warnings,
            );
            if base_atoms.is_empty() {
                continue;
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
                impact_seq += 1;
                impacts.push(Impact {
                    id: ImpactId(format!("i{impact_seq}")),
                    target: EntryId(contributor_id.clone()),
                    atoms,
                });
            }
        }

        for relation in &parsed.relations {
            let (to, built) = match relation {
                ParsedRelation::Remix { to } => (
                    to,
                    legacy::remix(EntryId(parsed.id.clone()), EntryId(to.clone())),
                ),
                ParsedRelation::FeatureMusic { to } => (
                    to,
                    legacy::feature_music(EntryId(parsed.id.clone()), EntryId(to.clone())),
                ),
                ParsedRelation::KilledBy {
                    to,
                    potential,
                    effect,
                } => (
                    to,
                    legacy::killed_by(
                        EntryId(parsed.id.clone()),
                        EntryId(to.clone()),
                        *potential,
                        *effect,
                    ),
                ),
                ParsedRelation::Contains { to, factor } => (
                    to,
                    legacy::contains_weighted(
                        EntryId(parsed.id.clone()),
                        EntryId(to.clone()),
                        *factor,
                    ),
                ),
            };
            if !known_ids.contains(to.as_str()) {
                warnings.push(format!(
                    "{context}: relation target '{to}' is not a known entry, relation dropped"
                ));
                continue;
            }
            relations.push(built);
        }

        if !parsed.roles.is_empty() {
            // Multiple <role> tags for the same id merge (their terms
            // concatenate, and weight_vector sums duplicate atomic
            // roles) rather than overwriting each other.
            let mut by_id: BTreeMap<&str, Vec<roles::RoleTerm>> = BTreeMap::new();
            for role in &parsed.roles {
                match roles::parse_role_expression(&role.roles_expr) {
                    Ok(terms) => by_id.entry(role.id.as_str()).or_default().extend(terms),
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
                relations.push(Relation {
                    kind: RelKind::Credit,
                    from: EntryId(contributor_id.to_string()),
                    to: EntryId(parsed.id.clone()),
                    transform,
                });
            }
        }
    }

    let taxonomy = nrs_core::derive_taxonomy(&relations);
    let snapshot = Snapshot {
        entries,
        impacts,
        relations,
        palette: Palette {
            anchors: Vec::new(),
            k_feat: Default::default(),
        },
        taxonomy,
    };
    (snapshot, warnings)
}
