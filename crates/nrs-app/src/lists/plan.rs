//! Runs `want`/`diff` over one site: matches NRS progress rows to the
//! user's list entries, asks the site about rows not on the list yet, and
//! reports what's on one side only.

use std::collections::{HashMap, HashSet};

use nrs_core::EntryId;
use nrs_store::{Progress, Store};

use crate::lists::score::{on_scale, scores};
use crate::lists::{diff, site_ref, want, ListEntry, Medium, Patch, Result, SiteRef, Totals};

/// One entry on the user's list.
#[derive(Debug, Clone)]
pub struct Listed {
    /// What `Site::save` takes.
    pub id: u64,
    /// Every ref that names the entry (AniList's include its MAL id).
    pub refs: Vec<SiteRef>,
    pub title: String,
    pub entry: ListEntry,
    pub totals: Totals,
}

/// An entry the site has, not on the user's list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Found {
    pub id: u64,
    pub totals: Totals,
}

pub trait Site {
    fn name(&self) -> &'static str;
    /// The top of the site's score scale.
    fn points(&self) -> u32;
    fn list(&mut self, medium: Medium) -> Result<Vec<Listed>>;
    /// The site's entries for `refs`, all of one medium; a ref missing
    /// from the result is one the site doesn't have.
    fn find(&mut self, refs: &[SiteRef]) -> Result<HashMap<SiteRef, Found>>;
    fn save(&mut self, medium: Medium, id: u64, patch: &Patch) -> Result<()>;
}

/// An NRS progress row, with what it's bound by (rows with no `site_ref`
/// are skipped).
#[derive(Debug, Clone)]
pub struct Row {
    pub title: String,
    pub site_ref: Option<SiteRef>,
    pub progress: Progress,
    /// 1-10 (`score::scores`).
    pub score: Option<f32>,
}

/// `entry`'s row, unscored (a live push doesn't send scores, and scoring
/// takes the whole snapshot).
pub fn row(entry: &nrs_core::Entry, progress: Progress) -> Row {
    Row {
        title: entry.title_cache.clone().unwrap_or_else(|| entry.store_id.clone()),
        site_ref: site_ref(entry),
        progress,
        score: None,
    }
}

/// Every progress row in `store`, scored.
pub fn rows(store: &Store) -> std::result::Result<Vec<Row>, String> {
    let (snapshot, params) = store.load_snapshot().map_err(|e| e.to_string())?;
    let scores = scores(&snapshot, &params)?;
    let entries: HashMap<&EntryId, _> =
        snapshot.entries.iter().map(|entry| (&entry.id, entry)).collect();
    let progress = store.list_progress().map_err(|e| e.to_string())?;
    Ok(progress
        .into_iter()
        .filter_map(|progress| {
            let entry = entries.get(&progress.entry)?;
            Some(Row {
                title: entry.title_cache.clone().unwrap_or_else(|| entry.store_id.clone()),
                site_ref: site_ref(entry),
                score: scores.get(&progress.entry).copied(),
                progress,
            })
        })
        .collect())
}

#[derive(Debug, Clone)]
pub struct Change {
    /// The row's entry, and what it's bound by.
    pub entry: EntryId,
    pub site_ref: SiteRef,
    pub medium: Medium,
    pub id: u64,
    pub title: String,
    /// `None`: not on the list yet.
    pub have: Option<ListEntry>,
    pub patch: Patch,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub changes: Vec<Change>,
    pub in_sync: usize,
    /// Rows bound by a MAL/AniList URL the site has no entry for.
    pub unknown: Vec<String>,
    /// Rows naming the same site entry as an earlier row; skipped.
    pub duplicates: Vec<String>,
    /// List entries no row names.
    pub untracked: Vec<String>,
}

