//! Which impacts an entry of each kind is expected to carry, and the
//! anti-patterns `nrs-validator` used to catch on the legacy XML, run
//! over the store instead. Rule names are the validator's, so the
//! XML's `<validatorSuppress>` waivers (imported into the `waiver`
//! table) still apply.
//!
//! The profile is the boilerplate every entry of a kind gets: an anime
//! is consumed (episodes), and has writing and visual impacts; a track
//! is consumed (its length) and has a music impact. Entries still
//! planned owe nothing yet; ones in progress owe only what progress
//! already tracks.

use std::collections::{BTreeSet, HashMap};

use nrs_store::{Progress, ProgressStatus, Waiver};
use serde::Serialize;

use crate::MUSIC_KINDS;

/// The impact kinds that count as having consumed an entry.
const CONSUMED_KINDS: &[&str] = &["consumed", "anime_consumed", "music_consumed_progress"];

/// `visual_kind` of an album-art visual, and of an animated one.
const VISUAL_ALBUM_ART: f32 = 0.25;
const VISUAL_ANIMATED: f32 = 1.0;

/// One impact an entry of some kind is expected to have.
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct Slot {
    /// The DAH kind that fills it.
    pub kind: &'static str,
    pub label: &'static str,
    /// The rule that flags it missing; `None` for an optional slot.
    pub rule: Option<&'static str>,
    /// Filled by setting the entry's progress rather than by hand: its
    /// impact follows the progress row.
    pub via_progress: bool,
    /// Form values to start a new impact of this kind from.
    pub defaults: &'static [(&'static str, &'static str)],
}

const fn slot(
    kind: &'static str,
    label: &'static str,
    rule: Option<&'static str>,
    via_progress: bool,
    defaults: &'static [(&'static str, &'static str)],
) -> Slot {
    Slot {
        kind,
        label,
        rule,
        via_progress,
        defaults,
    }
}

const NO_CONSUMED: &str = "dah-entry-no-consumed";
const NO_WRITING: &str = "dah-no-writing-impact";
const NO_VISUAL: &str = "dah-visualless-entry";
const NO_MUSIC: &str = "dah-no-music-impact";
const NO_PROGRESS: &str = "dah-no-progress";
const NO_DROPPED: &str = "dah-no-dropped-impact";
const LONE_PADS: &str = "dah-lone-pads";
const VISUAL_CHECKS: &str = "dah-visual-checks";

/// Every rule `check` can raise, for the filter language and the UI.
pub(crate) const RULES: &[&str] = &[
    NO_CONSUMED,
    NO_WRITING,
    NO_VISUAL,
    NO_MUSIC,
    NO_PROGRESS,
    NO_DROPPED,
    LONE_PADS,
    VISUAL_CHECKS,
];

/// Older spellings of a rule that legacy waivers still use.
fn canonical_rule(rule: &str) -> &str {
    match rule {
        "dah-no-visual" => NO_VISUAL,
        rule => rule,
    }
}

/// Whether `check` runs `rule` (or an older spelling of it); waivers
/// for the validator's other rules are kept but excuse nothing here.
pub(crate) fn is_checked(rule: &str) -> bool {
    RULES.contains(&canonical_rule(rule))
}

const WRITING: Slot = slot("writing", "Writing", Some(NO_WRITING), false, &[]);

const ANIME: &[Slot] = &[
    slot("anime_consumed", "Consumed", Some(NO_CONSUMED), true, &[]),
    WRITING,
    slot("visual", "Visual", Some(NO_VISUAL), false, &[("visual_kind", "1.0")]),
];
const GAME: &[Slot] = &[
    slot("consumed", "Consumed", Some(NO_CONSUMED), true, &[]),
    WRITING,
    slot("visual", "Visual", Some(NO_VISUAL), false, &[("visual_kind", "1.0")]),
];
const READ: &[Slot] = &[
    slot("consumed", "Consumed", Some(NO_CONSUMED), true, &[]),
    WRITING,
    slot("visual", "Visual", Some(NO_VISUAL), false, &[("visual_kind", "0.8")]),
];
const TRACK: &[Slot] = &[
    slot("music_consumed_progress", "Consumed", Some(NO_CONSUMED), false, &[]),
    slot("music", "Music", Some(NO_MUSIC), false, &[]),
    slot("visual", "Visual (MV)", None, false, &[("visual_kind", "0.8")]),
];
const ALBUM: &[Slot] = &[slot(
    "visual",
    "Album art",
    Some(NO_VISUAL),
    false,
    &[("visual_kind", "0.25")],
)];

