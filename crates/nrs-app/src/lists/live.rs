//! Pushes progress rows to the connected sites as they're written (not
//! their scores; `job` sends those). Each site has a lane: a queue by
//! entry, the latest write winning, and a worker thread that pushes it
//! after a short pause, so a run of +1 clicks becomes one save of the last
//! count, saves for one entry can't land out of order, and one site's
//! full sync doesn't hold up the other's pushes. The last few outcomes are
//! kept for the `/lists` page.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;

use super::plan::{push_one, Row};
use super::{connect, connected, network, now, site_index, Describe, SITES};

/// How long a write waits for the next one to the same entry.
const SETTLE: Duration = Duration::from_millis(1500);
const KEPT: usize = 30;

struct Lane {
    queue: Mutex<BTreeMap<String, Row>>,
    wake: Condvar,
    worker: OnceLock<()>,
}

impl Lane {
    const fn new() -> Self {
        Self {
            queue: Mutex::new(BTreeMap::new()),
            wake: Condvar::new(),
            worker: OnceLock::new(),
        }
    }
}

static LANES: [Lane; SITES.len()] = [Lane::new(), Lane::new()];
static RECENT: Mutex<VecDeque<Pushed>> = Mutex::new(VecDeque::new());

#[derive(Debug, Clone, Serialize)]
pub struct Pushed {
    /// Unix seconds.
    pub at: i64,
    pub site: String,
    pub entry: String,
    pub title: String,
    pub ok: bool,
    /// What was saved, or why not.
    pub outcome: String,
}

/// Queues `row` for each connected site, replacing an older queued one
/// for its entry. Rows bound by no MAL/AniList URL are dropped.
pub fn enqueue(row: Row) {
    if row.site_ref.is_none() {
        return;
    }
    for site in connected() {
        let lane = &LANES[site_index(site)];
        lane.worker.get_or_init(|| {
            std::thread::Builder::new()
                .name(format!("list-push-{site}"))
                .spawn(move || run(site))
                .expect("spawn a list push worker");
        });
        if let Ok(mut queue) = lane.queue.lock() {
            queue.insert(row.progress.entry.0.clone(), row.clone());
            lane.wake.notify_one();
        }
    }
}

/// The latest outcomes, newest first.
pub fn recent() -> Vec<Pushed> {
    RECENT
        .lock()
        .map(|recent| recent.iter().rev().cloned().collect())
        .unwrap_or_default()
}

fn record(site: &str, row: &Row, ok: bool, outcome: String) {
    if !ok {
        eprintln!("list push to {site} failed for {}: {outcome}", row.title);
    }
    if let Ok(mut recent) = RECENT.lock() {
        recent.push_back(Pushed {
            at: now(),
            site: site.to_string(),
            entry: row.progress.entry.0.clone(),
            title: row.title.clone(),
            ok,
            outcome,
        });
        while recent.len() > KEPT {
            recent.pop_front();
        }
    }
}

fn run(site: &'static str) {
    let lane = &LANES[site_index(site)];
    loop {
        {
            let Ok(mut queue) = lane.queue.lock() else { return };
            while queue.is_empty() {
                queue = match lane.wake.wait(queue) {
                    Ok(queue) => queue,
                    Err(_) => return,
                };
            }
        }
        std::thread::sleep(SETTLE);
        let rows: Vec<Row> = match lane.queue.lock() {
            Ok(mut queue) => std::mem::take(&mut *queue).into_values().collect(),
            Err(_) => return,
        };
        let _network = network(site);
        let mut client = match connect(site) {
            Ok((client, _)) => client,
            Err(error) => {
                rows.iter().for_each(|row| record(site, row, false, error.to_string()));
                continue;
            }
        };
        let name = client.name();
        for row in &rows {
            match push_one(client.as_mut(), row) {
                Ok(Some(change)) => {
                    let describe = Describe {
                        medium: change.medium,
                        patch: &change.patch,
                        have: None,
                    };
                    record(name, row, true, describe.to_string());
                }
                Ok(None) => record(name, row, false, format!("{name} has no entry for it")),
                Err(error) => record(name, row, false, error.to_string()),
            }
        }
    }
}