/// With `whole_list`, both media's lists are read and their untracked
/// entries reported; without, only the media `rows` name are read.
pub fn plan(site: &mut dyn Site, rows: &[Row], whole_list: bool) -> Result<Plan> {
    let mut plan = Plan::default();
    for medium in [Medium::Anime, Medium::Manga] {
        let rows: Vec<(&Row, SiteRef)> = rows
            .iter()
            .filter_map(|row| Some((row, row.site_ref?)))
            .filter(|(_, site_ref)| site_ref.medium() == medium)
            .collect();
        if rows.is_empty() && !whole_list {
            continue;
        }
        let listed = site.list(medium)?;
        let by_ref: HashMap<SiteRef, usize> = listed
            .iter()
            .enumerate()
            .flat_map(|(at, listed)| listed.refs.iter().map(move |&site_ref| (site_ref, at)))
            .collect();
        let by_id: HashMap<u64, usize> = listed
            .iter()
            .enumerate()
            .map(|(at, listed)| (listed.id, at))
            .collect();
        let mut missing: Vec<SiteRef> = rows
            .iter()
            .map(|&(_, site_ref)| site_ref)
            .filter(|site_ref| !by_ref.contains_key(site_ref))
            .collect();
        missing.sort();
        missing.dedup();
        let found = match missing.is_empty() {
            true => HashMap::new(),
            false => site.find(&missing)?,
        };

        let mut matched = HashSet::new();
        for (row, site_ref) in rows {
            let on_list = by_ref
                .get(&site_ref)
                .or_else(|| found.get(&site_ref).and_then(|found| by_id.get(&found.id)));
            let (id, have, totals) = match (on_list, found.get(&site_ref)) {
                (Some(&at), _) => (listed[at].id, Some(&listed[at].entry), listed[at].totals),
                (None, Some(found)) => (found.id, None, found.totals),
                (None, None) => {
                    plan.unknown.push(row.title.clone());
                    continue;
                }
            };
            if !matched.insert(id) {
                plan.duplicates.push(row.title.clone());
                continue;
            }
            let score = row.score.map(|score| on_scale(score, site.points()));
            match diff(&want(&row.progress, medium, totals, score), have) {
                Some(patch) => plan.changes.push(Change {
                    entry: row.progress.entry.clone(),
                    site_ref,
                    medium,
                    id,
                    title: row.title.clone(),
                    have: have.cloned(),
                    patch,
                }),
                None => plan.in_sync += 1,
            }
        }
        if whole_list {
            plan.untracked.extend(
                listed
                    .iter()
                    .filter(|listed| !matched.contains(&listed.id))
                    .map(|listed| format!("{} ({})", listed.title, medium.as_str())),
            );
        }
    }
    Ok(plan)
}

/// Saves `row`'s entry without reading the whole list: the site's entry
/// is looked up and every field NRS knows is sent, as if it weren't on
/// the list yet (a save only sets the fields it names). `None` when the
/// row has no site ref or the site has no such entry.
pub fn push_one(site: &mut dyn Site, row: &Row) -> Result<Option<Change>> {
    let Some(site_ref) = row.site_ref else {
        return Ok(None);
    };
    let Some(&found) = site.find(&[site_ref])?.get(&site_ref) else {
        return Ok(None);
    };
    let medium = site_ref.medium();
    let score = row.score.map(|score| on_scale(score, site.points()));
    let want = want(&row.progress, medium, found.totals, score);
    let patch = diff(&want, None).expect("an entry not on the list always differs");
    site.save(medium, found.id, &patch)?;
    Ok(Some(Change {
        entry: row.progress.entry.clone(),
        site_ref,
        medium,
        id: found.id,
        title: row.title.clone(),
        have: None,
        patch,
    }))
}

#[cfg(test)]
mod tests {
    use nrs_core::EntryId;
    use nrs_store::{ProgressStatus, ProgressUnit};

    use super::*;
    use crate::lists::ListStatus;

    /// A MAL-like site: ids are MAL ids.
    #[derive(Default)]
    struct Fake {
        listed: Vec<Listed>,
        exists: Vec<(u64, Totals)>,
        lists_read: Vec<Medium>,
        saved: Vec<(u64, Patch)>,
    }

    impl Site for Fake {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn points(&self) -> u32 {
            10
        }

        fn list(&mut self, medium: Medium) -> Result<Vec<Listed>> {
            self.lists_read.push(medium);
            Ok(self
                .listed
                .iter()
                .filter(|listed| listed.refs[0].medium() == medium)
                .cloned()
                .collect())
        }

        fn find(&mut self, refs: &[SiteRef]) -> Result<HashMap<SiteRef, Found>> {
            Ok(refs
                .iter()
                .filter_map(|&site_ref| {
                    let SiteRef::Mal(_, id) = site_ref else { return None };
                    let &(_, totals) = self.exists.iter().find(|(known, _)| *known == id)?;
                    Some((site_ref, Found { id, totals }))
                })
                .collect())
        }

        fn save(&mut self, _: Medium, id: u64, patch: &Patch) -> Result<()> {
            self.saved.push((id, patch.clone()));
            Ok(())
        }
    }

    fn listed(id: u64, status: ListStatus, progress: u32) -> Listed {
        Listed {
            id,
            refs: vec![SiteRef::Mal(Medium::Anime, id)],
            title: format!("listed {id}"),
            entry: ListEntry {
                status,
                rewatching: false,
                progress: Some(progress),
                volumes: None,
                repeats: 0,
                started: None,
                finished: None,
                score: None,
            },
            totals: Totals { units: Some(12), volumes: None },
        }
    }