/// What an entry of `kind` is expected to carry, in display order.
pub(crate) fn profile(kind: &str) -> &'static [Slot] {
    match kind {
        "anime" => ANIME,
        "game" => GAME,
        "vn" | "literature" => READ,
        "track" => TRACK,
        "album" => ALBUM,
        _ => &[],
    }
}

/// Kinds whose consumption progress tracks (`dah-no-progress`).
fn tracks_progress(kind: &str) -> bool {
    matches!(kind, "anime" | "game" | "vn" | "literature")
}

/// What the rules look at for one entry.
#[derive(Debug, Default)]
pub(crate) struct Facts {
    /// DAH kind -> how many impacts of it the entry has (`None` kinds,
    /// raw impacts, aren't counted).
    pub impacts: HashMap<String, usize>,
    /// The `visual_kind` of each visual impact.
    pub visual_kinds: Vec<f32>,
    pub progress: Option<(ProgressStatus, bool)>,
    pub waivers: Vec<Waiver>,
}

impl Facts {
    fn has(&self, kind: &str) -> bool {
        self.impacts.get(kind).is_some_and(|count| *count > 0)
    }

    /// Whether the slot holding `kind` is filled.
    pub(crate) fn fills(&self, kind: &str) -> bool {
        if CONSUMED_KINDS.contains(&kind) {
            CONSUMED_KINDS.iter().any(|kind| self.has(kind))
        } else {
            self.has(kind)
        }
    }

    pub(crate) fn count(&self, kind: &str) -> usize {
        if CONSUMED_KINDS.contains(&kind) {
            CONSUMED_KINDS
                .iter()
                .map(|kind| self.impacts.get(*kind).copied().unwrap_or(0))
                .sum()
        } else {
            self.impacts.get(kind).copied().unwrap_or(0)
        }
    }

    pub(crate) fn waived(&self, rule: &str) -> bool {
        self.waivers
            .iter()
            .any(|waiver| canonical_rule(&waiver.rule) == rule)
    }
}

