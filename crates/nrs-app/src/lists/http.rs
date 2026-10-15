//! Requests to one site, paced by what the site says about its limit.
//!
//! Pacing is per site and shared by every client for it (a full sync,
//! live pushes, a preview), so they spend one budget together; each
//! request reserves the site's next slot.
//!
//! - AniList reports `X-RateLimit-Limit`/`-Remaining` on every response.
//!   Requests go back to back while half the budget is left, then spread
//!   what remains over the minute it refills in.
//! - MAL reports nothing, so its gap adapts: doubling on a 429, shrinking
//!   back a tenth per success down to its floor.
//!
//! A 429 pauses the whole site until its `Retry-After` (else AniList's
//! `X-RateLimit-Reset`, else an exponential backoff with jitter) passes,
//! then retries the request, for up to `PATIENCE` in all. A 5xx or a
//! transport error retries `RETRIES` times with backoff. Saves set
//! values, so a retried one can't apply twice.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{site} returned HTTP {status}: {body}")]
    Http {
        site: &'static str,
        status: u16,
        body: String,
    },
    #[error("{site}: {message}")]
    Transport { site: &'static str, message: String },
    #[error("{site} sent an unexpected response: {message}")]
    Protocol { site: &'static str, message: String },
    #[error("{0}")]
    Auth(String),
}

/// AniList's budget refills over a minute.
const WINDOW: Duration = Duration::from_secs(60);
/// Longest a request keeps waiting out 429s before it fails.
const PATIENCE: Duration = Duration::from_secs(15 * 60);
/// Retries after a 5xx or a transport error.
const RETRIES: u32 = 4;
const MAX_BACKOFF: Duration = Duration::from_secs(5 * 60);
/// `Retry-After` counts whole seconds; a little over avoids landing early.
const MARGIN: Duration = Duration::from_millis(500);

/// The narrowest and widest a site's gap between requests gets.
#[derive(Debug, Clone, Copy)]
struct Limits {
    floor: Duration,
    ceiling: Duration,
}

fn limits(site: &str) -> Limits {
    match site {
        // The headers pace AniList; the floor only softens bursts.
        "AniList" => Limits {
            floor: Duration::from_millis(250),
            ceiling: WINDOW,
        },
        // MAL publishes no limit; a couple a second is what it tolerates.
        "MAL" => Limits {
            floor: Duration::from_millis(500),
            ceiling: Duration::from_secs(30),
        },
        _ => Limits {
            floor: Duration::ZERO,
            ceiling: Duration::from_secs(30),
        },
    }
}

#[derive(Debug)]
struct Throttle {
    gap: Duration,
    /// The next free slot.
    next: Option<Instant>,
    /// Set by a 429: nothing goes out before this.
    blocked_until: Option<Instant>,
    /// 429s in a row.
    strikes: u32,
}

static THROTTLES: Mutex<BTreeMap<&'static str, Throttle>> = Mutex::new(BTreeMap::new());

fn with_throttle<T>(site: &'static str, change: impl FnOnce(&mut Throttle, Limits) -> T) -> T {
    let limits = limits(site);
    let mut throttles = THROTTLES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let throttle = throttles.entry(site).or_insert(Throttle {
        gap: limits.floor,
        next: None,
        blocked_until: None,
        strikes: 0,
    });
    change(throttle, limits)
}

fn now_unix() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// In `[0.8, 1.2)`, so retries from several clients don't line up.
fn jitter() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.subsec_nanos());
    0.8 + 0.4 * f64::from(nanos % 1000) / 1000.0
}

/// 2s, 4s, 8s, ... up to `MAX_BACKOFF`, times `jitter`.
fn backoff(attempt: u32, jitter: f64) -> Duration {
    Duration::from_secs(2)
        .saturating_mul(1 << attempt.min(10))
        .min(MAX_BACKOFF)
        .mul_f64(jitter)
}

/// The gap that spends what's left of AniList's budget: the floor while
/// at least half remains, then the rest spread evenly over a window.
fn spread(remaining: u32, limit: u32, limits: Limits) -> Duration {
    if remaining.saturating_mul(2) >= limit {
        return limits.floor;
    }
    (WINDOW / (remaining + 1)).clamp(limits.floor, limits.ceiling)
}

