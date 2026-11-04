use crate::error::PluginError;
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
static CONNECTION_POOLS: LazyLock<Mutex<HashMap<String, CachedTransport>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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

/// The cached transport for `url` and the server's version.
///
/// The version is read once per connection. The pool lock is not held while
/// reading it, so a slow or unreachable server does not stall requests to
/// other connections. A version that could not be read is not cached: the
/// next call probes again rather than keeping a guess for the whole TTL.
pub async fn get_transport(url: &str) -> Result<(Transport, Option<Version>), PluginError> {
    if let Some(cached) = CONNECTION_POOLS.lock().await.get_mut(url) {
        cached.last_used = Instant::now();
        return Ok((cached.transport.clone(), Some(cached.version)));
    }

    let u = Url::parse(url).map_err(|_| PluginError::invalid_params("invalid url"))?;

    let conn_pool = SingleNodeConnectionPool::new(u);

    // Cannot use Transport::single_node(url) because
    // we want to support URL with special characters
    // e.g. http://user:pass@123@localhost:9200
    let transport = TransportBuilder::new(conn_pool)
        .build()
        .map_err(|_| PluginError::internal("failed to create transport"))?;

    let Some(version) = read_version(&transport).await else {
        return Ok((transport, None));
    };

    let cached = CONNECTION_POOLS
        .lock()
        .await
        .entry(url.to_string())
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
            let url = "http://127.0.0.1:1";

            let (_, version) = get_transport(url).await.expect("transport");

            assert_eq!(version, None);
            assert!(!CONNECTION_POOLS.lock().await.contains_key(url));
        })
    }
}