/// Every entry's facts, from the store's rows. Entries with nothing to
/// say are absent.
pub(crate) fn facts_by_entry(
    impacts: Vec<(String, Option<String>, Option<String>)>,
    progress: Vec<Progress>,
    waivers: Vec<Waiver>,
) -> HashMap<String, Facts> {
    let mut facts: HashMap<String, Facts> = HashMap::new();
    for (entry, kind, params) in impacts {
        let Some(kind) = kind else { continue };
        let entry = facts.entry(entry).or_default();
        if kind == "visual" {
            let visual_kind = params
                .as_deref()
                .and_then(|params| serde_json::from_str::<serde_json::Value>(params).ok())
                .and_then(|params| match &params["visual_kind"] {
                    serde_json::Value::String(value) => value.parse().ok(),
                    value => value.as_f64().map(|value| value as f32),
                });
            if let Some(visual_kind) = visual_kind {
                entry.visual_kinds.push(visual_kind);
            }
        }
        *entry.impacts.entry(kind).or_default() += 1;
    }
    for row in progress {
        facts.entry(row.entry.0).or_default().progress = Some((row.status, row.scored));
    }
    for waiver in waivers {
        facts
            .entry(waiver.entry.0.clone())
            .or_default()
            .waivers
            .push(waiver);
    }
    facts
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Issue {
    pub rule: &'static str,
    pub message: String,
    /// The profile slot (DAH kind) that would resolve it.
    pub slot: Option<&'static str>,
}

fn issue(rule: &'static str, message: impl Into<String>, slot: Option<&'static str>) -> Issue {
    Issue {
        rule,
        message: message.into(),
        slot,
    }
}

/// Every rule `kind`/`facts` break, waived or not.
fn all_issues(kind: &str, facts: &Facts) -> Vec<Issue> {
    let mut issues = Vec::new();
    let status = facts.progress.map(|(status, _)| status);
    let scored = facts.progress.is_none_or(|(_, scored)| scored);
    // Not started: nothing is owed. Under way: only consumption, which
    // progress keeps in step, unless that's been switched off.
    let owes_consumed = status != Some(ProgressStatus::Planned) && scored;
    let owes_profile = !matches!(
        status,
        Some(ProgressStatus::Planned | ProgressStatus::Active | ProgressStatus::Paused)
    );

    for slot in profile(kind) {
        let Some(rule) = slot.rule else { continue };
        let owed = if rule == NO_CONSUMED {
            owes_consumed
        } else {
            owes_profile
        };
        if owed && !facts.fills(slot.kind) {
            let what = slot.label.to_lowercase();
            issues.push(issue(rule, format!("no {what} impact"), Some(slot.kind)));
        }
    }

    if tracks_progress(kind) && facts.progress.is_none() {
        issues.push(issue(NO_PROGRESS, "no progress", None));
    }

    let dropped = status == Some(ProgressStatus::Dropped);
    match (dropped, facts.has("dropped")) {
        (true, false) => issues.push(issue(NO_DROPPED, "dropped, but no dropped impact", None)),
        (false, true) => issues.push(issue(
            NO_DROPPED,
            "has a dropped impact, but its progress isn't dropped",
            None,
        )),
        _ => {}
    }

    // The validator flagged every standalone `<pads>`; the DSL's
    // `cryPADS`/`maxAEIPADS` paired one with its cry or AEI. Here that
    // pairing shows as a cry or AEI on the same entry.
    if facts.has("pads") && !facts.has("cry") && !facts.has("aei") {
        issues.push(issue(
            LONE_PADS,
            "PADS without an accompanying cry or AEI",
            None,
        ));
    }

    let music = MUSIC_KINDS.contains(&kind);
    let close = |a: f32, b: f32| (a - b).abs() < 1e-4;
    if !music && facts.visual_kinds.iter().any(|v| close(*v, VISUAL_ALBUM_ART)) {
        issues.push(issue(
            VISUAL_CHECKS,
            "album-art visual on a non-music entry",
            None,
        ));
    }
    if music && facts.visual_kinds.iter().any(|v| close(*v, VISUAL_ANIMATED)) {
        issues.push(issue(VISUAL_CHECKS, "animated visual on a music entry", None));
    }
    issues
}

/// The rules `kind`/`facts` break that aren't waived.
pub(crate) fn issues(kind: &str, facts: &Facts) -> Vec<Issue> {
    let mut issues = all_issues(kind, facts);
    issues.retain(|issue| !facts.waived(issue.rule));
    issues
}

/// Rule names of the waivers for checked rules that no longer excuse
/// anything: the rule would pass without them (the validator made
/// these an error).
pub(crate) fn unused_waivers<'a>(kind: &str, facts: &'a Facts) -> BTreeSet<&'a str> {
    let firing: BTreeSet<&str> = all_issues(kind, facts)
        .iter()
        .map(|issue| issue.rule)
        .collect();
    facts
        .waivers
        .iter()
        .map(|waiver| waiver.rule.as_str())
        .filter(|rule| is_checked(rule) && !firing.contains(canonical_rule(rule)))
        .collect()
}

#[cfg(test)]
mod tests {
    use nrs_core::EntryId;

    use super::*;

    fn facts(kinds: &[&str], progress: Option<ProgressStatus>) -> Facts {
        let mut facts = Facts::default();
        for kind in kinds {
            *facts.impacts.entry(kind.to_string()).or_default() += 1;
        }
        facts.progress = progress.map(|status| (status, true));
        facts
    }

