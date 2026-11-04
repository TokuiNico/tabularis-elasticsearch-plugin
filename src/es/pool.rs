use crate::error::PluginError;
use elasticsearch::auth::Credentials;
use elasticsearch::http::transport::{SingleNodeConnectionPool, Transport, TransportBuilder};
use elasticsearch::Elasticsearch;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::LazyLock,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use url::Url;

// Pool TTL: To evict stale connections after 30 minutes
const POOL_TTL: Duration = Duration::from_secs(30 * 60);

// A global connection pool for cached Elasticsearch transports.
// Always accessed via a plain mutex: every hit needs to write `last_used`,
// so a read/write split bought nothing but complexity.
static CONNECTION_POOLS: LazyLock<Mutex<HashMap<Endpoint, CachedTransport>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Where to connect and how to sign in. The URL carries no credentials, so
/// they stay out of logs and error messages that print it; a URL that still
/// holds `user:pass@` keeps working.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub url: String,
    /// `(username, password)`, sent as HTTP Basic credentials.
    pub auth: Option<(String, String)>,
}

// Hand-written so that a `{:?}` of an endpoint never prints the password.
impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("url", &self.url)
            .field("username", &self.auth.as_ref().map(|(u, _)| u))
            .finish()
    }
}

#[cfg(test)]
impl Endpoint {
    /// An endpoint that signs in through the URL, or not at all.
    pub fn from_url(url: &str) -> Self {
        Self {
            url: url.to_string(),
            auth: None,
        }
    }
}

/// `major.minor` of an Elasticsearch server.
pub type Version = (u32, u32);

/// Which `_sql` contract a server speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlFlavor {
    /// X-Pack SQL, built in from 6.3 on: the statement travels as
    /// `{"query": …}` and the reply carries `columns` and `rows`.
    XPack,
    /// The NLPchina `sql` plugin, which is what serves `_sql` on older
    /// clusters: the statement is the whole request body, index names take no
    /// quoting, and the reply is an ordinary search response.
    Nlpchina,
}

impl SqlFlavor {
    /// A version that could not be read is treated as current, which leaves
    /// any later failure to surface as an ordinary SQL error.
    pub fn for_version(version: Option<Version>) -> Self {
        match version {
            Some(v) if v < (6, 3) => SqlFlavor::Nlpchina,
            _ => SqlFlavor::XPack,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CachedTransport {
    transport: Transport,
    version: Version,
    last_used: Instant,
}

/// Read the server version, or `None` when the server cannot be reached or
/// answers with something unexpected.
async fn read_version(transport: &Transport) -> Option<Version> {
    let body: Value = Elasticsearch::new(transport.clone())
        .info()
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    let mut parts = body["version"]["number"].as_str()?.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;

    Some((major, minor))
}

/// The cached transport for `endpoint` and the server's version.
///
/// The version is read once per connection. The pool lock is not held while
/// reading it, so a slow or unreachable server does not stall requests to
/// other connections. A version that could not be read is not cached: the
/// next call probes again rather than keeping a guess for the whole TTL.
pub async fn get_transport(
    endpoint: &Endpoint,
) -> Result<(Transport, Option<Version>), PluginError> {
    if let Some(cached) = CONNECTION_POOLS.lock().await.get_mut(endpoint) {
        cached.last_used = Instant::now();
        return Ok((cached.transport.clone(), Some(cached.version)));
    }

    let u = Url::parse(&endpoint.url).map_err(|_| PluginError::invalid_params("invalid url"))?;

    let conn_pool = SingleNodeConnectionPool::new(u);

    // Cannot use Transport::single_node(url) because
    // we want to support URL with special characters
    // e.g. http://user:pass@123@localhost:9200
    let mut builder = TransportBuilder::new(conn_pool);
    if let Some((username, password)) = &endpoint.auth {
        builder = builder.auth(Credentials::Basic(username.clone(), password.clone()));
    }
    let transport = builder
        .build()
        .map_err(|_| PluginError::internal("failed to create transport"))?;

    let Some(version) = read_version(&transport).await else {
        return Ok((transport, None));
    };

    let cached = CONNECTION_POOLS
        .lock()
        .await
        .entry(endpoint.clone())
        .or_insert_with(|| CachedTransport {
            transport,
            version,
            last_used: Instant::now(),
        })
        .clone();

    Ok((cached.transport, Some(cached.version)))
}

pub async fn cleanup_pools() {
    let mut pools = CONNECTION_POOLS.lock().await;

    pools.retain(|_, pool| pool.last_used.elapsed() < POOL_TTL);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server whose version cannot be read is not cached, so the next call
    /// probes it again instead of keeping a guessed SQL contract for the
    /// whole idle TTL.
    #[test]
    fn unreadable_version_is_not_cached() {
        crate::test_support::block_on(async {
            let endpoint = Endpoint {
                url: "http://127.0.0.1:1".to_string(),
                auth: None,
            };

            let (_, version) = get_transport(&endpoint).await.expect("transport");

            assert_eq!(version, None);
            assert!(!CONNECTION_POOLS.lock().await.contains_key(&endpoint));
        })
    }

    /// Answer one HTTP request with an Elasticsearch 7 `GET /` body and hand
    /// back the request head it received.
    fn serve_once() -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut head = [0u8; 4096];
            let n = stream.read(&mut head).expect("read");
            let body = r#"{"version":{"number":"7.17.0"}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("write");
            String::from_utf8_lossy(&head[..n]).into_owned()
        });
        (url, handle)
    }

    /// A separate username and password go out as HTTP Basic credentials and
    /// stay out of the URL; without them no `Authorization` header is sent.
    /// Cases: a password holding `@`, and no credentials.
    #[test]
    fn credentials_are_sent_as_basic_auth() {
        let cases = [
            (
                "with credentials",
                Some(("elastic", "secret@123")),
                Some("authorization: Basic ZWxhc3RpYzpzZWNyZXRAMTIz"),
            ),
            ("without credentials", None, None),
        ];

        for (name, auth, want) in cases {
            crate::test_support::block_on(async {
                let (url, server) = serve_once();
                let endpoint = Endpoint {
                    url,
                    auth: auth.map(|(u, p)| (u.to_string(), p.to_string())),
                };

                let (_, version) = get_transport(&endpoint).await.expect("transport");
                let head = server.join().expect("server");

                assert_eq!(version, Some((7, 17)), "{name}");
                match want {
                    Some(header) => assert!(head.contains(header), "{name}: {head}"),
                    None => assert!(!head.to_ascii_lowercase().contains("authorization"), "{name}: {head}"),
                }
            })
        }
    }
}
