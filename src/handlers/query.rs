//! Connection and query execution.
//!
//! `test_connection` and `ping` return success unconditionally — this is
//! what lets the driver show up in the Tabularis connection picker right
//! after `just dev-install`. Replace with real checks before shipping.

use crate::error::PluginError;
use crate::es::client::Client;
use crate::es::pool::SqlFlavor;
use crate::handlers::models::{ExecuteQueryResponse, Query, QueryMode};
use crate::{
    error::ErrorCode,
    es,
    rpc::{error_response, ok_response},
    utils::extractor,
};
use serde_json::{json, Value};

pub async fn test_connection(id: Value, params: &Value) -> Value {
    let url = match extractor::extract_url(params) {
        Some(tb) if !tb.is_empty() => tb,
        _ => {
            return error_response(
                id,
                ErrorCode::InvalidParams,
                "url must be a non-empty string",
            )
        }
    };

    let client = match es::client::Client::from_url(&url).await {
        Ok(client) => client,
        Err(err) => {
            return error_response(id, err.code, &err.message);
        }
    };

    match client.ping().await {
        Ok(_) => ok_response(id, json!({"success": true})),
        Err(err) => error_response(id, err.code, &err.message),
    }
}

pub async fn ping(id: Value, params: &Value) -> Value {
    test_connection(id, params).await
}

pub async fn execute_query(id: Value, params: &Value) -> Value {
    let url = match extractor::extract_url(params) {
        Some(tb) if !tb.is_empty() => tb,
        _ => {
            return error_response(
                id,
                ErrorCode::InvalidParams,
                "url must be a non-empty string",
            )
        }
    };

    let query = match extractor::extract_query(params) {
        Some(payload) if !payload.is_empty() => Query::from(payload),
        _ => {
            return error_response(
                id,
                ErrorCode::InvalidParams,
                "query must be a non-empty string",
            )
        }
    };

    let limit = extractor::extract_limit(params);

    let client = match Client::from_url(&url).await {
        Ok(client) => client,
        Err(err) => {
            return error_response(id, err.code, &err.message);
        }
    };

    match QueryExecutor::new(query).execute(client, limit).await {
        Ok(result) => ok_response(id, json!(result.capped(limit))),
        Err(err) => error_response(id, err.code, &err.message),
    }
}

struct QueryExecutor {
    query: Query,
}

impl QueryExecutor {
    fn new(query: Query) -> Self {
        Self { query }
    }

    /// Run the query. Only X-Pack SQL can ask the server for fewer rows; the
    /// other modes return the server's own default amount (ES|QL 1000 rows,
    /// NLPchina 200 hits, a search its `size`, 10 by default), which the
    /// caller then cuts to `limit`.
    async fn execute(
        &self,
        client: Client,
        limit: Option<usize>,
    ) -> Result<ExecuteQueryResponse, PluginError> {
        match self.query.mode {
            QueryMode::Rest => {
                let resp = client.search(self.query.clone()).await;
                resp.map(ExecuteQueryResponse::from)
            }
            QueryMode::Esql => {
                let resp = client.execute_esql(self.query.clone()).await;
                resp.map(ExecuteQueryResponse::from)
            }
            QueryMode::None | QueryMode::Sql => match client.sql_flavor() {
                SqlFlavor::XPack => {
                    let resp = client.execute_sql(self.query.clone(), limit).await;
                    resp.map(ExecuteQueryResponse::from)
                }
                SqlFlavor::Nlpchina => {
                    let resp = client.execute_nlpchina_sql(self.query.clone()).await;
                    resp.map(ExecuteQueryResponse::from)
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `execute_query` returns at most `limit` rows and says the result was
    /// truncated, for every query mode. The `posts` index holds 3 documents.
    /// Cases: SQL, ES|QL, and `#!rest` with a `size` above the limit.
    #[test]
    fn execute_query_respects_limit() {
        crate::test_support::block_on(async {
            let cases = [
                ("sql", "SELECT * FROM posts"),
                ("esql", "#!esql\nFROM posts"),
                ("rest", "#!rest\nPOST /posts/_search\n{\"size\":3}"),
            ];

            for (name, query) in cases {
                let params = json!({
                    "params": { "database": "http://elastic:secret@123@localhost:9200" },
                    "query": query,
                    "limit": 1,
                });

                let resp = execute_query(json!(1), &params).await;

                assert_eq!(
                    resp["result"]["rows"].as_array().map(Vec::len),
                    Some(1),
                    "{name}: {resp}"
                );
                assert_eq!(resp["result"]["truncated"], true, "{name}");
            }
        })
    }

    #[test]
    fn ping_success() {
        crate::test_support::block_on(async {
            let params = json!({
                "params": {
                    "database": "http://elastic:secret@123@localhost:9200"
                },
                "query": ""
            });

            let result = ping(json!(1), &params).await;

            assert_eq!(result["id"], 1);
        })
    }
}
