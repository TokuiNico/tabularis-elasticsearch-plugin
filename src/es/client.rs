use super::pool;
use super::pool::{Endpoint, SqlFlavor, Version};
use crate::error::PluginError;
use crate::es::models::{EsqlResponse, Index, IndexMapping, SearchResponse, SqlResponse};
use crate::handlers::models::Query;
use crate::utils::identifiers;
use elasticsearch::http::headers::HeaderMap;
use elasticsearch::http::request::JsonBody;
use elasticsearch::http::Method;
use elasticsearch::{
    cat::CatIndicesParts, http::StatusCode, indices::IndicesGetMappingParts, Elasticsearch,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use url::Url;

#[derive(Debug)]
pub struct Client {
    es: Elasticsearch,
    version: Option<Version>,
}

impl Client {
    pub async fn connect(endpoint: &Endpoint) -> Result<Self, PluginError> {
        let (transport, version) = pool::get_transport(endpoint).await?;

        Ok(Self {
            es: Elasticsearch::new(transport),
            version,
        })
    }

    /// Which `_sql` contract this server speaks.
    pub fn sql_flavor(&self) -> SqlFlavor {
        SqlFlavor::for_version(self.version)
    }

    pub async fn ping(&self) -> Result<(), PluginError> {
        let response = self.es.ping().send().await?;

        if response.status_code() != StatusCode::OK {
            return Err(PluginError::internal(
                response.text().await.unwrap_or_default(),
            ));
        }

        Ok(())
    }

    pub async fn get_indices(&self) -> Result<Vec<Index>, PluginError> {
        let response = self
            .es
            .cat()
            .indices(CatIndicesParts::None)
            .format("json")
            .s(&["index"])
            .send()
            .await?;

        if response.status_code() != StatusCode::OK {
            return Err(PluginError::internal(
                response.text().await.unwrap_or_default(),
            ));
        }

        Ok(response.json().await?)
    }

    pub async fn get_mapping(
        &self,
        index: &str,
    ) -> Result<HashMap<String, IndexMapping>, PluginError> {
        let response = self
            .es
            .indices()
            .get_mapping(IndicesGetMappingParts::Index(&[index]))
            .send()
            .await?;

        if response.status_code() != StatusCode::OK {
            return Err(PluginError::internal(
                response.text().await.unwrap_or_default(),
            ));
        }

        Ok(response.json().await?)
    }

    /// Run an X-Pack SQL statement, fetching at most `limit` rows.
    ///
    /// When rows remain, the server keeps a cursor open for the next page.
    /// The plugin never asks for one, so it closes the cursor right away
    /// instead of leaving it to time out on the server.
    pub async fn execute_sql(
        self,
        query: Query,
        limit: Option<usize>,
    ) -> Result<SqlResponse, PluginError> {
        let resp = self
            .es
            .sql()
            .query()
            .body(sql_body(&query.body, self.version, limit))
            .send()
            .await?;
        if resp.status_code() != StatusCode::OK {
            return Err(PluginError::internal(resp.text().await.unwrap_or_default()));
        }

        let result: SqlResponse = resp.json().await?;

        if let Some(cursor) = &result.cursor {
            // Best effort: the rows are already here, and an unclosed cursor
            // still expires on the server.
            let _ = self
                .es
                .sql()
                .clear_cursor()
                .body(json!({ "cursor": cursor }))
                .send()
                .await;
        }

        Ok(result)
    }

    /// Run a statement through the NLPchina `sql` plugin.
    ///
    /// It reads the statement from the request body rather than from a JSON
    /// envelope, rejects a quoted index name, and answers with an ordinary
    /// search response. It also runs `DELETE`, so only reads are sent.
    pub async fn execute_nlpchina_sql(self, query: Query) -> Result<SearchResponse, PluginError> {
        if !is_read_statement(&query.body) {
            return Err(PluginError::invalid_params(
                "This driver is read-only: only SELECT, SHOW and DESCRIBE statements are run.",
            ));
        }

        let statement = identifiers::strip_identifier_quotes(&query.body);

        let resp = self
            .es
            .send::<String, ()>(
                Method::Post,
                "/_sql",
                HeaderMap::new(),
                None,
                Some(statement),
                None,
            )
            .await?;

        if resp.status_code() != StatusCode::OK {
            return Err(PluginError::internal(resp.text().await.unwrap_or_default()));
        }

        Ok(resp.json().await?)
    }

    pub async fn execute_esql(&self, query: Query) -> Result<EsqlResponse, PluginError> {
        let resp = self
            .es
            .esql()
            .query()
            .body(json!({"query": query.body}))
            .send()
            .await?;
        if resp.status_code() != StatusCode::OK {
            return Err(PluginError::internal(resp.text().await.unwrap_or_default()));
        }

        Ok(resp.json().await?)
    }

    /// Execute query in format REST
    ///
    /// Query body example:
    ///
    /// GET /user_index/_search
    /// {
    ///   "size": 1000,
    ///   "query": {
    ///     "multi_match": {
    ///       "query": "%GU%",
    ///       "fields": [
    ///         "name^1.0"
    ///       ]
    ///     }
    ///   },
    ///   "track_total_hits": -1
    /// }
    pub async fn search(self, query: Query) -> Result<SearchResponse, PluginError> {
        let (header, body) = query.body.split_once('\n').unwrap_or((&query.body, ""));

        let (method, path) = header
            .split_once(char::is_whitespace)
            .map(|(m, p)| (m.trim(), p.trim()))
            .ok_or_else(|| PluginError::invalid_params("Invalid request line"))?;

        let method = match method.to_ascii_uppercase().as_str() {
            "GET" => Method::Get,
            "POST" => Method::Post,
            other => {
                return Err(PluginError::invalid_params(format!(
                    "Unsupported HTTP method: {other}. Only GET and POST are supported."
                )))
            }
        };

        let url = Url::parse(&format!("http://localhost{path}")) // Assume path already have /
            .map_err(|e| PluginError::invalid_params(format!("Invalid path: {e}")))?;

        let path = url.path();

        if !(path == "/_search" || path.ends_with("/_search")) {
            return Err(PluginError::invalid_params(format!(
                "Unsupported path: {path}. Only '/_search' and '/<index>/_search' are supported."
            )));
        }

        let query_params: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

        let query_params = (!query_params.is_empty()).then_some(&query_params);

        let body = (!body.is_empty())
            .then_some(body)
            .map(|b| serde_json::from_str::<Value>(b))
            .transpose()
            .map_err(|err| PluginError::internal(err.to_string()))? // Option<Result<T, E>> -> Result<Option<T>, E>
            .map(JsonBody::from);

        let response = self
            .es
            .send(method, path, HeaderMap::new(), query_params, body, None)
            .await?;

        if response.status_code() != StatusCode::OK {
            return Err(PluginError::internal(format!(
                "Unexpected status code {} {}",
                response.status_code(),
                response.text().await.unwrap_or_default()
            )));
        }

        Ok(response.json().await?)
    }

    pub async fn translate_sql(&self, query: &str) -> Result<Value, PluginError> {
        let response = self
            .es
            .sql()
            .translate()
            .body(json!({ "query": query }))
            .send()
            .await?;

        if response.status_code() != StatusCode::OK {
            return Err(PluginError::internal(
                response.text().await.unwrap_or_default(),
            ));
        }

        Ok(response.json().await?)
    }
}

/// The X-Pack `_sql` request body. On servers that support it (6.7 on), a
/// multi-value field returns its first value instead of failing the whole
/// query with "Arrays (returned by [x]) are not supported". With a `limit`,
/// the first page holds at most that many rows.
fn sql_body(statement: &str, version: Option<Version>, limit: Option<usize>) -> Value {
    let mut body = json!({ "query": statement });

    if let Some(limit) = limit {
        body["fetch_size"] = json!(limit);
    }

    if version.is_none_or(|v| v >= (6, 7)) {
        body["field_multi_value_leniency"] = json!(true);
    }

    body
}

/// Whether a statement only reads, judged by its first word. Anything else,
/// including a statement that opens with a comment, counts as a write.
fn is_read_statement(statement: &str) -> bool {
    let first_word = statement
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();

    matches!(first_word.as_str(), "SELECT" | "SHOW" | "DESCRIBE" | "DESC")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::models::QueryMode;

    #[derive(Debug)]
    struct TestCase {
        q: Query,
    }

    /// Nothing listens here, so a request fails at connect time.
    const UNREACHABLE: &str = "http://127.0.0.1:1";

    /// The X-Pack SQL request asks the server to return the first value of a
    /// multi-value field instead of failing with "Arrays (returned by [x])
    /// are not supported", on servers that know the option (6.7 on) and on
    /// servers whose version could not be read (treated as current).
    /// Cases: 7.17, 6.7, 6.6, unknown.
    #[test]
    fn sql_body_sets_multi_value_leniency() {
        let cases = [
            ("7.17", Some((7, 17)), true),
            ("6.7", Some((6, 7)), true),
            ("6.6", Some((6, 6)), false),
            ("unknown", None, true),
        ];

        for (name, version, lenient) in cases {
            let body = sql_body("SELECT 1", version, None);
            assert_eq!(body["query"], "SELECT 1", "{name}");
            assert_eq!(
                body.get("field_multi_value_leniency").is_some(),
                lenient,
                "{name}"
            );
        }
    }

    /// With a row limit, the X-Pack SQL request asks for at most that many
    /// rows (`fetch_size`) instead of the server's default page of 1000.
    #[test]
    fn sql_body_sets_fetch_size_from_limit() {
        assert_eq!(
            sql_body("SELECT 1", Some((7, 17)), Some(100))["fetch_size"],
            100
        );
        assert!(sql_body("SELECT 1", Some((7, 17)), None)
            .get("fetch_size")
            .is_none());
    }

    /// When rows remain, `execute_sql` closes the cursor the server opened
    /// for them: asking for the next page with it fails. The `posts` index
    /// holds 3 documents.
    #[test]
    fn execute_sql_closes_cursor() {
        crate::test_support::block_on(async {
            let client = Client::connect(&Endpoint::from_url("http://elastic:secret@123@localhost:9200"))
                .await
                .expect("client");
            let q = Query {
                mode: QueryMode::Sql,
                body: "SELECT * FROM posts".to_string(),
            };

            let resp = client.execute_sql(q, Some(1)).await.expect("query");
            let cursor = resp.cursor.expect("cursor for the remaining rows");

            let client = Client::connect(&Endpoint::from_url("http://elastic:secret@123@localhost:9200"))
                .await
                .expect("client");
            let next_page = client
                .es
                .sql()
                .query()
                .body(json!({ "cursor": cursor }))
                .send()
                .await
                .expect("request");
            assert_ne!(next_page.status_code(), StatusCode::OK);
        })
    }

    /// The NLPchina `_sql` plugin also runs `DELETE`, so only statements that
    /// read are sent to it; the manifest promises a read-only driver.
    /// Cases: reads in mixed case and with leading whitespace, then writes
    /// and anything not recognisably a read.
    #[test]
    fn nlpchina_accepts_only_reads() {
        let cases = [
            ("SELECT * FROM idx", true),
            ("  select count(*) from idx", true),
            ("\nSHOW TABLES", true),
            ("DESCRIBE idx", true),
            ("DELETE FROM idx WHERE a = 1", false),
            ("delete from idx", false),
            ("UPDATE idx SET a = 1", false),
            ("-- note\nSELECT 1", false),
            ("", false),
        ];

        for (statement, read) in cases {
            assert_eq!(is_read_statement(statement), read, "{statement:?}");
        }
    }

    /// `#!rest` only searches, so it only sends the methods `_search` takes.
    /// A `PUT`/`DELETE`/`HEAD` is refused before anything goes on the wire.
    #[test]
    fn rest_refuses_methods_other_than_get_and_post() {
        crate::test_support::block_on(async {
            for method in ["PUT", "DELETE", "HEAD"] {
                let client = Client::connect(&Endpoint::from_url(UNREACHABLE)).await.expect("client");
                let q = Query {
                    mode: QueryMode::Rest,
                    body: format!("{method} /posts/_search\n{{}}"),
                };

                let err = client.search(q).await.expect_err(method);
                assert_eq!(err.code, crate::error::ErrorCode::InvalidParams, "{method}");
            }
        })
    }

    #[test]
    fn get_mapping_success() {
        crate::test_support::block_on(async {
            let client = Client::connect(&Endpoint::from_url("http://elastic:secret@123@localhost:9200")).await;
            assert!(client.is_ok(), "{:?}", client);

            let index_name = "posts";
            let resp = client.unwrap().get_mapping(index_name).await;
            assert!(resp.is_ok(), "{:?}", resp);
        })
    }

    #[test]
    fn execute_sql_success() {
        crate::test_support::block_on(async {
            let tcs = [TestCase {
                q: Query {
                    mode: QueryMode::Sql,
                    body: r"SELECT * FROM posts".to_string(),
                },
            }];

            for tc in tcs {
                let client = Client::connect(&Endpoint::from_url("http://elastic:secret@123@localhost:9200")).await;
                assert!(client.is_ok(), "{:?}", client);

                let resp = client.unwrap().execute_sql(tc.q, None).await;
                assert!(resp.is_ok(), "{:?}", resp);
            }
        })
    }

    #[test]
    fn execute_esql_success() {
        crate::test_support::block_on(async {
            let tcs = [
                TestCase {
                    q: Query {
                        mode: QueryMode::Esql,
                        body: "FROM posts | WHERE author LIKE \"*Erwin*\" | KEEP author, content"
                            .to_string(),
                    },
                },
                TestCase {
                    q: Query {
                        mode: QueryMode::Esql,
                        body:
                            "FROM posts \n| WHERE author LIKE \"*Erwin*\" \n| KEEP author, content"
                                .to_string(),
                    },
                },
            ];

            for tc in tcs {
                let client = Client::connect(&Endpoint::from_url("http://elastic:secret@123@localhost:9200")).await;
                assert!(client.is_ok(), "{:?}", client);

                let resp = client.unwrap().execute_esql(tc.q).await;
                assert!(resp.is_ok(), "{:?}", resp);
            }
        })
    }

    #[test]
    fn execute_search_success() {
        crate::test_support::block_on(async {
            let tcs = [
                TestCase {
                    q: Query {
                        mode: QueryMode::Rest,
                        body: "POST /posts/_search\n{\"_source\":[\"author\",\"content\"],\"query\":{\"wildcard\":{\"author\":{\"value\":\"*Erwin*\"}}}}"
                            .to_string(),
                    },
                },
            ];

            for tc in tcs {
                let client = Client::connect(&Endpoint::from_url("http://elastic:secret@123@localhost:9200")).await;
                assert!(client.is_ok(), "{:?}", client);

                let resp = client.unwrap().search(tc.q).await;
                assert!(resp.is_ok(), "{:?}", resp);
            }
        })
    }
}
