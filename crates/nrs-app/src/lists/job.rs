//! A full sync, progress and scores, to each connected site: what the
//! `/lists` page's "sync now" and a scheduled `POST /api/v1/lists/sync`
//! start. One runs at a time, each site in a thread of its own; `state`
//! reports on it while it runs and after.

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;

use super::plan::{plan, Row};
use super::{connect, network, now, rate_limited_until, Result};

#[derive(Debug, Clone, Serialize)]
pub struct JobState {
    pub running: bool,
    /// Unix seconds.
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub sites: Vec<SiteRun>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SiteRun {
    pub site: String,
    /// `waiting`, `reading list`, `saving`, `done` or `failed`.
    pub stage: &'static str,
    pub note: Option<String>,
    pub changes: usize,
    pub saved: usize,
    pub failed: usize,
    pub in_sync: usize,
    /// Rows the site has no entry for.
    pub unknown: usize,
    pub errors: Vec<String>,
    /// Unix seconds, while a 429 holds the site back.
    pub rate_limited_until: Option<i64>,
}

const KEPT_ERRORS: usize = 20;

static STATE: Mutex<JobState> = Mutex::new(JobState {
    running: false,
    started_at: None,
    finished_at: None,
    sites: Vec::new(),
});

fn lock() -> std::sync::MutexGuard<'static, JobState> {
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn state() -> JobState {
    let mut state = lock().clone();
    for run in &mut state.sites {
        run.rate_limited_until = rate_limited_until(&run.site);
    }
    state
}

/// Starts syncing `rows` to `sites`; `false` when a sync is already
/// running.
pub fn start(rows: Vec<Row>, sites: Vec<&'static str>) -> bool {
    {
        let mut state = lock();
        if state.running {
            return false;
        }
        *state = JobState {
            running: true,
            started_at: Some(now()),
            finished_at: None,
            sites: sites
                .iter()
                .map(|&site| SiteRun {
                    site: site.to_string(),
                    stage: "waiting",
                    note: None,
                    changes: 0,
                    saved: 0,
                    failed: 0,
                    in_sync: 0,
                    unknown: 0,
                    errors: Vec::new(),
                    rate_limited_until: None,
                })
                .collect(),
        };
    }
    std::thread::spawn(move || run(rows, sites));
    true
}

/// Blocks until no sync is running, then reports the last one.
pub fn wait() -> JobState {
    loop {
        let state = state();
        if !state.running {
            return state;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn update(at: usize, change: impl FnOnce(&mut SiteRun)) {
    if let Some(run) = lock().sites.get_mut(at) {
        change(run);
    }
}

fn run(rows: Vec<Row>, sites: Vec<&'static str>) {
    std::thread::scope(|scope| {
        for (at, site) in sites.into_iter().enumerate() {
            let rows = &rows;
            scope.spawn(move || {
                // Waits out a push to this site already under way.
                let _network = network(site);
                let result = sync_site(at, site, rows);
                update(at, |run| match result {
                    Ok(()) => run.stage = "done",
                    Err(error) => {
                        run.stage = "failed";
                        run.errors.push(error.to_string());
                    }
                });
            });
        }
    });
    let mut state = lock();
    state.running = false;
    state.finished_at = Some(now());
    for run in &state.sites {
        eprintln!(
            "list sync {}: {}; {} saved, {} failed, {} in sync, {} unknown",
            run.site, run.stage, run.saved, run.failed, run.in_sync, run.unknown
        );
    }
}

fn sync_site(at: usize, site: &str, rows: &[Row]) -> Result<()> {
    let (mut client, note) = connect(site)?;
    let name = client.name();
    update(at, |run| {
        run.site = name.to_string();
        run.note = note;
        run.stage = "reading list";
    });
    let plan = plan(client.as_mut(), rows, false)?;
    update(at, |run| {
        run.changes = plan.changes.len();
        run.in_sync = plan.in_sync;
        run.unknown = plan.unknown.len();
        run.stage = "saving";
    });
    for change in &plan.changes {
        let result = client.save(change.medium, change.id, &change.patch);
        update(at, |run| match result {
            Ok(()) => run.saved += 1,
            Err(error) => {
                run.failed += 1;
                if run.errors.len() < KEPT_ERRORS {
                    run.errors.push(format!("{}: {error}", change.title));
                }
            }
        });
    }
    Ok(())
}