    fn row(id: Option<u64>, status: ProgressStatus, current: f32) -> Row {
        let mut progress = Progress::new(EntryId("e".into()), status, ProgressUnit::Episode);
        progress.current = current;
        Row {
            title: format!("row {id:?}"),
            site_ref: id.map(|id| SiteRef::Mal(Medium::Anime, id)),
            progress,
            score: None,
        }
    }

    #[test]
    fn rows_split_into_changes_in_sync_and_reports() {
        let mut site = Fake {
            listed: vec![
                listed(1, ListStatus::Current, 3),
                listed(2, ListStatus::Completed, 12),
                listed(9, ListStatus::Planned, 0),
            ],
            exists: vec![(3, Totals { units: Some(24), volumes: None })],
            ..Fake::default()
        };
        let rows = [
            row(Some(1), ProgressStatus::Completed, 12.0),
            row(Some(2), ProgressStatus::Completed, 12.0),
            row(Some(3), ProgressStatus::Completed, 24.0),
            row(Some(4), ProgressStatus::Active, 1.0),
            row(Some(1), ProgressStatus::Active, 1.0),
            row(None, ProgressStatus::Active, 1.0),
        ];
        let plan = plan(&mut site, &rows, true).unwrap();
        let changes: Vec<_> = plan
            .changes
            .iter()
            .map(|change| (change.id, change.have.is_some(), change.patch.progress))
            .collect();
        assert_eq!(changes, [(1, true, Some(12)), (3, false, Some(24))]);
        assert_eq!(plan.in_sync, 1);
        assert_eq!(plan.unknown, ["row Some(4)"]);
        assert_eq!(plan.duplicates, ["row Some(1)"]);
        assert_eq!(plan.untracked, ["listed 9 (anime)"]);
        assert_eq!(site.lists_read, [Medium::Anime, Medium::Manga]);
    }

    #[test]
    fn a_found_entry_already_on_the_list_is_updated_not_added() {
        // An AniList entry whose idMal the list didn't carry.
        let mut entry = listed(77, ListStatus::Current, 3);
        entry.refs = vec![SiteRef::AniList(Medium::Anime, 77)];
        let mut site = Fake {
            listed: vec![entry],
            ..Fake::default()
        };
        struct Resolving(Fake);
        impl Site for Resolving {
            fn name(&self) -> &'static str {
                "resolving"
            }
            fn points(&self) -> u32 {
                10
            }
            fn list(&mut self, medium: Medium) -> Result<Vec<Listed>> {
                self.0.list(medium)
            }
            fn find(&mut self, refs: &[SiteRef]) -> Result<HashMap<SiteRef, Found>> {
                Ok(refs
                    .iter()
                    .map(|&site_ref| (site_ref, Found { id: 77, totals: Totals::default() }))
                    .collect())
            }
            fn save(&mut self, medium: Medium, id: u64, patch: &Patch) -> Result<()> {
                self.0.save(medium, id, patch)
            }
        }
        let mut resolving = Resolving(std::mem::take(&mut site));
        let rows = [row(Some(5), ProgressStatus::Active, 4.0)];
        let plan = plan(&mut resolving, &rows, true).unwrap();
        assert_eq!(plan.changes.len(), 1);
        assert!(plan.changes[0].have.is_some());
        assert!(plan.untracked.is_empty());
    }

    #[test]
    fn push_one_finds_and_saves_without_reading_the_list() {
        let mut site = Fake {
            exists: vec![(3, Totals { units: Some(24), volumes: None })],
            ..Fake::default()
        };
        let pushed = push_one(&mut site, &row(Some(3), ProgressStatus::Completed, 24.0)).unwrap();
        assert_eq!(pushed.map(|change| change.id), Some(3));
        assert!(site.lists_read.is_empty());
        assert_eq!(site.saved[0].1.progress, Some(24));
        assert!(push_one(&mut site, &row(Some(4), ProgressStatus::Active, 1.0)).unwrap().is_none());
        assert!(push_one(&mut site, &row(None, ProgressStatus::Active, 1.0)).unwrap().is_none());
    }

    #[test]
    fn without_the_whole_list_only_named_media_are_read() {
        let mut site = Fake::default();
        let rows = [row(Some(1), ProgressStatus::Active, 1.0)];
        let plan = plan(&mut site, &rows, false).unwrap();
        assert_eq!(site.lists_read, [Medium::Anime]);
        assert_eq!(plan.unknown, ["row Some(1)"]);
    }
}
