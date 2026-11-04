use crate::es::pool::Endpoint;
use serde_json::Value;

/// Extracts the endpoint from the params object: the URL (stored in the
/// `database` field) and, when `username` is set, the credentials to sign in
/// with. Tabularis fills `password` from the keychain.
pub fn extract_endpoint(params: &Value) -> Option<Endpoint> {
    let conn = params.get("params")?;
    let text = |key: &str| conn.get(key).and_then(Value::as_str).filter(|s| !s.is_empty());

    let url = match text("database") {
        Some(url) => url.to_string(),
        None => url_from_host(conn)?,
    };
    let auth = text("username")
        .map(|user| (user.to_string(), text("password").unwrap_or_default().to_string()));

    Some(Endpoint { url, auth })
}

/// A URL from the standard `host`/`port` fields, which Tabularis fills when
/// the URL field is left empty. A host that already has a scheme keeps it;
/// otherwise the scheme is `https` when `ssl_mode` asks for TLS, else `http`.
fn url_from_host(conn: &Value) -> Option<String> {
    let host = conn.get("host")?.as_str().filter(|h| !h.is_empty())?;
    let scheme = match conn.get("ssl_mode").and_then(Value::as_str) {
        Some("require" | "verify_ca" | "verify_full") => "https",
        _ => "http",
    };
    let base = if host.contains("://") {
        host.to_string()
    } else {
        format!("{scheme}://{host}")
    };

    Some(match conn.get("port").and_then(Value::as_u64) {
        Some(port) => format!("{}:{port}", base.trim_end_matches('/')),
        None => base,
    })
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

    /// The endpoint is the URL in `database` (or, when that is empty, one built
    /// from `host`/`port`) plus credentials only when a `username` is set; with
    /// neither a URL nor a host there is no endpoint.
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
            (
                "host with scheme and port",
                json!({"database": "", "host": "https://es.example.com", "port": 443, "username": "u", "password": "p"}),
                Some(("https://es.example.com:443", Some(("u", "p")))),
            ),
            (
                "bare host defaults to http",
                json!({"host": "es.example.com", "port": 9200}),
                Some(("http://es.example.com:9200", None)),
            ),
            (
                "bare host with ssl required",
                json!({"host": "es.example.com", "port": 9200, "ssl_mode": "require"}),
                Some(("https://es.example.com:9200", None)),
            ),
            (
                "host without port",
                json!({"host": "http://es.example.com"}),
                Some(("http://es.example.com", None)),
            ),
            (
                "url wins over host",
                json!({"database": url, "host": "other", "port": 1}),
                Some((url, None)),
            ),
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
