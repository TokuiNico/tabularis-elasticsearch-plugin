use crate::es::pool::Endpoint;
use serde_json::Value;

/// Extracts the endpoint from the params object: the URL (stored in the
/// `database` field) and, when `username` is set, the credentials to sign in
/// with. Tabularis fills `password` from the keychain.
pub fn extract_endpoint(params: &Value) -> Option<Endpoint> {
    let conn = params.get("params")?;
    let text = |key: &str| conn.get(key).and_then(Value::as_str).filter(|s| !s.is_empty());

    let url = text("database")?.to_string();
    let auth = text("username")
        .map(|user| (user.to_string(), text("password").unwrap_or_default().to_string()));

    Some(Endpoint { url, auth })
}

/// Extracts the table from the params object
pub fn extract_table(params: &Value) -> Option<String> {
    params
        .get("table")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
}

/// Extracts the row cap Tabularis sends with `execute_query`. Zero counts as
/// no cap.
pub fn extract_limit(params: &Value) -> Option<usize> {
    params
        .get("limit")
        .and_then(Value::as_u64)
        .filter(|&n| n > 0)
        .map(|n| n as usize)
}

/// Extracts the "query" from the params object
pub fn extract_query(params: &Value) -> Option<String> {
    params
        .get("query")
        .and_then(|d| d.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::{extract_endpoint, extract_limit};
    use serde_json::json;

    /// The endpoint is the URL in `database` plus credentials only when a
    /// `username` is set; a missing or empty URL gives no endpoint.
    /// Cases: URL alone, URL with credentials, username without password,
    /// empty username, password without username, no URL, empty URL.
    #[test]
    fn endpoint_from_connection_params() {
        let url = "http://localhost:9200";
        let cases = [
            ("url only", json!({"database": url}), Some((url, None))),
            (
                "with credentials",
                json!({"database": url, "username": "u", "password": "p@1"}),
                Some((url, Some(("u", "p@1")))),
            ),
            (
                "username only",
                json!({"database": url, "username": "u", "password": null}),
                Some((url, Some(("u", "")))),
            ),
            (
                "empty username",
                json!({"database": url, "username": "", "password": "p"}),
                Some((url, None)),
            ),
            (
                "password only",
                json!({"database": url, "password": "p"}),
                Some((url, None)),
            ),
            ("no url", json!({"username": "u"}), None),
            ("empty url", json!({"database": ""}), None),
        ];

        for (name, conn, want) in cases {
            let got = extract_endpoint(&json!({ "params": conn }))
                .map(|e| (e.url, e.auth.map(|(u, p)| (u, p))));
            let want = want.map(|(u, a)| {
                (u.to_string(), a.map(|(x, y)| (x.to_string(), y.to_string())))
            });
            assert_eq!(got, want, "{name}");
        }
    }

    /// `limit` is the row cap Tabularis sends with `execute_query` (the MCP
    /// `run_query` limit, or the editor's page size). Zero, null and a
    /// missing value all mean no cap.
    #[test]
    fn limit_is_positive_or_absent() {
        let cases = [
            ("set", json!({"limit": 100}), Some(100)),
            ("zero", json!({"limit": 0}), None),
            ("null", json!({"limit": null}), None),
            ("missing", json!({}), None),
        ];

        for (name, params, want) in cases {
            assert_eq!(extract_limit(&params), want, "{name}");
        }
    }
}
