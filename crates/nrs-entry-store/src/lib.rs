//! Blocking client for the NRS Store Protocol v0.2.
//!
//! NRS persists only the opaque `(store, store_id)` returned by a type
//! store. Provider identifiers, aliases, metadata aggregation, and
//! deduplication remain private to that service.

use std::thread;
use std::time::{Duration, Instant};

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use url::Url;

const PROTOCOL_MAJOR: &str = "0";
const DEFAULT_JOB_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid entry-store URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("entry-store transport error: {0}")]
    Transport(String),
    #[error("entry store returned HTTP {status} ({code}): {message}")]
    Remote {
        status: u16,
        code: String,
        message: String,
    },
    #[error("entry-store protocol error: {0}")]
    Protocol(String),
    #[error("entry store does not support {0}")]
    Unsupported(&'static str),
    #[error("entry-store job failed ({code}): {message}")]
    JobFailed { code: String, message: String },
    #[error("entry-store job was canceled")]
    JobCanceled,
    #[error("entry-store job did not finish within {0:?}")]
    JobTimeout(Duration),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct EntryMeta {
    pub store_id: String,
    pub title: String,
    pub kind: String,
    pub cover_url: Option<String>,
    pub release_date: Option<String>,
    #[serde(default)]
    pub extra: Value,
}

/// A `lookup` match: the store ID a query already belongs to, plus
/// store-specific details of the match.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct LookupMatch {
    pub store_id: String,
    #[serde(default)]
    pub extra: Value,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct RelationSuggestion {
    pub kind: String,
    pub target_store_id: String,
    pub confidence: f32,
    pub evidence: String,
}

/// The language-independent boundary from `INFRA.md` §7.
pub trait EntryStore {
    fn resolve(&self, store_id: &str) -> Result<EntryMeta>;
    /// The entry `query` already belongs to, without fetching; `None` when
    /// the store knows no such entry.
    fn lookup(&self, query: &str) -> Result<Option<LookupMatch>>;
    fn ingest(&self, query: &str) -> Result<String>;
    fn suggest_relations(&self, store_id: &str) -> Result<Vec<RelationSuggestion>>;
}

#[derive(Clone)]
pub struct HttpEntryStore {
    base_url: Url,
    job_timeout: Duration,
    expected_store: Option<String>,
}

impl HttpEntryStore {
    pub fn new(base_url: &str) -> Result<Self> {
        let mut base_url = Url::parse(base_url)?;
        if base_url.cannot_be_a_base() || !matches!(base_url.scheme(), "http" | "https") {
            return Err(Error::Protocol(
                "entry-store URL must be an HTTP(S) base URL".into(),
            ));
        }
        base_url.set_query(None);
        base_url.set_fragment(None);
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }
        Ok(Self {
            base_url,
            job_timeout: DEFAULT_JOB_TIMEOUT,
            expected_store: None,
        })
    }

    pub fn with_job_timeout(mut self, timeout: Duration) -> Self {
        self.job_timeout = timeout;
        self
    }

    pub fn expecting_store(mut self, store: impl Into<String>) -> Self {
        self.expected_store = Some(store.into());
        self
    }

    pub fn capabilities(&self) -> Result<Capabilities> {
        let capabilities: Capabilities = self.get(&["nrs-store", "v1", "capabilities"])?;
        let major = capabilities
            .protocol_version
            .split('.')
            .next()
            .unwrap_or_default();
        if major != PROTOCOL_MAJOR {
            return Err(Error::Protocol(format!(
                "unsupported protocol version {}",
                capabilities.protocol_version
            )));
        }
        if let Some(expected) = &self.expected_store {
            if &capabilities.store != expected {
                return Err(Error::Protocol(format!(
                    "expected store {expected}, got {}",
                    capabilities.store
                )));
            }
        }
        Ok(capabilities)
    }

    fn ensure_operation(&self, operation: &'static str) -> Result<()> {
        let capabilities = self.capabilities()?;
        let supported = match operation {
            "resolve" => capabilities.operations.resolve,
            "lookup" => capabilities.operations.lookup,
            "ingest" => capabilities.operations.ingest,
            "suggest_relations" => capabilities.operations.suggest_relations,
            _ => false,
        };
        if supported {
            Ok(())
        } else {
            Err(Error::Unsupported(operation))
        }
    }

    fn endpoint(&self, segments: &[&str]) -> Result<Url> {
        let mut url = self.base_url.clone();
        let mut path = url
            .path_segments_mut()
            .map_err(|_| Error::Protocol("entry-store URL cannot contain path segments".into()))?;
        path.pop_if_empty();
        path.extend(segments);
        drop(path);
        Ok(url)
    }

    fn get<T: DeserializeOwned>(&self, segments: &[&str]) -> Result<T> {
        self.get_url(self.endpoint(segments)?)
    }

    fn get_url<T: DeserializeOwned>(&self, url: Url) -> Result<T> {
        decode_response(ureq::get(url.as_str()).timeout(REQUEST_TIMEOUT).call())
    }

    fn post<T: DeserializeOwned, B: Serialize>(&self, segments: &[&str], body: &B) -> Result<T> {
        let url = self.endpoint(segments)?;
        let body = serde_json::to_value(body)
            .map_err(|error| Error::Protocol(format!("could not encode request: {error}")))?;
        decode_response(
            ureq::post(url.as_str())
                .timeout(REQUEST_TIMEOUT)
                .send_json(body),
        )
    }
}