/// How long a 429 says to wait: `Retry-After` (seconds), else until
/// `X-RateLimit-Reset` (unix seconds), else a backoff by `strikes`.
fn wait_after_429(
    retry_after: Option<&str>,
    reset: Option<&str>,
    now_unix: i64,
    strikes: u32,
    jitter: f64,
) -> Duration {
    if let Some(seconds) = retry_after.and_then(|value| value.trim().parse::<u64>().ok()) {
        return Duration::from_secs(seconds) + MARGIN;
    }
    if let Some(reset) = reset.and_then(|value| value.trim().parse::<i64>().ok()) {
        if reset > now_unix {
            return Duration::from_secs((reset - now_unix) as u64) + MARGIN;
        }
    }
    backoff(strikes, jitter)
}

/// Waits for the site's next slot (and out any pause a 429 set since).
fn wait_turn(site: &'static str) {
    let start = with_throttle(site, |throttle, _| {
        let start = [Some(Instant::now()), throttle.next, throttle.blocked_until]
            .into_iter()
            .flatten()
            .max()
            .expect("now is always there");
        throttle.next = Some(start + throttle.gap);
        start
    });
    sleep(start.saturating_duration_since(Instant::now()));
    loop {
        let blocked = with_throttle(site, |throttle, _| throttle.blocked_until);
        match blocked.map(|until| until.saturating_duration_since(Instant::now())) {
            Some(wait) if !wait.is_zero() => sleep(wait),
            _ => return,
        }
    }
}

fn header_u32(response: &ureq::Response, name: &str) -> Option<u32> {
    response.header(name)?.trim().parse().ok()
}

fn succeeded(site: &'static str, response: &ureq::Response) {
    let remaining = header_u32(response, "X-RateLimit-Remaining");
    let limit = header_u32(response, "X-RateLimit-Limit");
    with_throttle(site, |throttle, limits| {
        throttle.strikes = 0;
        throttle.gap = match (remaining, limit) {
            (Some(remaining), Some(limit)) => spread(remaining, limit, limits),
            _ => throttle.gap.mul_f64(0.9).max(limits.floor),
        };
    });
}

/// Pauses the site for what `response` (a 429) asks; how long.
fn throttled(site: &'static str, response: &ureq::Response) -> Duration {
    with_throttle(site, |throttle, limits| {
        let wait = wait_after_429(
            response.header("Retry-After"),
            response.header("X-RateLimit-Reset"),
            now_unix(),
            throttle.strikes,
            jitter(),
        );
        throttle.strikes += 1;
        throttle.gap = throttle.gap.saturating_mul(2).clamp(limits.floor, limits.ceiling);
        let until = Instant::now() + wait;
        throttle.blocked_until = Some(throttle.blocked_until.map_or(until, |at| at.max(until)));
        wait
    })
}

/// When `site` (as a client names it, any case) may send again, in unix
/// seconds, while a 429 holds it back.
pub fn rate_limited_until(site: &str) -> Option<i64> {
    let throttles = THROTTLES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let (_, throttle) = throttles
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(site))?;
    let left = throttle.blocked_until?.checked_duration_since(Instant::now())?;
    Some(now_unix() + left.as_secs() as i64 + 1)
}

pub(crate) struct Paced {
    pub site: &'static str,
    agent: ureq::Agent,
}

impl Paced {
    pub fn new(site: &'static str) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .user_agent("nrs-app")
            .build();
        Self { site, agent }
    }

