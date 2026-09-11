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
// Always accessed via a plain mutex: every access (hit or miss) needs to
// write `last_used`, so a read/write split bought nothing but complexity.
static CONNECTION_POOLS: LazyLock<Mutex<HashMap<String, CachedTransport>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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

#[derive(Debug, Clone)]
pub struct CachedTransport {
    transport: Transport,
    sql_flavor: SqlFlavor,
    last_used: Instant,
}

/// Read the server version once per connection and pick the `_sql` contract
/// from it. A version that cannot be read is treated as current, which leaves
/// any later failure to surface as an ordinary SQL error.
async fn detect_sql_flavor(transport: &Transport) -> SqlFlavor {
    async fn version(transport: &Transport) -> Option<(u32, u32)> {
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

    match version(transport).await {
        Some(v) if v < (6, 3) => SqlFlavor::Nlpchina,
        _ => SqlFlavor::XPack,
    }
}

pub async fn get_transport(url: &str) -> Result<(Transport, SqlFlavor), PluginError> {
    let mut pools = CONNECTION_POOLS.lock().await;

    if let Some(cached) = pools.get_mut(url) {
        cached.last_used = Instant::now();
        return Ok((cached.transport.clone(), cached.sql_flavor));
    }

    let u = Url::parse(url).map_err(|_| PluginError::invalid_params("invalid url"))?;

    let conn_pool = SingleNodeConnectionPool::new(u);

    // Cannot use Transport::single_node(url) because
    // we want to support URL with special characters
    // e.g. http://user:pass@123@localhost:9200
    let transport = TransportBuilder::new(conn_pool)
        .build()
        .map_err(|_| PluginError::internal("failed to create transport"))?;

    let sql_flavor = detect_sql_flavor(&transport).await;

    let cached = pools
        .entry(url.to_string())
        .or_insert_with(|| CachedTransport {
            transport,
            sql_flavor,
            last_used: Instant::now(),
        });

    Ok((cached.transport.clone(), cached.sql_flavor))
}

pub async fn cleanup_pools() {
    let mut pools = CONNECTION_POOLS.lock().await;

    pools.retain(|_, pool| pool.last_used.elapsed() < POOL_TTL);
}