impl EntryStore for HttpEntryStore {
    fn resolve(&self, store_id: &str) -> Result<EntryMeta> {
        self.ensure_operation("resolve")?;
        self.get(&["nrs-store", "v1", "entries", store_id])
    }

    fn lookup(&self, query: &str) -> Result<Option<LookupMatch>> {
        self.ensure_operation("lookup")?;
        let mut url = self.endpoint(&["nrs-store", "v1", "lookup"])?;
        url.query_pairs_mut().append_pair("query", query);
        match self.get_url(url) {
            Ok(found) => Ok(Some(found)),
            Err(Error::Remote { status: 404, .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn ingest(&self, query: &str) -> Result<String> {
        self.ensure_operation("ingest")?;
        let created: JobCreated =
            self.post(&["nrs-store", "v1", "ingest"], &IngestRequest { query })?;
        if created.op != "ingest" {
            return Err(Error::Protocol(format!(
                "ingest created a {} job",
                created.op
            )));
        }

        let started = Instant::now();
        loop {
            let job: Job = self.get(&["nrs-store", "v1", "jobs", &created.job_id])?;
            match job.status.as_str() {
                "pending" | "running" | "canceling" => {}
                "succeeded" => {
                    return job
                        .result
                        .map(|result| result.store_id)
                        .ok_or_else(|| Error::Protocol("successful ingest had no result".into()));
                }
                "failed" => {
                    let error = job.error.ok_or_else(|| {
                        Error::Protocol("failed ingest had no error payload".into())
                    })?;
                    return Err(Error::JobFailed {
                        code: error.code,
                        message: error.message,
                    });
                }
                "canceled" => return Err(Error::JobCanceled),
                status => {
                    return Err(Error::Protocol(format!(
                        "job returned unknown status {status}"
                    )));
                }
            }
            if started.elapsed() >= self.job_timeout {
                return Err(Error::JobTimeout(self.job_timeout));
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn suggest_relations(&self, store_id: &str) -> Result<Vec<RelationSuggestion>> {
        self.ensure_operation("suggest_relations")?;
        let response: Suggestions = self.get(&[
            "nrs-store",
            "v1",
            "entries",
            store_id,
            "suggested-relations",
        ])?;
        Ok(response.suggestions)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Capabilities {
    pub store: String,
    pub protocol_version: String,
    pub operations: Operations,
    pub jobs: JobCapabilities,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Operations {
    pub resolve: bool,
    /// Added in 0.2; absent from older stores.
    #[serde(default)]
    pub lookup: bool,
    pub ingest: bool,
    pub suggest_relations: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JobCapabilities {
    pub streaming: bool,
}

#[derive(Serialize)]
struct IngestRequest<'a> {
    query: &'a str,
}

#[derive(Deserialize)]
struct JobCreated {
    job_id: String,
    op: String,
    #[allow(dead_code)]
    status: String,
}

#[derive(Deserialize)]
struct Job {
    status: String,
    result: Option<IngestResult>,
    error: Option<RemoteError>,
}

#[derive(Deserialize)]
struct IngestResult {
    store_id: String,
}

#[derive(Deserialize)]
struct Suggestions {
    suggestions: Vec<RelationSuggestion>,
}

#[derive(Deserialize)]
struct RemoteError {
    code: String,
    message: String,
}

fn decode_response<T: DeserializeOwned>(
    response: std::result::Result<ureq::Response, ureq::Error>,
) -> Result<T> {
    match response {
        Ok(response) => response
            .into_json()
            .map_err(|error| Error::Protocol(format!("invalid JSON response: {error}"))),
        Err(ureq::Error::Status(status, response)) => {
            let error = response.into_json::<RemoteError>().unwrap_or(RemoteError {
                code: "http_error".into(),
                message: "entry store returned a non-JSON error".into(),
            });
            Err(Error::Remote {
                status,
                code: error.code,
                message: error.message,
            })
        }
        Err(error) => Err(Error::Transport(error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn opaque_store_ids_are_path_encoded() {
        let client = HttpEntryStore::new("http://127.0.0.1:3738").unwrap();
        let url = client
            .endpoint(&["nrs-store", "v1", "entries", "id/with spaces"])
            .unwrap();
        assert_eq!(
            url.as_str(),
            "http://127.0.0.1:3738/nrs-store/v1/entries/id%2Fwith%20spaces"
        );
    }

    #[test]
    fn preserves_a_configured_base_path() {
        let client = HttpEntryStore::new("http://localhost:3738/services/anime").unwrap();
        let url = client
            .endpoint(&["nrs-store", "v1", "capabilities"])
            .unwrap();
        assert_eq!(
            url.as_str(),
            "http://localhost:3738/services/anime/nrs-store/v1/capabilities"
        );
    }

    #[test]
    fn follows_ingest_job_then_resolves_opaque_id() {
        let responses = [
            MockResponse::ok(
                "GET /nrs-store/v1/capabilities HTTP/1.1",
                r#"{"store":"anime","protocol_version":"0.1","operations":{"resolve":true,"ingest":true,"suggest_relations":true},"jobs":{"streaming":false}}"#,
            ),
            MockResponse::accepted(
                "POST /nrs-store/v1/ingest HTTP/1.1",
                r#"{"job_id":"job-1","op":"ingest","status":"pending"}"#,
            ),
            MockResponse::ok(
                "GET /nrs-store/v1/jobs/job-1 HTTP/1.1",
                r#"{"status":"succeeded","result":{"store_id":"opaque/id"},"error":null}"#,
            ),
            MockResponse::ok(
                "GET /nrs-store/v1/capabilities HTTP/1.1",
                r#"{"store":"anime","protocol_version":"0.1","operations":{"resolve":true,"ingest":true,"suggest_relations":true},"jobs":{"streaming":false}}"#,
            ),
            MockResponse::ok(
                "GET /nrs-store/v1/entries/opaque%2Fid HTTP/1.1",
                r#"{"store_id":"opaque/id","title":"Fixture Anime","kind":"anime","cover_url":null,"release_date":"2026","extra":{}}"#,
            ),
        ];
        let (base_url, server) = mock_server(responses);
        let client = HttpEntryStore::new(&base_url).unwrap();

        let store_id = client.ingest("https://myanimelist.net/anime/1").unwrap();
        assert_eq!(store_id, "opaque/id");
        let metadata = client.resolve(&store_id).unwrap();
        assert_eq!(metadata.title, "Fixture Anime");
        assert_eq!(metadata.store_id, store_id);

        server.join().unwrap();
    }

    #[test]
    fn lookup_maps_not_found_to_none() {
        let capabilities = r#"{"store":"musiclib","protocol_version":"0.2","operations":{"resolve":true,"lookup":true,"ingest":false,"suggest_relations":true},"jobs":{"streaming":false}}"#;
        let responses = [
            MockResponse::ok("GET /nrs-store/v1/capabilities HTTP/1.1", capabilities),
            MockResponse::ok(
                "GET /nrs-store/v1/lookup?query=https%3A%2F%2Fexample.com%2Fa%3Fb%3Dc HTTP/1.1",
                r#"{"store_id":"42","extra":{"source":"x"}}"#,
            ),
            MockResponse::ok("GET /nrs-store/v1/capabilities HTTP/1.1", capabilities),
            MockResponse {
                request_line: "GET /nrs-store/v1/lookup?query=unknown HTTP/1.1",
                status: "404 Not Found",
                body: r#"{"code":"not_found","message":"no such resource"}"#,
            },
        ];
        let (base_url, server) = mock_server(responses);
        let client = HttpEntryStore::new(&base_url).unwrap();

        let found = client.lookup("https://example.com/a?b=c").unwrap().unwrap();
        assert_eq!(found.store_id, "42");
        assert_eq!(found.extra["source"], "x");
        assert_eq!(client.lookup("unknown").unwrap(), None);

        server.join().unwrap();
    }

    #[test]
    fn lookup_is_unsupported_on_a_0_1_store() {
        let responses = [MockResponse::ok(
            "GET /nrs-store/v1/capabilities HTTP/1.1",
            r#"{"store":"anime","protocol_version":"0.1","operations":{"resolve":true,"ingest":true,"suggest_relations":false},"jobs":{"streaming":false}}"#,
        )];
        let (base_url, server) = mock_server(responses);
        let client = HttpEntryStore::new(&base_url).unwrap();

        assert!(matches!(
            client.lookup("x"),
            Err(Error::Unsupported("lookup"))
        ));

        server.join().unwrap();
    }

    struct MockResponse {
        request_line: &'static str,
        status: &'static str,
        body: &'static str,
    }

    impl MockResponse {
        fn ok(request_line: &'static str, body: &'static str) -> Self {
            Self {
                request_line,
                status: "200 OK",
                body,
            }
        }

        fn accepted(request_line: &'static str, body: &'static str) -> Self {
            Self {
                request_line,
                status: "202 Accepted",
                body,
            }
        }
    }

    fn mock_server<const N: usize>(
        responses: [MockResponse; N],
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                assert_eq!(request_line.trim_end(), response.request_line);

                let mut content_length = 0;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" {
                        break;
                    }
                    if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:")
                    {
                        content_length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; content_length];
                reader.read_exact(&mut body).unwrap();

                write!(
                    stream,
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.status,
                    response.body.len(),
                    response.body
                )
                .unwrap();
            }
        });
        (format!("http://{address}"), server)
    }
}