    fn rules(kind: &str, facts: &Facts) -> Vec<&'static str> {
        issues(kind, facts).into_iter().map(|issue| issue.rule).collect()
    }

    #[test]
    fn completed_anime_owes_its_whole_profile() {
        let bare = facts(&[], Some(ProgressStatus::Completed));
        assert_eq!(rules("anime", &bare), [NO_CONSUMED, NO_WRITING, NO_VISUAL]);
        let full = facts(
            &["anime_consumed", "writing", "visual"],
            Some(ProgressStatus::Completed),
        );
        assert!(rules("anime", &full).is_empty());
    }

    #[test]
    fn progress_status_decides_what_is_owed() {
        assert!(rules("anime", &facts(&[], Some(ProgressStatus::Planned))).is_empty());
        assert_eq!(
            rules("anime", &facts(&[], Some(ProgressStatus::Active))),
            [NO_CONSUMED]
        );
        assert_eq!(
            rules("anime", &facts(&["consumed", "writing", "visual"], None)),
            [NO_PROGRESS]
        );
    }

    #[test]
    fn track_owes_consumption_and_music_but_not_visual() {
        assert_eq!(rules("track", &facts(&[], None)), [NO_CONSUMED, NO_MUSIC]);
        assert!(rules("track", &facts(&["music_consumed_progress", "music"], None)).is_empty());
        assert!(rules("artist", &facts(&[], None)).is_empty());
        assert_eq!(rules("album", &facts(&[], None)), [NO_VISUAL]);
    }

    #[test]
    fn dropped_status_and_impact_must_agree() {
        let base = ["anime_consumed", "writing", "visual"];
        let dropped = facts(&base, Some(ProgressStatus::Dropped));
        assert_eq!(rules("anime", &dropped), [NO_DROPPED]);
        let mut with = base.to_vec();
        with.push("dropped");
        assert!(rules("anime", &facts(&with, Some(ProgressStatus::Dropped))).is_empty());
        assert_eq!(
            rules("anime", &facts(&with, Some(ProgressStatus::Completed))),
            [NO_DROPPED]
        );
    }

    #[test]
    fn pads_needs_a_cry_or_aei_beside_it() {
        assert_eq!(rules("other", &facts(&["pads"], None)), [LONE_PADS]);
        assert!(rules("other", &facts(&["pads", "aei"], None)).is_empty());
    }

    #[test]
    fn visual_kind_must_suit_the_medium() {
        let mut anime = facts(
            &["anime_consumed", "writing", "visual"],
            Some(ProgressStatus::Completed),
        );
        anime.visual_kinds = vec![0.25];
        assert_eq!(rules("anime", &anime), [VISUAL_CHECKS]);
        let mut album = facts(&["visual"], None);
        album.visual_kinds = vec![1.0];
        assert_eq!(rules("album", &album), [VISUAL_CHECKS]);
    }

    #[test]
    fn waivers_silence_rules_and_unused_ones_are_reported() {
        let mut facts = facts(&["anime_consumed", "visual"], Some(ProgressStatus::Completed));
        let waiver = |rule: &str| Waiver {
            entry: EntryId("e".into()),
            rule: rule.into(),
            reason: None,
        };
        facts.impacts.remove("visual");
        facts.waivers = vec![
            waiver(NO_WRITING),
            waiver(LONE_PADS),
            waiver("dah-no-visual"),
            waiver("dah-no-best-girl"),
        ];
        assert!(rules("anime", &facts).is_empty());
        assert_eq!(
            unused_waivers("anime", &facts).into_iter().collect::<Vec<_>>(),
            [LONE_PADS]
        );
    }

    #[test]
    fn facts_read_visual_kinds_from_params() {
        let facts = facts_by_entry(
            vec![
                ("e".into(), Some("visual".into()), Some(r#"{"visual_kind":"0.25"}"#.into())),
                ("e".into(), Some("writing".into()), None),
                ("e".into(), None, None),
            ],
            vec![],
            vec![],
        );
        assert_eq!(facts["e"].visual_kinds, [0.25]);
        assert_eq!(facts["e"].count("writing"), 1);
    }
}