    /// Sends the request `send` makes, again on each retry.
    pub fn send(
        &mut self,
        send: impl Fn(&ureq::Agent) -> Result<ureq::Response, ureq::Error>,
    ) -> crate::lists::Result<ureq::Response> {
        let site = self.site;
        let started = Instant::now();
        let mut failures = 0;
        loop {
            wait_turn(site);
            let error = match send(&self.agent) {
                Ok(response) => {
                    succeeded(site, &response);
                    return Ok(response);
                }
                Err(error) => error,
            };
            match error {
                ureq::Error::Status(429, response) => {
                    let wait = throttled(site, &response);
                    if started.elapsed() + wait > PATIENCE {
                        return Err(Error::Http {
                            site,
                            status: 429,
                            body: format!(
                                "still rate limited after {} minutes; giving up",
                                PATIENCE.as_secs() / 60
                            ),
                        });
                    }
                    eprintln!("{site}: rate limited (429); pausing {}s", wait.as_secs_f32().ceil());
                }
                ureq::Error::Status(status, _) if status >= 500 && failures < RETRIES => {
                    let wait = backoff(failures, jitter());
                    failures += 1;
                    eprintln!("{site}: HTTP {status}; retrying in {}s", wait.as_secs_f32().ceil());
                    sleep(wait);
                }
                ureq::Error::Transport(transport) if failures < RETRIES => {
                    let wait = backoff(failures, jitter());
                    failures += 1;
                    eprintln!("{site}: {transport}; retrying in {}s", wait.as_secs_f32().ceil());
                    sleep(wait);
                }
                ureq::Error::Status(status, response) => {
                    let body = response.into_string().unwrap_or_default();
                    return Err(Error::Http {
                        site,
                        status,
                        body: body.chars().take(500).collect(),
                    });
                }
                ureq::Error::Transport(transport) => {
                    return Err(Error::Transport {
                        site,
                        message: transport.to_string(),
                    })
                }
            }
        }
    }

    pub fn json<T: DeserializeOwned>(
        &mut self,
        send: impl Fn(&ureq::Agent) -> Result<ureq::Response, ureq::Error>,
    ) -> crate::lists::Result<T> {
        let site = self.site;
        self.send(send)?
            .into_json()
            .map_err(|error| Error::Protocol {
                site,
                message: error.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    use super::*;

    /// A server answering one connection per response, in order.
    fn serve(responses: Vec<&'static str>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap() > 2 {
                    line.clear();
                }
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        url
    }

    const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}";

    #[derive(serde::Deserialize)]
    struct Ok_ {
        ok: bool,
    }

    #[test]
    fn a_429_waits_out_retry_after_and_retries() {
        let url = serve(vec![
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            OK,
        ]);
        let mut http = Paced::new("test-429");
        let started = Instant::now();
        let body: Ok_ = http.json(|agent| agent.get(&url).call()).unwrap();
        assert!(body.ok);
        assert!(started.elapsed() >= Duration::from_secs(1) + MARGIN);
        assert!(rate_limited_until("TEST-429").is_none(), "the pause is over");
    }

    #[test]
    fn other_client_errors_fail_at_once() {
        let url = serve(vec![
            "HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope",
        ]);
        let mut http = Paced::new("test-404");
        let started = Instant::now();
        match http.send(|agent| agent.get(&url).call()) {
            Err(Error::Http { status: 404, body, .. }) => assert_eq!(body, "nope"),
            other => panic!("expected a 404, got {other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_5xx_is_retried() {
        let url = serve(vec![
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            OK,
        ]);
        let mut http = Paced::new("test-503");
        let body: Ok_ = http.json(|agent| agent.get(&url).call()).unwrap();
        assert!(body.ok);
    }

    #[test]
    fn a_429_waits_for_retry_after_then_reset_then_backs_off() {
        let at = |seconds| Duration::from_secs(seconds) + MARGIN;
        assert_eq!(wait_after_429(Some("7"), Some("2000"), 1000, 0, 1.0), at(7));
        assert_eq!(wait_after_429(None, Some("1030"), 1000, 0, 1.0), at(30));
        assert_eq!(wait_after_429(Some("soon"), Some("900"), 1000, 2, 1.0), Duration::from_secs(8));
        assert_eq!(backoff(20, 1.0), MAX_BACKOFF);
    }

    #[test]
    fn anilist_spreads_the_last_half_of_its_budget() {
        let limits = limits("AniList");
        assert_eq!(spread(90, 90, limits), limits.floor);
        assert_eq!(spread(45, 90, limits), limits.floor);
        assert_eq!(spread(9, 90, limits), Duration::from_secs(6));
        assert_eq!(spread(0, 90, limits), WINDOW);
    }
}
